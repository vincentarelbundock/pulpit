#![forbid(unsafe_code)]
//! PKCS#12 credential loading with p12-keystore crate (§30).
//! Credential structure and key analysis for S1.

use crate::sign::errors::SigningError;
use der::{Decode, Encode};
use pkcs8::DecodePrivateKey;
use sha2::Digest;
use x509_cert::Certificate;
use zeroize::Zeroize;
#[cfg(feature = "p12-keystore")]
use zeroize::Zeroizing;

#[derive(Debug)]
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

impl std::fmt::Debug for ZeroizingKeyMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZeroizingKeyMaterial")
            .field("_key_type", &self._key_type)
            .field("data", &"<redacted>")
            .finish()
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

/// Load a PKCS#12 credential.
///
/// The passphrase is taken **by value** in a [`Zeroizing<String>`] so that this
/// function owns the only live copy and its buffer is wiped when this function
/// returns (§30). Callers must not keep a plain `String` copy alive.
pub fn load_pkcs12_impl(
    pkcs12_data: &[u8],
    passphrase: zeroize::Zeroizing<String>,
) -> Result<Credential, SigningError> {
    #[cfg(feature = "p12-keystore")]
    {
        // Load the PKCS#12 KeyStore with default import policy
        let policy = p12_keystore::Pkcs12ImportPolicy::default();
        let keystore =
            p12_keystore::KeyStore::from_pkcs12(pkcs12_data, passphrase.as_str(), policy).map_err(
                |e| {
                    let err_msg = format!("{:?}", e);
                    // Map MAC/decryption failures to WrongPassphrase
                    if err_msg.contains("MacError")
                        || err_msg.contains("InvalidMac")
                        || err_msg.contains("InvalidPassword")
                        || err_msg.contains("decrypt")
                    {
                        SigningError::WrongPassphrase
                    } else {
                        SigningError::KeyLoadFailed(format!("Failed to load PKCS#12: {}", err_msg))
                    }
                },
            )?;

        // Find the first private key entry in the keystore
        let mut key_der = None;
        let mut signer_cert = None;
        let mut cert_chain = Vec::new();

        // The passphrase is no longer needed once the keystore is decrypted;
        // dropping it here wipes the buffer we own.
        drop(passphrase);

        for (_name, entry) in keystore.entries() {
            match entry {
                p12_keystore::KeyStoreEntry::PrivateKeyChain(chain) => {
                    // Get the private key DER (PKCS#8 format) via PrivateKey::as_der()
                    let key_bytes = chain.key().as_der();
                    key_der = Some(Zeroizing::new(key_bytes.to_vec()));

                    // Get the certificate chain - certs() returns &[Certificate]
                    let certs = chain.certs();
                    if !certs.is_empty() {
                        if let Ok(cert) = Certificate::from_der(certs[0].as_der()) {
                            signer_cert = Some(cert);
                        }
                    }

                    // Add remaining chain certificates
                    for cert in certs.iter().skip(1) {
                        if let Ok(cert_obj) = Certificate::from_der(cert.as_der()) {
                            cert_chain.push(cert_obj);
                        }
                    }
                    break;
                }
                p12_keystore::KeyStoreEntry::Certificate(_cert) => {
                    // Skip standalone certificates; we're looking for key chains
                }
                p12_keystore::KeyStoreEntry::Secret(_secret) => {
                    // Skip secrets; we're looking for key chains
                }
            }
        }

        let key_der = key_der.ok_or_else(|| {
            SigningError::KeyLoadFailed("No private key found in PKCS#12".to_string())
        })?;

        let signer_cert = signer_cert.ok_or_else(|| {
            SigningError::KeyLoadFailed("No certificate found in PKCS#12".to_string())
        })?;

        let (key_type, public_key_info) = analyze_private_key(&key_der)?;

        check_key_matches_certificate(&key_der, key_type, &signer_cert)?;

        Ok(Credential {
            signer_certificate: signer_cert,
            cert_chain,
            key_material: ZeroizingKeyMaterial::new(key_type, key_der.to_vec()),
            public_key_info,
        })
    }

    #[cfg(not(feature = "p12-keystore"))]
    {
        let _ = (pkcs12_data, passphrase);
        Err(SigningError::KeyLoadFailed(
            "PKCS#12 support requires 'p12-keystore' feature".to_string(),
        ))
    }
}

