#![forbid(unsafe_code)]
//! Mechanism and digest selection per SPEC-signing.md §26.2.
//! When the user has not chosen explicitly, derive the mechanism from the signing
//! certificate's public key.

use crate::sign::credential::PublicKeyInfo;
use crate::sign::errors::SigningError;

/// Supported digest algorithms (§26.2)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgorithm {
    /// SHA-256 (for RSA ≤2048, EC ≤256)
    Sha256,
    /// SHA-384 (for RSA ≤3072, EC ≤384)
    Sha384,
    /// SHA-512 (for RSA >3072, EC >384, Ed25519)
    Sha512,
}

impl DigestAlgorithm {
    pub fn oid(&self) -> &'static str {
        match self {
            DigestAlgorithm::Sha256 => "2.16.840.1.101.3.4.2.1", // id-sha256
            DigestAlgorithm::Sha384 => "2.16.840.1.101.3.4.2.2", // id-sha384
            DigestAlgorithm::Sha512 => "2.16.840.1.101.3.4.2.3", // id-sha512
        }
    }

    pub fn hash_len(&self) -> usize {
        match self {
            DigestAlgorithm::Sha256 => 32,
            DigestAlgorithm::Sha384 => 48,
            DigestAlgorithm::Sha512 => 64,
        }
    }

    /// Select digest for RSA key size per §26.2 table
    pub fn select_for_rsa_bits(bits: usize) -> Result<DigestAlgorithm, SigningError> {
        if bits <= 2048 {
            Ok(DigestAlgorithm::Sha256)
        } else if bits <= 3072 {
            Ok(DigestAlgorithm::Sha384)
        } else {
            Ok(DigestAlgorithm::Sha512)
        }
    }

    /// Select digest for EC key size per §26.2 table
    pub fn select_for_ec_bits(bits: usize) -> Result<DigestAlgorithm, SigningError> {
        if bits <= 256 {
            Ok(DigestAlgorithm::Sha256)
        } else if bits <= 384 {
            Ok(DigestAlgorithm::Sha384)
        } else {
            Ok(DigestAlgorithm::Sha512)
        }
    }
}

/// Signing mechanism that determines algorithm OIDs and signature format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningMechanism {
    /// PKCS#1 v1.5 with SHA-256
    Rsa2048Sha256,
    /// PKCS#1 v1.5 with SHA-384
    Rsa3072Sha384,
    /// PKCS#1 v1.5 with SHA-512
    Rsa4096Sha512,
    /// ECDSA with P-256 and SHA-256
    EcdsaP256Sha256,
    /// ECDSA with P-384 and SHA-384
    EcdsaP384Sha384,
    /// ECDSA with P-521 and SHA-512
    EcdsaP521Sha512,
    /// Ed25519 with SHA-512 (fixed)
    Ed25519,
}

impl SigningMechanism {
    pub fn digest_algorithm(&self) -> DigestAlgorithm {
        match self {
            SigningMechanism::Rsa2048Sha256 | SigningMechanism::EcdsaP256Sha256 => {
                DigestAlgorithm::Sha256
            }
            SigningMechanism::Rsa3072Sha384 | SigningMechanism::EcdsaP384Sha384 => {
                DigestAlgorithm::Sha384
            }
            SigningMechanism::Rsa4096Sha512 | SigningMechanism::EcdsaP521Sha512 => {
                DigestAlgorithm::Sha512
            }
            SigningMechanism::Ed25519 => DigestAlgorithm::Sha512,
        }
    }

    pub fn signature_algorithm_oid(&self) -> &'static str {
        match self {
            SigningMechanism::Rsa2048Sha256 => "1.2.840.113549.1.1.11", // sha256WithRSAEncryption
            SigningMechanism::Rsa3072Sha384 => "1.2.840.113549.1.1.12", // sha384WithRSAEncryption
            SigningMechanism::Rsa4096Sha512 => "1.2.840.113549.1.1.13", // sha512WithRSAEncryption
            SigningMechanism::EcdsaP256Sha256 => "1.2.840.10045.4.3.2", // ecdsa-with-SHA256
            SigningMechanism::EcdsaP384Sha384 => "1.2.840.10045.4.3.3", // ecdsa-with-SHA384
            SigningMechanism::EcdsaP521Sha512 => "1.2.840.10045.4.3.4", // ecdsa-with-SHA512
            SigningMechanism::Ed25519 => "1.3.101.112",                 // id-Ed25519
        }
    }

    /// Expected signature length in bytes
    pub fn signature_length(&self) -> usize {
        match self {
            SigningMechanism::Rsa2048Sha256 => 256,   // 2048 bits
            SigningMechanism::Rsa3072Sha384 => 384,   // 3072 bits
            SigningMechanism::Rsa4096Sha512 => 512,   // 4096 bits
            SigningMechanism::EcdsaP256Sha256 => 72,  // ~72 bytes for DER-encoded
            SigningMechanism::EcdsaP384Sha384 => 104, // ~104 bytes for DER-encoded
            SigningMechanism::EcdsaP521Sha512 => 140, // ~140 bytes for DER-encoded
            SigningMechanism::Ed25519 => 64,          // 512 bits
        }
    }
}

