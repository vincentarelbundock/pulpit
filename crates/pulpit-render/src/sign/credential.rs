#![forbid(unsafe_code)]
//! PKCS#12 credential loading - deferred pending API clarification with p12-keystore/pkcs12 crates.
//! Credential structure and key analysis ready for S1.

use crate::sign::errors::SigningError;
use der::Decode;
use pkcs8::DecodePrivateKey;
use sha2::Digest;
use x509_cert::Certificate;
use zeroize::Zeroize;

pub struct Credential {
    pub signer_certificate: Certificate,
    pub cert_chain: Vec<Certificate>,
    key_material: ZeroizingKeyMaterial,
    pub public_key_info: PublicKeyInfo,
}

#[derive(Debug, Clone)]
pub enum PublicKeyInfo {
    Rsa { bits: usize },
    EcP256,
    EcP384,
    EcP521,
    Ed25519,
}

impl PublicKeyInfo {
    pub fn bits(&self) -> Option<usize> {
        match self {
            PublicKeyInfo::Rsa { bits } => Some(*bits),
            PublicKeyInfo::EcP256 => Some(256),
            PublicKeyInfo::EcP384 => Some(384),
            PublicKeyInfo::EcP521 => Some(521),
            PublicKeyInfo::Ed25519 => None,
        }
    }

    pub fn key_type_name(&self) -> &'static str {
        match self {
            PublicKeyInfo::Rsa { .. } => "RSA",
            PublicKeyInfo::EcP256 => "EC P-256",
            PublicKeyInfo::EcP384 => "EC P-384",
            PublicKeyInfo::EcP521 => "EC P-521",
            PublicKeyInfo::Ed25519 => "Ed25519",
        }
    }
}

struct ZeroizingKeyMaterial {
    _key_type: KeyType,
    data: Box<[u8]>,
}

impl ZeroizingKeyMaterial {
    fn new(key_type: KeyType, data: Vec<u8>) -> Self {
        let boxed = data.into_boxed_slice();
        ZeroizingKeyMaterial {
            _key_type: key_type,
            data: boxed,
        }
    }

    fn data(&self) -> &[u8] {
        &self.data
    }
}

impl Drop for ZeroizingKeyMaterial {
    fn drop(&mut self) {
        self.data.zeroize();
    }
}

#[derive(Debug, Clone, Copy)]
enum KeyType {
    Rsa,
    EcP256,
    EcP384,
    EcP521,
    Ed25519,
}

#[derive(Debug, Clone)]
pub struct CredentialSummary {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub sha256_fingerprint: String,
    pub key_algorithm: String,
    pub key_bits: Option<usize>,
}

pub fn load_pkcs12_impl(
    _pkcs12_data: &[u8],
    _passphrase: &str,
) -> Result<Credential, SigningError> {
    Err(SigningError::KeyLoadFailed(
        "PKCS#12 loading deferred - API exploration ongoing".to_string(),
    ))
}

pub fn from_parts(
    cert_der: &[u8],
    key_der: &[u8],
    chain: Vec<Vec<u8>>,
) -> Result<Credential, SigningError> {
    let signer_certificate = Certificate::from_der(cert_der)
        .map_err(|e| SigningError::InvalidCertificate(format!("Invalid cert: {}", e)))?;

    let mut cert_chain = Vec::new();
    for chain_cert_der in chain {
        match Certificate::from_der(&chain_cert_der) {
            Ok(c) => cert_chain.push(c),
            Err(_) => {}
        }
    }

    let (key_type, public_key_info) = analyze_private_key(key_der)?;

    Ok(Credential {
        signer_certificate,
        cert_chain,
        key_material: ZeroizingKeyMaterial::new(key_type, key_der.to_vec()),
        public_key_info,
    })
}

fn analyze_private_key(pkey_der: &[u8]) -> Result<(KeyType, PublicKeyInfo), SigningError> {
    let pki = pkcs8::PrivateKeyInfo::from_der(pkey_der)
        .map_err(|e| SigningError::InvalidCertificate(format!("Invalid key: {}", e)))?;

    let oid_str = pki.algorithm.oid.to_string();

    if oid_str == "1.2.840.113549.1.1.1" {
        let bits = extract_rsa_bits(pkey_der).unwrap_or(2048);
        return Ok((KeyType::Rsa, PublicKeyInfo::Rsa { bits }));
    }
    if oid_str == "1.2.840.10045.3.1.7" {
        return Ok((KeyType::EcP256, PublicKeyInfo::EcP256));
    }
    if oid_str == "1.3.132.1.12.0" {
        return Ok((KeyType::EcP384, PublicKeyInfo::EcP384));
    }
    if oid_str == "1.3.132.1.12.1" {
        return Ok((KeyType::EcP521, PublicKeyInfo::EcP521));
    }
    if oid_str == "1.3.101.112" {
        return Ok((KeyType::Ed25519, PublicKeyInfo::Ed25519));
    }

    Err(SigningError::UnsupportedKeyAlgorithm {
        algorithm: oid_str,
    })
}

fn extract_rsa_bits(pkey_der: &[u8]) -> Option<usize> {
    use rsa::traits::PublicKeyParts;
    let rsa = rsa::RsaPrivateKey::from_pkcs8_der(pkey_der).ok()?;
    Some(rsa.size() * 8)
}

impl Credential {
    pub(crate) fn private_key_der(&self) -> &[u8] {
        self.key_material.data()
    }

    pub fn summary(&self) -> Result<CredentialSummary, SigningError> {
        use x509_cert::der::Encode;

        let cert_der = self.signer_certificate.to_der()
            .map_err(|e| SigningError::DerEncodingFailed(e.to_string()))?;
        let fingerprint = {
            let mut h = sha2::Sha256::new();
            h.update(&cert_der);
            hex::encode(h.finalize())
        };

        Ok(CredentialSummary {
            subject: format!("{:?}", self.signer_certificate.tbs_certificate.subject),
            issuer: format!("{:?}", self.signer_certificate.tbs_certificate.issuer),
            serial: hex::encode(self.signer_certificate.tbs_certificate.serial_number.as_bytes()),
            not_before: self.signer_certificate.tbs_certificate.validity.not_before.to_string(),
            not_after: self.signer_certificate.tbs_certificate.validity.not_after.to_string(),
            sha256_fingerprint: fingerprint,
            key_algorithm: self.public_key_info.key_type_name().to_string(),
            key_bits: self.public_key_info.bits(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_key_info_bits() {
        assert_eq!(PublicKeyInfo::Rsa { bits: 2048 }.bits(), Some(2048));
        assert_eq!(PublicKeyInfo::EcP256.bits(), Some(256));
        assert_eq!(PublicKeyInfo::Ed25519.bits(), None);
    }
}
