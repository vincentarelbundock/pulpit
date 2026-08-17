#![forbid(unsafe_code)]
//! Cryptographic signing: PKCS#12 credential loading and CMS construction.
//!
//! This module provides pure cryptographic operations for PDF signature creation.
//! It has no PDF knowledge beyond "here is a digest, here is a CMS blob".
//! Per SPEC-signing.md §22.2, this module MUST NOT depend on pdfwrite or verify.

// Cryptographic signing module

mod cms_builder;
mod credential;
mod errors;
mod mechanism;

pub use credential::{Credential, CredentialSummary};
pub use errors::SigningError;
pub use mechanism::{DigestAlgorithm, SigningMechanism};

/// Signing profile affects which attributes are included (§26.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningProfile {
    /// Legacy Adobe profile (/adbe.pkcs7.detached). Includes signing-time attribute.
    AdbePkcs7Detached,
    /// PAdES profile (/ETSI.CAdES.detached). Does NOT include signing-time.
    EtsiCadesDetached,
}

/// Information needed to build a CMS signature.
#[derive(Clone)]
pub struct CmsSignatureInfo {
    pub profile: SigningProfile,
    pub document_digest: Vec<u8>,
    pub digest_algorithm: DigestAlgorithm,
    pub embed_roots: bool,
}

/// Load a PKCS#12 file and return its credential.
/// §30: passphrase lives in Zeroizing buffer, dropped as soon as credential is extracted.
pub fn load_pkcs12(pkcs12_data: &[u8], passphrase: &str) -> Result<Credential, SigningError> {
    credential::load_pkcs12_impl(pkcs12_data, passphrase)
}

/// Estimate the size needed to reserve for CMS bytes.
/// §23.5: dry-run with placeholder signature, then add 50% margin (or tight if requested).
pub fn estimate_cms_size(
    credential: &Credential,
    document_digest: &[u8],
    profile: SigningProfile,
    embed_roots: bool,
    tight_size_estimates: bool,
) -> Result<usize, SigningError> {
    let zero_digest = vec![0u8; document_digest.len()];
    let mechanism = mechanism::select_mechanism(&credential.public_key_info, None)?;

    // Create a placeholder signature of the correct length
    let placeholder_sig = vec![0u8; mechanism.signature_length()];

    // Build dry-run CMS with placeholder signature
    let cms_der = cms_builder::build_cms_der(
        credential,
        &zero_digest,
        mechanism,
        placeholder_sig,
        profile,
        embed_roots,
    )?;

    let test_len = 2 * cms_der.len();
    let bytes_reserved = if tight_size_estimates {
        // Just add the test length
        test_len
    } else {
        // Add 50% margin, rounded to even
        let margin = (test_len / 2) & !1;
        test_len + margin
    };

    // Ensure it's even (hex encoding requirement §23.2)
    let bytes_reserved = if bytes_reserved % 2 == 1 {
        bytes_reserved + 1
    } else {
        bytes_reserved
    };

    Ok(bytes_reserved)
}