#[allow(dead_code)]
pub fn from_parts(
    cert_der: &[u8],
    key_der: &[u8],
    chain: Vec<Vec<u8>>,
) -> Result<Credential, SigningError> {
    let signer_certificate = Certificate::from_der(cert_der)
        .map_err(|e| SigningError::InvalidCertificate(format!("Invalid cert: {}", e)))?;

    let mut cert_chain = Vec::new();
    for chain_cert_der in chain {
        if let Ok(c) = Certificate::from_der(&chain_cert_der) {
            cert_chain.push(c);
        }
    }

    let (key_type, public_key_info) = analyze_private_key(key_der)?;
    check_key_matches_certificate(key_der, key_type, &signer_certificate)?;

    Ok(Credential {
        signer_certificate,
        cert_chain,
        key_material: ZeroizingKeyMaterial::new(key_type, key_der.to_vec()),
        public_key_info,
    })
}

/// `id-ecPublicKey`.
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
/// `prime256v1` / NIST P-256.
const OID_SECP256R1: &str = "1.2.840.10045.3.1.7";
/// `secp384r1` / NIST P-384 (SEC 2, ansi-X9-62 arc `1.3.132.0.34`).
const OID_SECP384R1: &str = "1.3.132.0.34";
/// `secp521r1` / NIST P-521 (`1.3.132.0.35`).
const OID_SECP521R1: &str = "1.3.132.0.35";

/// Map a named-curve OID to a key type. Unknown curves are refused by name.
fn classify_named_curve(curve_oid: &str) -> Result<(KeyType, PublicKeyInfo), SigningError> {
    match curve_oid {
        OID_SECP256R1 => Ok((KeyType::EcP256, PublicKeyInfo::EcP256)),
        OID_SECP384R1 => Ok((KeyType::EcP384, PublicKeyInfo::EcP384)),
        OID_SECP521R1 => Ok((KeyType::EcP521, PublicKeyInfo::EcP521)),
        other => Err(SigningError::UnsupportedKeyAlgorithm {
            algorithm: format!("unsupported EC curve {other}"),
        }),
    }
}

/// Cheap sanity check that the private key in the PKCS#12 actually belongs to
/// the certificate it shipped with: derive the public key from the private key
/// and compare its DER `SubjectPublicKeyInfo` bits against the certificate's.
///
/// A mismatch means the credential would produce signatures no verifier could
/// validate against the embedded certificate, so it is refused at load time.
/// Key types we cannot derive a public key for are passed through unchecked
/// rather than guessed at.
fn check_key_matches_certificate(
    key_der: &[u8],
    key_type: KeyType,
    cert: &Certificate,
) -> Result<(), SigningError> {
    use pkcs8::EncodePublicKey;

    let derived: Option<Vec<u8>> = match key_type {
        KeyType::Rsa => rsa::RsaPrivateKey::from_pkcs8_der(key_der)
            .ok()
            .and_then(|k| k.to_public_key().to_public_key_der().ok())
            .map(|d| d.as_bytes().to_vec()),
        KeyType::EcP256 => p256::SecretKey::from_pkcs8_der(key_der)
            .ok()
            .and_then(|k| k.public_key().to_public_key_der().ok())
            .map(|d| d.as_bytes().to_vec()),
        KeyType::EcP384 => p384::SecretKey::from_pkcs8_der(key_der)
            .ok()
            .and_then(|k| k.public_key().to_public_key_der().ok())
            .map(|d| d.as_bytes().to_vec()),
        KeyType::EcP521 => p521::SecretKey::from_pkcs8_der(key_der)
            .ok()
            .and_then(|k| k.public_key().to_public_key_der().ok())
            .map(|d| d.as_bytes().to_vec()),
        KeyType::Ed25519 => {
            use ed25519_dalek::pkcs8::DecodePrivateKey as _;
            ed25519_dalek::SigningKey::from_pkcs8_der(key_der)
                .ok()
                .and_then(|k| k.verifying_key().to_public_key_der().ok())
                .map(|d| d.as_bytes().to_vec())
        }
    };

    let Some(derived_spki) = derived else {
        // Could not derive; nothing to compare against, so do not claim a
        // verdict either way.
        return Ok(());
    };

    let cert_spki = cert
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|e| SigningError::InvalidCertificate(format!("certificate public key: {e}")))?;

    if derived_spki != cert_spki {
        return Err(SigningError::KeyLoadFailed(
            "the private key does not match the certificate's public key; \
             the credential file is unchanged — check that the PKCS#12 pairs \
             the right key and certificate"
                .to_string(),
        ));
    }
    Ok(())
}