/// Select mechanism from certificate's public key, with optional explicit digest override.
/// Per §26.2, if the mechanism implies a specific digest (Ed25519), an override that
/// disagrees causes DigestMechanismMismatch error.
pub fn select_mechanism(
    pub_key_info: &PublicKeyInfo,
    requested_digest: Option<DigestAlgorithm>,
) -> Result<SigningMechanism, SigningError> {
    match pub_key_info {
        PublicKeyInfo::Rsa { bits } => {
            let digest = requested_digest
                .or_else(|| DigestAlgorithm::select_for_rsa_bits(*bits).ok())
                .ok_or_else(|| SigningError::UnsupportedKeyAlgorithm {
                    algorithm: format!("RSA {} bits", bits),
                })?;

            match digest {
                DigestAlgorithm::Sha256 => Ok(SigningMechanism::Rsa2048Sha256),
                DigestAlgorithm::Sha384 => Ok(SigningMechanism::Rsa3072Sha384),
                DigestAlgorithm::Sha512 => Ok(SigningMechanism::Rsa4096Sha512),
            }
        }
        PublicKeyInfo::EcP256 => {
            let digest = requested_digest.unwrap_or(DigestAlgorithm::Sha256);

            if digest != DigestAlgorithm::Sha256 {
                return Err(SigningError::DigestMechanismMismatch {
                    implied: "sha256".to_string(),
                    requested: format!("{:?}", digest).to_lowercase(),
                });
            }
            Ok(SigningMechanism::EcdsaP256Sha256)
        }
        PublicKeyInfo::EcP384 => {
            let digest = requested_digest.unwrap_or(DigestAlgorithm::Sha384);

            if digest != DigestAlgorithm::Sha384 {
                return Err(SigningError::DigestMechanismMismatch {
                    implied: "sha384".to_string(),
                    requested: format!("{:?}", digest).to_lowercase(),
                });
            }
            Ok(SigningMechanism::EcdsaP384Sha384)
        }
        PublicKeyInfo::EcP521 => {
            let digest = requested_digest.unwrap_or(DigestAlgorithm::Sha512);

            if digest != DigestAlgorithm::Sha512 {
                return Err(SigningError::DigestMechanismMismatch {
                    implied: "sha512".to_string(),
                    requested: format!("{:?}", digest).to_lowercase(),
                });
            }
            Ok(SigningMechanism::EcdsaP521Sha512)
        }
        PublicKeyInfo::Ed25519 => {
            // Ed25519 always uses SHA-512
            if let Some(requested) = requested_digest {
                if requested != DigestAlgorithm::Sha512 {
                    return Err(SigningError::DigestMechanismMismatch {
                        implied: "sha512".to_string(),
                        requested: format!("{:?}", requested).to_lowercase(),
                    });
                }
            }
            Ok(SigningMechanism::Ed25519)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rsa_2048_default_digest() {
        let digest = DigestAlgorithm::select_for_rsa_bits(2048).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha256);
    }

    #[test]
    fn test_rsa_3072_default_digest() {
        let digest = DigestAlgorithm::select_for_rsa_bits(3072).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha384);
    }

    #[test]
    fn test_rsa_4096_default_digest() {
        let digest = DigestAlgorithm::select_for_rsa_bits(4096).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha512);
    }

    #[test]
    fn test_ec_p256_default_digest() {
        let digest = DigestAlgorithm::select_for_ec_bits(256).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha256);
    }

    #[test]
    fn test_ec_p384_default_digest() {
        let digest = DigestAlgorithm::select_for_ec_bits(384).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha384);
    }

    #[test]
    fn test_ec_p521_default_digest() {
        let digest = DigestAlgorithm::select_for_ec_bits(521).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha512);
    }

    #[test]
    fn test_mechanism_rsa_2048() {
        let key_info = PublicKeyInfo::Rsa { bits: 2048 };
        let mech = select_mechanism(&key_info, None).unwrap();
        assert_eq!(mech, SigningMechanism::Rsa2048Sha256);
    }

    #[test]
    fn test_mechanism_ed25519_sha512_required() {
        let key_info = PublicKeyInfo::Ed25519;
        let mech = select_mechanism(&key_info, Some(DigestAlgorithm::Sha512)).unwrap();
        assert_eq!(mech, SigningMechanism::Ed25519);
    }

    #[test]
    fn test_mechanism_ed25519_wrong_digest() {
        let key_info = PublicKeyInfo::Ed25519;
        let result = select_mechanism(&key_info, Some(DigestAlgorithm::Sha256));
        assert!(result.is_err());
    }

    #[test]
    fn test_signature_lengths() {
        assert_eq!(SigningMechanism::Rsa2048Sha256.signature_length(), 256);
        assert_eq!(SigningMechanism::Rsa3072Sha384.signature_length(), 384);
        assert_eq!(SigningMechanism::Rsa4096Sha512.signature_length(), 512);
        assert_eq!(SigningMechanism::Ed25519.signature_length(), 64);
    }
}