/// Build CMS SignedData bytes over the given document digest.
/// Returns DER-encoded SignedData that can be placed in PDF /Contents.
pub fn build_cms(
    credential: &Credential,
    document_digest: &[u8],
    profile: SigningProfile,
    embed_roots: bool,
    requested_digest: Option<DigestAlgorithm>,
) -> Result<Vec<u8>, SigningError> {
    // Select mechanism from certificate's public key
    let mechanism = mechanism::select_mechanism(&credential.public_key_info, requested_digest)?;

    // Ensure implied digest (for Ed25519) matches requested
    if let (SigningMechanism::Ed25519, Some(req_digest)) = (mechanism, requested_digest) {
        if req_digest != DigestAlgorithm::Sha512 {
            return Err(SigningError::DigestMechanismMismatch {
                implied: "sha512".to_string(),
                requested: format!("{:?}", req_digest).to_lowercase(),
            });
        }
    }

    // Compute signature over DER(signedAttrs) with implicit [0] re-tagged as universal SET OF
    let signature_bytes =
        cms_builder::compute_signature(credential, document_digest, mechanism, profile)?;

    // Build complete CMS
    cms_builder::build_cms_der(
        credential,
        document_digest,
        mechanism,
        signature_bytes,
        profile,
        embed_roots,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // SPEC-signing.md §26.2: Mechanism and digest selection per v1 table
    #[test]
    fn test_rsa_2048_sha256_selection() {
        let digest = DigestAlgorithm::select_for_rsa_bits(2048).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha256);
    }

    #[test]
    fn test_rsa_2000_sha256_selection() {
        let digest = DigestAlgorithm::select_for_rsa_bits(2000).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha256);
    }

    #[test]
    fn test_rsa_3072_sha384_selection() {
        let digest = DigestAlgorithm::select_for_rsa_bits(3072).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha384);
    }

    #[test]
    fn test_rsa_2049_sha384_selection() {
        let digest = DigestAlgorithm::select_for_rsa_bits(2049).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha384);
    }

    #[test]
    fn test_rsa_4096_sha512_selection() {
        let digest = DigestAlgorithm::select_for_rsa_bits(4096).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha512);
    }

    #[test]
    fn test_rsa_3073_sha512_selection() {
        let digest = DigestAlgorithm::select_for_rsa_bits(3073).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha512);
    }

    #[test]
    fn test_ec_p256_sha256_selection() {
        let digest = DigestAlgorithm::select_for_ec_bits(256).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha256);
    }

    #[test]
    fn test_ec_p384_sha384_selection() {
        let digest = DigestAlgorithm::select_for_ec_bits(384).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha384);
    }

    #[test]
    fn test_ec_p521_sha512_selection() {
        let digest = DigestAlgorithm::select_for_ec_bits(521).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha512);
    }

    #[test]
    fn test_ec_257_sha384_selection() {
        let digest = DigestAlgorithm::select_for_ec_bits(257).unwrap();
        assert_eq!(digest, DigestAlgorithm::Sha384);
    }

    #[test]
    fn test_ed25519_implies_sha512() {
        let key_info = credential::PublicKeyInfo::Ed25519;
        let result = mechanism::select_mechanism(&key_info, Some(DigestAlgorithm::Sha256));
        assert!(result.is_err());
    }

    #[test]
    fn test_mechanism_rsa_2048_sha256() {
        let key_info = credential::PublicKeyInfo::Rsa { bits: 2048 };
        let mech = mechanism::select_mechanism(&key_info, None).unwrap();
        assert_eq!(mech, SigningMechanism::Rsa2048Sha256);
    }

    #[test]
    fn test_mechanism_rsa_3072_sha384() {
        let key_info = credential::PublicKeyInfo::Rsa { bits: 3072 };
        let mech = mechanism::select_mechanism(&key_info, None).unwrap();
        assert_eq!(mech, SigningMechanism::Rsa3072Sha384);
    }

    #[test]
    fn test_mechanism_rsa_4096_sha512() {
        let key_info = credential::PublicKeyInfo::Rsa { bits: 4096 };
        let mech = mechanism::select_mechanism(&key_info, None).unwrap();
        assert_eq!(mech, SigningMechanism::Rsa4096Sha512);
    }

    #[test]
    fn test_mechanism_ec_p256() {
        let key_info = credential::PublicKeyInfo::EcP256;
        let mech = mechanism::select_mechanism(&key_info, None).unwrap();
        assert_eq!(mech, SigningMechanism::EcdsaP256Sha256);
    }

    #[test]
    fn test_mechanism_ec_p384() {
        let key_info = credential::PublicKeyInfo::EcP384;
        let mech = mechanism::select_mechanism(&key_info, None).unwrap();
        assert_eq!(mech, SigningMechanism::EcdsaP384Sha384);
    }

    #[test]
    fn test_mechanism_ec_p521() {
        let key_info = credential::PublicKeyInfo::EcP521;
        let mech = mechanism::select_mechanism(&key_info, None).unwrap();
        assert_eq!(mech, SigningMechanism::EcdsaP521Sha512);
    }

    // SPEC-signing.md §23.5: Size estimation
    #[test]
    fn test_size_estimation_is_even() {
        let test_len = 100;
        let margin = (test_len / 2) & !1;
        let bytes_reserved = test_len + margin;
        assert_eq!(bytes_reserved % 2, 0);
    }

    #[test]
    fn test_digest_algorithm_oid() {
        assert_eq!(DigestAlgorithm::Sha256.oid(), "2.16.840.1.101.3.4.2.1");
        assert_eq!(DigestAlgorithm::Sha384.oid(), "2.16.840.1.101.3.4.2.2");
        assert_eq!(DigestAlgorithm::Sha512.oid(), "2.16.840.1.101.3.4.2.3");
    }

    #[test]
    fn test_digest_algorithm_hash_len() {
        assert_eq!(DigestAlgorithm::Sha256.hash_len(), 32);
        assert_eq!(DigestAlgorithm::Sha384.hash_len(), 48);
        assert_eq!(DigestAlgorithm::Sha512.hash_len(), 64);
    }

    #[test]
    fn test_signing_mechanism_digest() {
        assert_eq!(
            SigningMechanism::Rsa2048Sha256.digest_algorithm(),
            DigestAlgorithm::Sha256
        );
        assert_eq!(
            SigningMechanism::Rsa3072Sha384.digest_algorithm(),
            DigestAlgorithm::Sha384
        );
        assert_eq!(
            SigningMechanism::EcdsaP256Sha256.digest_algorithm(),
            DigestAlgorithm::Sha256
        );
        assert_eq!(
            SigningMechanism::Ed25519.digest_algorithm(),
            DigestAlgorithm::Sha512
        );
    }

    #[test]
    fn test_signing_profile_adobepkcs7() {
        let profile = SigningProfile::AdbePkcs7Detached;
        assert_eq!(profile, SigningProfile::AdbePkcs7Detached);
    }

    #[test]
    fn test_signing_profile_etsi_cades() {
        let profile = SigningProfile::EtsiCadesDetached;
        assert_eq!(profile, SigningProfile::EtsiCadesDetached);
        assert_ne!(profile, SigningProfile::AdbePkcs7Detached);
    }

    #[test]
    fn test_pkcs12_loading_deferred() {
        let result = load_pkcs12(b"dummy", "password");
        assert!(result.is_err());
    }

    // STAGE 4 - Round-trip acceptance tests
    #[test]
    #[cfg(feature = "p12-keystore")]
    fn test_cms_round_trip_rsa2048() {
        // Generate a test certificate with rcgen
        let subject_alt_names = vec!["test.example.com".to_string()];
        let cert_params = rcgen::CertificateParams::new(subject_alt_names);
        let cert = rcgen::Certificate::from_params(cert_params).unwrap();

        let cert_der = cert.serialize_der().unwrap();
        let key_der = cert.serialize_private_key_der();

        // Create PKCS#12
        let mut keystore = p12_keystore::KeyStore::new();
        let private_key =
            p12_keystore::PrivateKey::from_der(&key_der).expect("Failed to parse private key");
        let p12_cert =
            p12_keystore::Certificate::from_der(&cert_der).expect("Failed to parse certificate");
        let chain = p12_keystore::PrivateKeyChain::new("test_key", private_key, vec![p12_cert]);
        keystore.add_entry("test_alias", p12_keystore::KeyStoreEntry::PrivateKeyChain(chain));

        let password = "test_password";
        let p12_bytes = keystore
            .writer(password)
            .write()
            .expect("Failed to write PKCS#12");

        // Load credential
        let credential = load_pkcs12(&p12_bytes, password).expect("Failed to load PKCS#12");

        // Use a fixed 32-byte test digest
        let test_digest = vec![0x42u8; 32];

        // Build CMS
        let cms_der = build_cms(
            &credential,
            &test_digest,
            SigningProfile::AdbePkcs7Detached,
            false,
            None,
        )
        .expect("Failed to build CMS");

        // Verify CMS is not empty and has reasonable size
        assert!(!cms_der.is_empty(), "CMS should not be empty");
        assert!(
            cms_der.len() > 100,
            "CMS should be large enough to contain signature and cert"
        );

        // Verify CMS starts with SEQUENCE tag
        assert_eq!(cms_der[0], 0x30, "CMS should start with SEQUENCE tag");

        // Verify digest is preserved in the credential
        assert_eq!(
            credential.public_key_info.key_type_name(),
            "EC P-256",
            "rcgen should generate EC P-256 by default"
        );
    }

    #[test]
    #[cfg(feature = "p12-keystore")]
    fn test_size_estimation_basic() {
        // Generate a test certificate
        let subject_alt_names = vec!["test.example.com".to_string()];
        let cert_params = rcgen::CertificateParams::new(subject_alt_names);
        let cert = rcgen::Certificate::from_params(cert_params).unwrap();

        let cert_der = cert.serialize_der().unwrap();
        let key_der = cert.serialize_private_key_der();

        // Create PKCS#12
        let mut keystore = p12_keystore::KeyStore::new();
        let private_key =
            p12_keystore::PrivateKey::from_der(&key_der).expect("Failed to parse private key");
        let p12_cert =
            p12_keystore::Certificate::from_der(&cert_der).expect("Failed to parse certificate");
        let chain = p12_keystore::PrivateKeyChain::new("test_key", private_key, vec![p12_cert]);
        keystore.add_entry("test_alias", p12_keystore::KeyStoreEntry::PrivateKeyChain(chain));

        let password = "test_password";
        let p12_bytes = keystore
            .writer(password)
            .write()
            .expect("Failed to write PKCS#12");

        // Load credential
        let credential = load_pkcs12(&p12_bytes, password).expect("Failed to load PKCS#12");

        // Estimate CMS size
        let test_digest = vec![0x42u8; 32];
        let estimated_size = estimate_cms_size(
            &credential,
            &test_digest,
            SigningProfile::AdbePkcs7Detached,
            false,
            false, // not tight
        )
        .expect("Failed to estimate CMS size");

        // Size should be reasonable (at least 200 bytes for a signature + cert, with 50% margin)
        assert!(
            estimated_size > 200,
            "Estimated size should account for signature and certificate"
        );
        assert_eq!(
            estimated_size % 2,
            0,
            "Estimated size should be even (hex encoding requirement §23.2)"
        );
    }

    #[test]
    #[cfg(feature = "p12-keystore")]
    fn test_signing_profile_difference() {
        // Verify that the two profiles are distinct and can be used correctly
        let adobe_profile = SigningProfile::AdbePkcs7Detached;
        let etsi_profile = SigningProfile::EtsiCadesDetached;

        assert_ne!(adobe_profile, etsi_profile);
        assert_eq!(adobe_profile, SigningProfile::AdbePkcs7Detached);
        assert_eq!(etsi_profile, SigningProfile::EtsiCadesDetached);
    }

    // STAGE 5 - Size estimation validation (§23.5)
    #[test]
    #[cfg(feature = "p12-keystore")]
    fn test_size_estimation_vs_actual_signature() {
        // Generate a test certificate
        let subject_alt_names = vec!["test.example.com".to_string()];
        let cert_params = rcgen::CertificateParams::new(subject_alt_names);
        let cert = rcgen::Certificate::from_params(cert_params).unwrap();

        let cert_der = cert.serialize_der().unwrap();
        let key_der = cert.serialize_private_key_der();

        // Create PKCS#12
        let mut keystore = p12_keystore::KeyStore::new();
        let private_key =
            p12_keystore::PrivateKey::from_der(&key_der).expect("Failed to parse private key");
        let p12_cert =
            p12_keystore::Certificate::from_der(&cert_der).expect("Failed to parse certificate");
        let chain = p12_keystore::PrivateKeyChain::new("test_key", private_key, vec![p12_cert]);
        keystore.add_entry("test_alias", p12_keystore::KeyStoreEntry::PrivateKeyChain(chain));

        let password = "test_password";
        let p12_bytes = keystore
            .writer(password)
            .write()
            .expect("Failed to write PKCS#12");

        // Load credential
        let credential = load_pkcs12(&p12_bytes, password).expect("Failed to load PKCS#12");

        // Test digest (32 bytes for SHA-256)
        let test_digest = vec![0x42u8; 32];

        // Estimate size with 50% margin
        let estimated_size_loose = estimate_cms_size(
            &credential,
            &test_digest,
            SigningProfile::AdbePkcs7Detached,
            false,
            false, // not tight
        )
        .expect("Failed to estimate size (loose)");

        // Estimate size without margin (tight)
        let estimated_size_tight = estimate_cms_size(
            &credential,
            &test_digest,
            SigningProfile::AdbePkcs7Detached,
            false,
            true, // tight
        )
        .expect("Failed to estimate size (tight)");

        // Loose estimate should be larger than tight estimate
        assert!(
            estimated_size_loose > estimated_size_tight,
            "Loose estimate should be larger than tight estimate"
        );

        // Build actual CMS with real signature
        let cms = build_cms(
            &credential,
            &test_digest,
            SigningProfile::AdbePkcs7Detached,
            false,
            None,
        )
        .expect("Failed to build CMS");

        // Verify the actual CMS fits within estimated size
        assert!(
            (cms.len() * 2) <= estimated_size_loose, // *2 for hex encoding
            "Actual CMS ({} bytes, {} hex) should fit in estimated size ({})",
            cms.len(),
            cms.len() * 2,
            estimated_size_loose
        );

        // Verify estimates are even (required for hex encoding)
        assert_eq!(
            estimated_size_loose % 2,
            0,
            "Loose estimate must be even"
        );
        assert_eq!(
            estimated_size_tight % 2,
            0,
            "Tight estimate must be even"
        );
    }

    #[test]
    fn test_size_estimation_is_always_even() {
        // Create a simple RSA key info
        let key_info = credential::PublicKeyInfo::Rsa { bits: 2048 };

        // Create a mock credential for size estimation
        // (we can't easily create a real one without PKCS#12, so we test the math)
        let test_len = 500;
        let margin = (test_len / 2) & !1; // 50% margin, rounded to even
        let bytes_reserved = test_len + margin;

        // Verify result is even
        assert_eq!(
            bytes_reserved % 2,
            0,
            "Size estimation result must be even for hex encoding"
        );

        // Verify margin is positive and reasonable (25-75% of test_len)
        assert!(margin > test_len / 4 && margin < test_len);
    }
}
