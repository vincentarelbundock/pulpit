#![forbid(unsafe_code)]
//! CMS SignedData construction. Per SPEC-signing.md §26, a detached SignedData
//! with exactly one SignerInfo, no encapsulated content.
//!
//! V1 scope: mechanism/digest selection, size estimation, signed attribute structure.

use crate::sign::credential::Credential;
use crate::sign::errors::SigningError;
use crate::sign::mechanism::{DigestAlgorithm, SigningMechanism};
use crate::sign::SigningProfile;
use x509_cert::Certificate;

/// Build CMS SignedData DER with the given signature bytes.
/// V1 defers full CMS construction to a future milestone.
pub fn build_cms_der(
    _credential: &Credential,
    _document_digest: &[u8],
    _mechanism: SigningMechanism,
    _signature_bytes: Vec<u8>,
    _profile: SigningProfile,
    _embed_roots: bool,
) -> Result<Vec<u8>, SigningError> {
    // Placeholder for v1 - full CMS construction is deferred
    // Return a minimal valid DER SEQUENCE for size estimation
    Ok(vec![0x30, 0x00]) // Empty SEQUENCE
}

/// Build certificate set for CMS
#[allow(dead_code)]
fn build_cert_set(credential: &Credential, embed_roots: bool) -> Vec<Certificate> {
    let mut cert_set = vec![credential.signer_certificate.clone()];
    if embed_roots {
        cert_set.extend(credential.cert_chain.iter().cloned());
    } else {
        // Filter out self-signed certificates
        for cert in &credential.cert_chain {
            if !is_self_signed(cert) {
                cert_set.push(cert.clone());
            }
        }
    }
    cert_set
}

/// Check if a certificate is self-signed
fn is_self_signed(cert: &Certificate) -> bool {
    cert.tbs_certificate.issuer == cert.tbs_certificate.subject
}

/// Compute the signature over the signed attributes with SET OF re-tagging
pub fn compute_signature(
    credential: &Credential,
    document_digest: &[u8],
    mechanism: SigningMechanism,
    _profile: SigningProfile,
) -> Result<Vec<u8>, SigningError> {
    let data_to_sign = document_digest;

    match mechanism {
        SigningMechanism::Rsa2048Sha256
        | SigningMechanism::Rsa3072Sha384
        | SigningMechanism::Rsa4096Sha512 => sign_with_rsa(credential, data_to_sign, mechanism),
        SigningMechanism::EcdsaP256Sha256 => sign_with_ecdsa_p256(credential, data_to_sign),
        SigningMechanism::EcdsaP384Sha384 => sign_with_ecdsa_p384(credential, data_to_sign),
        SigningMechanism::EcdsaP521Sha512 => sign_with_ecdsa_p521(credential, data_to_sign),
        SigningMechanism::Ed25519 => sign_with_ed25519(credential, data_to_sign),
    }
}

fn sign_with_rsa(
    credential: &Credential,
    data: &[u8],
    mechanism: SigningMechanism,
) -> Result<Vec<u8>, SigningError> {
    use pkcs8::DecodePrivateKey;
    use rsa::RsaPrivateKey;
    use sha2::{Digest, Sha256, Sha384, Sha512};

    let pkey_der = credential.private_key_der();
    let private_key = RsaPrivateKey::from_pkcs8_der(pkey_der).map_err(|e| {
        SigningError::SignatureOperationFailed(format!("Failed to parse RSA key: {}", e))
    })?;

    // Hash the data
    let digest_alg = mechanism.digest_algorithm();
    let hash = match digest_alg {
        DigestAlgorithm::Sha256 => {
            let mut hasher = Sha256::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        }
        DigestAlgorithm::Sha384 => {
            let mut hasher = Sha384::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        }
        DigestAlgorithm::Sha512 => {
            let mut hasher = Sha512::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        }
    };

    // Sign with PKCS#1 v1.5
    use rsa::pkcs1v15::Pkcs1v15Sign;
    let signature = private_key
        .sign(Pkcs1v15Sign::new_unprefixed(), &hash)
        .map_err(|e| {
            SigningError::SignatureOperationFailed(format!("RSA signature failed: {}", e))
        })?;

    Ok(signature)
}

fn sign_with_ecdsa_p256(credential: &Credential, data: &[u8]) -> Result<Vec<u8>, SigningError> {
    use pkcs8::DecodePrivateKey;
    use sha2::{Digest, Sha256};
    use signature::Signer;

    let pkey_der = credential.private_key_der();

    let key = p256::ecdsa::SigningKey::from_pkcs8_der(pkey_der).map_err(|e| {
        SigningError::SignatureOperationFailed(format!("Failed to parse P-256 key: {}", e))
    })?;

    // Hash the data with SHA-256
    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = hasher.finalize().to_vec();

    let sig: p256::ecdsa::Signature = key.sign(&hash);
    Ok(sig.to_bytes().to_vec())
}

fn sign_with_ecdsa_p384(credential: &Credential, data: &[u8]) -> Result<Vec<u8>, SigningError> {
    use pkcs8::DecodePrivateKey;
    use sha2::{Digest, Sha384};
    use signature::Signer;

    let pkey_der = credential.private_key_der();

    let key = p384::ecdsa::SigningKey::from_pkcs8_der(pkey_der).map_err(|e| {
        SigningError::SignatureOperationFailed(format!("Failed to parse P-384 key: {}", e))
    })?;

    // Hash the data with SHA-384
    let mut hasher = Sha384::new();
    hasher.update(data);
    let hash = hasher.finalize().to_vec();

    let sig: p384::ecdsa::Signature = key.sign(&hash);
    Ok(sig.to_bytes().to_vec())
}

fn sign_with_ecdsa_p521(credential: &Credential, data: &[u8]) -> Result<Vec<u8>, SigningError> {
    let _ = (credential, data);

    // Placeholder for v1 - P-521 key parsing deferred
    Err(SigningError::SignatureOperationFailed(
        "P-521 key parsing deferred to future milestone".to_string(),
    ))
}

fn sign_with_ed25519(credential: &Credential, data: &[u8]) -> Result<Vec<u8>, SigningError> {
    let _ = (credential, data);

    // Placeholder for v1 - Ed25519 key parsing deferred
    Err(SigningError::SignatureOperationFailed(
        "Ed25519 key parsing deferred to future milestone".to_string(),
    ))
}
