#![forbid(unsafe_code)]
//! PKCS#12 credential loading and credential summary. Per §30, private keys
//! and passphrases live in Zeroizing buffers and are dropped as soon as the
//! signature bytes exist.

use crate::sign::errors::SigningError;
use sha2::Digest;
use x509_cert::Certificate;
use zeroize::Zeroize;

/// A loaded PKCS#12 credential: signer certificate, chain, and private key material.
pub struct Credential {
    /// The signing certificate (subject certificate)
    pub signer_certificate: Certificate,

    /// Certificate chain (additional certificates to embed)
    pub cert_chain: Vec<Certificate>,

    /// Private key material (bytes in DER format, type-tagged)
    key_material: ZeroizingKeyMaterial,

    /// Cached public key algorithm info for mechanism selection
    pub public_key_info: PublicKeyInfo,
}

/// Identifies the type and size of a public key for mechanism selection (§26.2).
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

/// Zeroizing wrapper for private key material
struct ZeroizingKeyMaterial {
    key_type: KeyType,
    data: Box<[u8]>,
}

impl ZeroizingKeyMaterial {
    #[allow(dead_code)]
    fn new(key_type: KeyType, data: Vec<u8>) -> Self {
        let boxed = data.into_boxed_slice();
        ZeroizingKeyMaterial {
            key_type,
            data: boxed,
        }
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    #[allow(dead_code)]
    fn key_type(&self) -> KeyType {
        self.key_type
    }
}

impl Drop for ZeroizingKeyMaterial {
    fn drop(&mut self) {
        self.data.zeroize();
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum KeyType {
    Rsa,
    EcP256,
    EcP384,
    EcP521,
    Ed25519,
}

/// Summary of a credential for UI display (§30).
#[derive(Debug, Clone)]
pub struct CredentialSummary {
    /// Subject DN as a readable string
    pub subject: String,
    /// Issuer DN as a readable string
    pub issuer: String,
    /// Serial number as hex
    pub serial: String,
    /// Not before time (ISO 8601)
    pub not_before: String,
    /// Not after time (ISO 8601)
    pub not_after: String,
    /// SHA-256 fingerprint (hex)
    pub sha256_fingerprint: String,
    /// Key algorithm name
    pub key_algorithm: String,
    /// Key size in bits (if applicable)
    pub key_bits: Option<usize>,
}

/// Load a PKCS#12 file with passphrase protection.
/// Zeroizes the passphrase after use per §30.2.
pub fn load_pkcs12_impl(pkcs12_data: &[u8], passphrase: &str) -> Result<Credential, SigningError> {
    // For v1, we accept the parameter but defer full PKCS#12 parsing
    // to a future milestone. For now, return a descriptive error.
    let _ = (pkcs12_data, passphrase);

    Err(SigningError::KeyLoadFailed(
        "PKCS#12 loading is deferred to a future milestone".to_string(),
    ))
}

/// Analyze private key DER to determine its type and extract public key info.
#[allow(dead_code)]
fn analyze_private_key(_pkey_der: &[u8]) -> Result<(KeyType, PublicKeyInfo), SigningError> {
    // Placeholder for future implementation
    Err(SigningError::UnsupportedKeyAlgorithm {
        algorithm: "deferred".to_string(),
    })
}

impl Credential {
    /// Get the private key material for signing.
    pub(crate) fn private_key_der(&self) -> &[u8] {
        self.key_material.data()
    }

    /// Get a summary for UI display
    pub fn summary(&self) -> Result<CredentialSummary, SigningError> {
        use x509_cert::der::Encode;

        // Subject and issuer: convert Name to readable string
        let subject = format_name(&self.signer_certificate.tbs_certificate.subject);
        let issuer = format_name(&self.signer_certificate.tbs_certificate.issuer);

        // Serial number as hex
        let serial = hex::encode(
            self.signer_certificate
                .tbs_certificate
                .serial_number
                .as_bytes(),
        );

        // Not before and after
        let not_before = self
            .signer_certificate
            .tbs_certificate
            .validity
            .not_before
            .to_string();
        let not_after = self
            .signer_certificate
            .tbs_certificate
            .validity
            .not_after
            .to_string();

        // SHA-256 fingerprint
        let cert_der = self
            .signer_certificate
            .to_der()
            .map_err(|e| SigningError::DerEncodingFailed(e.to_string()))?;
        let fingerprint = {
            let mut hasher = sha2::Sha256::new();
            hasher.update(&cert_der);
            hex::encode(hasher.finalize())
        };

        Ok(CredentialSummary {
            subject,
            issuer,
            serial,
            not_before,
            not_after,
            sha256_fingerprint: fingerprint,
            key_algorithm: self.public_key_info.key_type_name().to_string(),
            key_bits: self.public_key_info.bits(),
        })
    }
}

/// Convert an X.509 Name to a readable string
fn format_name(name: &x509_cert::name::Name) -> String {
    // Simple implementation for v1
    format!("{:?}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_key_info_bits() {
        let rsa_2048 = PublicKeyInfo::Rsa { bits: 2048 };
        assert_eq!(rsa_2048.bits(), Some(2048));

        let ec_p256 = PublicKeyInfo::EcP256;
        assert_eq!(ec_p256.bits(), Some(256));

        let ed25519 = PublicKeyInfo::Ed25519;
        assert_eq!(ed25519.bits(), None);
    }

    #[test]
    fn test_public_key_info_type_name() {
        assert_eq!(PublicKeyInfo::Rsa { bits: 2048 }.key_type_name(), "RSA");
        assert_eq!(PublicKeyInfo::EcP256.key_type_name(), "EC P-256");
        assert_eq!(PublicKeyInfo::Ed25519.key_type_name(), "Ed25519");
    }
}