fn analyze_private_key(pkey_der: &[u8]) -> Result<(KeyType, PublicKeyInfo), SigningError> {
    let pki = pkcs8::PrivateKeyInfo::from_der(pkey_der)
        .map_err(|e| SigningError::InvalidCertificate(format!("Invalid key: {}", e)))?;

    let oid_str = pki.algorithm.oid.to_string();

    if oid_str == "1.2.840.113549.1.1.1" {
        let bits = extract_rsa_bits(pkey_der).unwrap_or(2048);
        return Ok((KeyType::Rsa, PublicKeyInfo::Rsa { bits }));
    }

    // Handle EC keys: the algorithm OID is the generic id-ecPublicKey, and the
    // named curve lives in the algorithm parameters. There is no safe default
    // here — an unrecognised or undecodable curve is an error, never a guess.
    if oid_str == OID_EC_PUBLIC_KEY {
        let params =
            pki.algorithm
                .parameters
                .ok_or_else(|| SigningError::UnsupportedKeyAlgorithm {
                    algorithm:
                        "EC key without named-curve parameters (only named curves are supported)"
                            .to_string(),
                })?;
        let curve_oid = <der::asn1::ObjectIdentifier>::try_from(params).map_err(|_| {
            SigningError::UnsupportedKeyAlgorithm {
                algorithm: "EC key whose parameters are not a named-curve OID".to_string(),
            }
        })?;
        return classify_named_curve(&curve_oid.to_string());
    }

    // Some producers put the named-curve OID directly in the algorithm field.
    if matches!(
        oid_str.as_str(),
        OID_SECP256R1 | OID_SECP384R1 | OID_SECP521R1
    ) {
        return classify_named_curve(&oid_str);
    }
    if oid_str == "1.3.101.112" {
        return Ok((KeyType::Ed25519, PublicKeyInfo::Ed25519));
    }

    Err(SigningError::UnsupportedKeyAlgorithm { algorithm: oid_str })
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

        let cert_der = self
            .signer_certificate
            .to_der()
            .map_err(|e| SigningError::DerEncodingFailed(e.to_string()))?;
        let fingerprint = {
            let mut h = sha2::Sha256::new();
            h.update(&cert_der);
            hex::encode(h.finalize())
        };

        Ok(CredentialSummary {
            subject: self.signer_certificate.tbs_certificate.subject.to_string(),
            issuer: self.signer_certificate.tbs_certificate.issuer.to_string(),
            serial: hex::encode(
                self.signer_certificate
                    .tbs_certificate
                    .serial_number
                    .as_bytes(),
            ),
            not_before: self
                .signer_certificate
                .tbs_certificate
                .validity
                .not_before
                .to_string(),
            not_after: self
                .signer_certificate
                .tbs_certificate
                .validity
                .not_after
                .to_string(),
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
    fn named_curve_oids_classify_to_the_right_curve() {
        // The canonical SEC 2 / ANSI X9.62 named-curve OIDs. These must match
        // what the verifier accepts (verify/cms_check.rs).
        assert!(matches!(
            classify_named_curve("1.2.840.10045.3.1.7"),
            Ok((KeyType::EcP256, PublicKeyInfo::EcP256))
        ));
        assert!(matches!(
            classify_named_curve("1.3.132.0.34"),
            Ok((KeyType::EcP384, PublicKeyInfo::EcP384))
        ));
        assert!(matches!(
            classify_named_curve("1.3.132.0.35"),
            Ok((KeyType::EcP521, PublicKeyInfo::EcP521))
        ));
    }

    #[test]
    fn unknown_curve_oids_are_refused_by_name_never_defaulted() {
        // Includes the two OIDs this code previously used by mistake for
        // P-384/P-521; they are not named curves at all and must be refused.
        for bogus in [
            "1.3.132.1.12.0",
            "1.3.132.1.12.1",
            "1.3.132.0.10", // secp256k1: a real curve, but not supported here
            "1.2.3.4",
        ] {
            match classify_named_curve(bogus) {
                Err(SigningError::UnsupportedKeyAlgorithm { algorithm }) => {
                    assert!(
                        algorithm.contains(bogus),
                        "error must name the offending OID, got {algorithm}"
                    );
                }
                other => panic!("expected unsupported-curve error for {bogus}, got {other:?}"),
            }
        }
    }

    #[test]
    fn ec_key_without_named_curve_parameters_is_refused() {
        // id-ecPublicKey with absent parameters: a PKCS#8 PrivateKeyInfo whose
        // AlgorithmIdentifier is `SEQUENCE { OID 1.2.840.10045.2.1 }`.
        let der = hex::decode("3011020100300906072a8648ce3d0201040100").expect("valid hex");
        match analyze_private_key(&der) {
            Err(SigningError::UnsupportedKeyAlgorithm { .. }) => {}
            other => panic!("expected unsupported-key error, got {other:?}"),
        }
    }

    #[test]
    fn test_public_key_info_bits() {
        assert_eq!(PublicKeyInfo::Rsa { bits: 2048 }.bits(), Some(2048));
        assert_eq!(PublicKeyInfo::EcP256.bits(), Some(256));
        assert_eq!(PublicKeyInfo::Ed25519.bits(), None);
    }

    #[test]
    #[cfg(feature = "p12-keystore")]
    fn test_pkcs12_loading_and_passphrase() {
        // Generate a test certificate and key with rcgen
        let subject_alt_names = vec!["test.example.com".to_string()];
        let cert_params = rcgen::CertificateParams::new(subject_alt_names);
        let cert = rcgen::Certificate::from_params(cert_params).unwrap();

        // Get the certificate and key DER
        let cert_der = cert.serialize_der().unwrap();
        let key_der = cert.serialize_private_key_der();

        // Create a PKCS#12 file with the certificate and key
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

        let password = "test_password_123";
        let p12_bytes = keystore
            .writer(password)
            .write()
            .expect("Failed to write PKCS#12");

        // Test loading with correct passphrase succeeds
        let credential =
            load_pkcs12_impl(&p12_bytes, zeroize::Zeroizing::new(password.to_string()))
                .expect("Failed to load PKCS#12");
        // Verify we got a credential with certificate and key info
        // The signer_certificate should be populated from the PKCS#12
        assert!(
            !credential.public_key_info.key_type_name().is_empty(),
            "Expected key type to be identified"
        );

        // Test loading with wrong passphrase fails with WrongPassphrase error
        let result = load_pkcs12_impl(
            &p12_bytes,
            zeroize::Zeroizing::new("wrong_password".to_string()),
        );
        assert!(result.is_err(), "Loading with wrong passphrase should fail");
        match result {
            Err(SigningError::WrongPassphrase) => {
                // Expected - this is what we're testing
            }
            Err(e) => panic!("Expected WrongPassphrase error, got {:?}", e),
            Ok(_) => panic!("Expected error, but loading succeeded"),
        }
    }

    #[test]
    #[cfg(feature = "p12-keystore")]
    fn test_pkcs12_loading_missing_key() {
        // Create a PKCS#12 with only a certificate, no private key
        let keystore = p12_keystore::KeyStore::new();
        let p12_bytes = keystore
            .writer("password")
            .write()
            .expect("Failed to write empty PKCS#12");

        // Try to load - should fail because there's no private key
        let result = load_pkcs12_impl(&p12_bytes, zeroize::Zeroizing::new("password".to_string()));
        assert!(
            result.is_err(),
            "Loading PKCS#12 without private key should fail"
        );
    }
}
