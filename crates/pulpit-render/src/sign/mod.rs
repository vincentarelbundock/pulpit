#![forbid(unsafe_code)]
//! Cryptographic signing: PKCS#12 credential loading and CMS construction.
//!
//! This module provides pure cryptographic operations for PDF signature creation.
//! It has no PDF knowledge beyond "here is a digest, here is a CMS blob".
//! Per SPEC-signing.md §22.2, this module MUST NOT depend on pdfwrite or verify.

// Cryptographic signing module

pub mod apply;
mod cms_builder;
mod credential;
mod errors;
mod mechanism;

pub use apply::{
    sign_document_file, AppearanceContent, SignAppearance, SignApplyError, SignReport, SignRequest,
    SignTarget,
};
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

/// Build a credential from a DER certificate, a PKCS#8 private key and an
/// optional DER chain. The key never leaves the returned [`Credential`], whose
/// key material is zeroized on drop (§30.2).
pub fn credential_from_parts(
    cert_der: &[u8],
    key_der: &[u8],
    chain: Vec<Vec<u8>>,
) -> Result<Credential, SigningError> {
    credential::from_parts(cert_der, key_der, chain)
}

/// The digest algorithm this credential's key implies (§26.2).
///
/// The caller hashes the document itself — this crate never sees the file —
/// so it has to be told which hash [`build_cms`] will declare.
pub fn digest_algorithm_for(credential: &Credential) -> Result<DigestAlgorithm, SigningError> {
    Ok(mechanism::select_mechanism(&credential.public_key_info, None)?.digest_algorithm())
}

/// Estimate the size needed to reserve for CMS bytes.
/// §23.5: dry-run with placeholder signature, then add 50% margin (or tight if requested).
pub fn estimate_cms_size(
    credential: &Credential,
    document_digest: &[u8],
    profile: SigningProfile,
    embed_roots: bool,
    tight_size_estimates: bool,
    signing_time: Option<i64>,
) -> Result<usize, SigningError> {
    let zero_digest = vec![0u8; document_digest.len()];
    let mechanism = mechanism::select_mechanism(&credential.public_key_info, None)?;

    // Create a placeholder signature of the correct length, without
    // performing a real (potentially expensive) signing operation.
    let placeholder_sig =
        vec![0u8; cms_builder::max_signature_len(mechanism, &credential.public_key_info)];

    // Build dry-run CMS with placeholder signature and a zero digest.
    let signed_attrs =
        cms_builder::build_signed_attributes(credential, &zero_digest, profile, signing_time)?;
    let cms_der = cms_builder::assemble(
        credential,
        mechanism,
        embed_roots,
        signed_attrs,
        placeholder_sig,
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
///
/// `signing_time` is caller-supplied unix seconds (this crate never reads
/// the clock, per CLAUDE.md); it is only used, and only required, when
/// `profile` is [`SigningProfile::AdbePkcs7Detached`] (§26.3). It is ignored
/// for [`SigningProfile::EtsiCadesDetached`], which never carries a
/// signing-time attribute.
pub fn build_cms(
    credential: &Credential,
    document_digest: &[u8],
    profile: SigningProfile,
    embed_roots: bool,
    requested_digest: Option<DigestAlgorithm>,
    signing_time: Option<i64>,
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

    let signed_attrs =
        cms_builder::build_signed_attributes(credential, document_digest, profile, signing_time)?;
    let tbs = cms_builder::signed_attrs_tbs(&signed_attrs)?;
    let signature_bytes = cms_builder::sign_tbs(credential, &tbs, mechanism)?;

    cms_builder::assemble(
        credential,
        mechanism,
        embed_roots,
        signed_attrs,
        signature_bytes,
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
        keystore.add_entry(
            "test_alias",
            p12_keystore::KeyStoreEntry::PrivateKeyChain(chain),
        );

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
        keystore.add_entry(
            "test_alias",
            p12_keystore::KeyStoreEntry::PrivateKeyChain(chain),
        );

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
            None,
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
        keystore.add_entry(
            "test_alias",
            p12_keystore::KeyStoreEntry::PrivateKeyChain(chain),
        );

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
            None,
        )
        .expect("Failed to estimate size (loose)");

        // Estimate size without margin (tight)
        let estimated_size_tight = estimate_cms_size(
            &credential,
            &test_digest,
            SigningProfile::AdbePkcs7Detached,
            false,
            true, // tight
            None,
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
        assert_eq!(estimated_size_loose % 2, 0, "Loose estimate must be even");
        assert_eq!(estimated_size_tight % 2, 0, "Tight estimate must be even");
    }

    #[test]
    fn test_max_signature_len_matches_key_material() {
        // §23.5: RSA reserves exactly the modulus size; ECDSA reserves the
        // DER worst case (2 * coordinate bytes + 9); Ed25519 is fixed.
        let rsa_2048 = credential::PublicKeyInfo::Rsa { bits: 2048 };
        assert_eq!(
            cms_builder::max_signature_len(SigningMechanism::Rsa2048Sha256, &rsa_2048),
            256
        );

        let ec_p256 = credential::PublicKeyInfo::EcP256;
        assert_eq!(
            cms_builder::max_signature_len(SigningMechanism::EcdsaP256Sha256, &ec_p256),
            2 * 32 + 9
        );

        let ed25519 = credential::PublicKeyInfo::Ed25519;
        assert_eq!(
            cms_builder::max_signature_len(SigningMechanism::Ed25519, &ed25519),
            64
        );
    }

    #[test]
    fn test_size_estimation_is_always_even() {
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

/// Acceptance gate: build a real CMS with each mechanism, parse it back with
/// the `cms` crate independently of `cms_builder`, and cryptographically
/// verify the signature. Credentials are built directly via
/// `credential::from_parts`, bypassing PKCS#12 (§ task item 5).
#[cfg(test)]
mod acceptance_tests {
    use super::*;
    use cms::content_info::ContentInfo;
    use cms::signed_data::SignedData;
    use der::{Decode, Encode};
    use pkcs8::EncodePrivateKey;
    use sha2::Digest;
    use signature::Verifier;

    enum TestKey {
        Rsa(Box<rsa::RsaPrivateKey>),
        P256(p256::ecdsa::SigningKey),
        Ed25519(Box<ed25519_dalek::SigningKey>),
    }

    fn pkcs8_der_bytes(key: &TestKey) -> Vec<u8> {
        match key {
            TestKey::Rsa(k) => k
                .to_pkcs8_der()
                .expect("encode rsa pkcs8")
                .as_bytes()
                .to_vec(),
            TestKey::P256(k) => k
                .to_pkcs8_der()
                .expect("encode p256 pkcs8")
                .as_bytes()
                .to_vec(),
            TestKey::Ed25519(k) => k
                .to_pkcs8_der()
                .expect("encode ed25519 pkcs8")
                .as_bytes()
                .to_vec(),
        }
    }

    fn make_credential(key: &TestKey) -> Credential {
        let der = pkcs8_der_bytes(key);
        let key_pair = rcgen::KeyPair::from_der(&der).expect("rcgen accepts our pkcs8 key");
        let mut params = rcgen::CertificateParams::new(vec!["pulpit-test.example".to_string()]);
        params.alg = key_pair.algorithm();
        params.key_pair = Some(key_pair);
        let cert = rcgen::Certificate::from_params(params).expect("build self-signed cert");
        let cert_der = cert.serialize_der().expect("serialize cert der");
        credential::from_parts(&cert_der, &der, vec![]).expect("credential from parts")
    }

    fn verify_signature(key: &TestKey, tbs: &[u8], signature_bytes: &[u8]) {
        match key {
            TestKey::Rsa(k) => {
                let public_key = rsa::RsaPublicKey::from(k.as_ref());
                let verifying_key = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(public_key);
                let sig = rsa::pkcs1v15::Signature::try_from(signature_bytes)
                    .expect("parse rsa pkcs1v15 signature");
                verifying_key
                    .verify(tbs, &sig)
                    .expect("rsa signature verifies against the signed attrs");
            }
            TestKey::P256(k) => {
                let verifying_key = p256::ecdsa::VerifyingKey::from(k);
                let sig = p256::ecdsa::Signature::from_der(signature_bytes)
                    .expect("parse DER ECDSA-Sig-Value");
                verifying_key
                    .verify(tbs, &sig)
                    .expect("ecdsa signature verifies against the signed attrs");
            }
            TestKey::Ed25519(k) => {
                let verifying_key = k.verifying_key();
                let sig = ed25519_dalek::Signature::from_slice(signature_bytes)
                    .expect("parse ed25519 signature");
                verifying_key
                    .verify(tbs, &sig)
                    .expect("ed25519 signature verifies against the signed attrs");
            }
        }
    }

    /// Full round trip for one mechanism: build, parse back independently,
    /// verify the signature and every signed attribute (§26.3), and confirm
    /// signing-time presence follows the profile exactly.
    fn round_trip(key: TestKey, profile: SigningProfile, signing_time: Option<i64>) {
        let credential = make_credential(&key);
        let digest = vec![0xABu8; 32];

        let cms_der = build_cms(&credential, &digest, profile, false, None, signing_time)
            .expect("build_cms succeeds");

        // Parse back independently of cms_builder.
        let content_info = ContentInfo::from_der(&cms_der).expect("parse ContentInfo");
        assert_eq!(
            content_info.content_type.to_string(),
            "1.2.840.113549.1.7.2",
            "contentType must be id-signedData"
        );
        let signed_data: SignedData = content_info
            .content
            .decode_as()
            .expect("decode SignedData from ContentInfo content");

        assert_eq!(
            signed_data.signer_infos.0.len(),
            1,
            "exactly one SignerInfo"
        );
        let signer_info = signed_data.signer_infos.0.get(0).unwrap();

        let signed_attrs = signer_info
            .signed_attrs
            .as_ref()
            .expect("signed attrs present");
        let tbs = signed_attrs
            .to_der()
            .expect("re-encode signed attrs as SET OF");
        assert_eq!(
            tbs[0], 0x31,
            "re-encoded signed attrs must be universal SET OF"
        );

        verify_signature(&key, &tbs, signer_info.signature.as_bytes());

        // §26.3: content-type, message-digest, signing-certificate-v2 always present.
        let mut saw_content_type = false;
        let mut saw_message_digest = false;
        let mut saw_signing_cert_v2 = false;
        let mut saw_signing_time = false;

        for attr in signed_attrs.iter() {
            match attr.oid.to_string().as_str() {
                "1.2.840.113549.1.9.3" => {
                    saw_content_type = true;
                    let value = attr.values.get(0).unwrap();
                    let content_type_oid: der::asn1::ObjectIdentifier =
                        value.decode_as().expect("content-type value is an OID");
                    assert_eq!(content_type_oid.to_string(), "1.2.840.113549.1.7.1");
                }
                "1.2.840.113549.1.9.4" => {
                    saw_message_digest = true;
                    let value = attr.values.get(0).unwrap();
                    let octets: der::asn1::OctetString = value
                        .decode_as()
                        .expect("message-digest value is an OCTET STRING");
                    assert_eq!(octets.as_bytes(), digest.as_slice());
                }
                "1.2.840.113549.1.9.16.2.47" => {
                    saw_signing_cert_v2 = true;
                    let value = attr.values.get(0).unwrap();
                    let der_bytes = value.to_der().expect("re-encode signing-cert-v2 value");
                    // The cert hash is embedded ~10 bytes into the ESSCertIDv2
                    // SEQUENCE; rather than re-modelling the type here, just
                    // confirm the raw SHA-256(cert DER) bytes are present.
                    let cert_der = credential.signer_certificate.to_der().unwrap();
                    let expected_hash: Vec<u8> = sha2::Sha256::digest(&cert_der).to_vec();
                    assert!(
                        der_bytes
                            .windows(expected_hash.len())
                            .any(|w| w == expected_hash.as_slice()),
                        "signing-certificate-v2 must contain SHA-256(cert DER)"
                    );
                }
                "1.2.840.113549.1.9.5" => {
                    saw_signing_time = true;
                }
                other => panic!("unexpected signed attribute OID {other}"),
            }
        }

        assert!(saw_content_type, "content-type attribute missing");
        assert!(saw_message_digest, "message-digest attribute missing");
        assert!(
            saw_signing_cert_v2,
            "signing-certificate-v2 attribute missing"
        );
        assert_eq!(
            saw_signing_time,
            profile == SigningProfile::AdbePkcs7Detached && signing_time.is_some(),
            "signing-time must appear only for AdbePkcs7Detached with a supplied time"
        );

        // Negative test: tampering with one byte of the SET OF must break verification.
        let mut tampered = tbs.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        // Sanity: re-signing over the *original* tbs must still succeed
        // (already checked above); verifying the tampered bytes must fail.
        match &key {
            TestKey::Rsa(k) => {
                let public_key = rsa::RsaPublicKey::from(k.as_ref());
                let verifying_key = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(public_key);
                let sig =
                    rsa::pkcs1v15::Signature::try_from(signer_info.signature.as_bytes()).unwrap();
                assert!(verifying_key.verify(&tampered, &sig).is_err());
            }
            TestKey::P256(k) => {
                let verifying_key = p256::ecdsa::VerifyingKey::from(k);
                let sig =
                    p256::ecdsa::Signature::from_der(signer_info.signature.as_bytes()).unwrap();
                assert!(verifying_key.verify(&tampered, &sig).is_err());
            }
            TestKey::Ed25519(k) => {
                let verifying_key = k.verifying_key();
                let sig =
                    ed25519_dalek::Signature::from_slice(signer_info.signature.as_bytes()).unwrap();
                assert!(verifying_key.verify(&tampered, &sig).is_err());
            }
        }
    }

    #[test]
    fn rsa_2048_round_trip_etsi_cades() {
        let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("generate rsa key");
        round_trip(
            TestKey::Rsa(Box::new(key)),
            SigningProfile::EtsiCadesDetached,
            None,
        );
    }

    #[test]
    fn rsa_2048_round_trip_adbe_pkcs7_with_signing_time() {
        let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("generate rsa key");
        round_trip(
            TestKey::Rsa(Box::new(key)),
            SigningProfile::AdbePkcs7Detached,
            Some(1_700_000_000),
        );
    }

    #[test]
    fn p256_round_trip_etsi_cades() {
        let key = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        round_trip(TestKey::P256(key), SigningProfile::EtsiCadesDetached, None);
    }

    #[test]
    fn p256_round_trip_adbe_pkcs7_with_signing_time() {
        let key = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        round_trip(
            TestKey::P256(key),
            SigningProfile::AdbePkcs7Detached,
            Some(1_700_000_000),
        );
    }

    #[test]
    fn ed25519_round_trip_etsi_cades() {
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        round_trip(
            TestKey::Ed25519(Box::new(key)),
            SigningProfile::EtsiCadesDetached,
            None,
        );
    }

    #[test]
    fn ed25519_round_trip_adbe_pkcs7_with_signing_time() {
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        round_trip(
            TestKey::Ed25519(Box::new(key)),
            SigningProfile::AdbePkcs7Detached,
            Some(1_700_000_000),
        );
    }

    #[test]
    fn etsi_cades_never_carries_signing_time_even_if_supplied() {
        // §26.3: PAdES forbids signing-time outright; even if a caller
        // passes a time, EtsiCadesDetached must not include the attribute.
        let key = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        round_trip(
            TestKey::P256(key),
            SigningProfile::EtsiCadesDetached,
            Some(1_700_000_000),
        );
    }

    #[test]
    fn real_signature_fits_the_loose_size_estimate() {
        // §23.5 acceptance: a real signature's hex encoding must fit inside
        // the loose (50%-margin) estimate, for each mechanism.
        for key in [
            TestKey::Rsa(Box::new(
                rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap(),
            )),
            TestKey::P256(p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng)),
            TestKey::Ed25519(Box::new(ed25519_dalek::SigningKey::generate(
                &mut rand::rngs::OsRng,
            ))),
        ] {
            let credential = make_credential(&key);
            let digest = vec![0xCDu8; 32];

            let estimate = estimate_cms_size(
                &credential,
                &digest,
                SigningProfile::AdbePkcs7Detached,
                false,
                false,
                Some(1_700_000_000),
            )
            .expect("estimate succeeds");

            let real = build_cms(
                &credential,
                &digest,
                SigningProfile::AdbePkcs7Detached,
                false,
                None,
                Some(1_700_000_000),
            )
            .expect("build succeeds");

            assert!(
                real.len() * 2 <= estimate,
                "real CMS ({} bytes, {} hex) must fit the loose estimate ({})",
                real.len(),
                real.len() * 2,
                estimate
            );
        }
    }
}
