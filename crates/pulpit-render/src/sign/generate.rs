//! Self-signed PKCS#12 credential generation for local signing identities.
//!
//! The operation is deliberately independent of PDF signing: callers provide
//! the validity window and receive an encrypted PKCS#12 container. No clock or
//! filesystem access crosses this boundary.

use std::time::SystemTime;

#[cfg(feature = "p12-keystore")]
use der::Encode;
#[cfg(feature = "p12-keystore")]
use p256::ecdsa::{DerSignature, SigningKey};
#[cfg(feature = "p12-keystore")]
use p256::pkcs8::EncodePrivateKey;
#[cfg(feature = "p12-keystore")]
use rand::rngs::OsRng;
#[cfg(feature = "p12-keystore")]
use rand::RngCore;
use thiserror::Error;
#[cfg(feature = "p12-keystore")]
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
#[cfg(feature = "p12-keystore")]
use x509_cert::name::Name;
#[cfg(feature = "p12-keystore")]
use x509_cert::serial_number::SerialNumber;
#[cfg(feature = "p12-keystore")]
use x509_cert::spki::SubjectPublicKeyInfoOwned;
#[cfg(feature = "p12-keystore")]
use x509_cert::time::{Time, Validity};
use zeroize::Zeroizing;

/// User-visible identity fields for a new local signing credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredentialIdentity {
    pub common_name: String,
    pub organization: Option<String>,
    pub email: Option<String>,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
}

/// A generated encrypted PKCS#12 container and its parsed public summary.
#[derive(Debug)]
pub struct GeneratedPkcs12 {
    pub bytes: Zeroizing<Vec<u8>>,
    pub summary: super::CredentialSummary,
}

#[derive(Debug, Error)]
pub enum CredentialGenerationError {
    #[error("A full name is required for a signing identity")]
    MissingCommonName,
    #[error("The credential expiry must be later than its start date")]
    InvalidValidity,
    #[error("Could not encode the certificate identity: {0}")]
    InvalidIdentity(String),
    #[error("Could not generate the signing certificate: {0}")]
    Certificate(String),
    #[error("Could not package the signing credential: {0}")]
    Pkcs12(String),
    #[error("The generated credential could not be verified: {0}")]
    Verification(String),
}

/// Generate an encrypted, self-signed ECDSA P-256 PKCS#12 credential.
///
/// The passphrase is owned and zeroized here. The returned bytes are also
/// zeroized when dropped; callers normally write them immediately to an
/// owner-private file and then discard them.
#[cfg(feature = "p12-keystore")]
pub fn generate_self_signed_pkcs12(
    identity: &NewCredentialIdentity,
    passphrase: Zeroizing<String>,
) -> Result<GeneratedPkcs12, CredentialGenerationError> {
    let common_name = identity.common_name.trim();
    if common_name.is_empty() {
        return Err(CredentialGenerationError::MissingCommonName);
    }
    if identity.not_after <= identity.not_before {
        return Err(CredentialGenerationError::InvalidValidity);
    }

    let subject = subject_name(identity)?;
    let validity = Validity {
        not_before: Time::try_from(identity.not_before)
            .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?,
        not_after: Time::try_from(identity.not_after)
            .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?,
    };

    let signing_key = SigningKey::random(&mut OsRng);
    let public_key = SubjectPublicKeyInfoOwned::from_key(*signing_key.verifying_key())
        .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?;
    let mut serial = [0u8; 16];
    OsRng.fill_bytes(&mut serial);
    // DER INTEGER values are signed. Keeping the high bit clear makes this a
    // positive serial without needing a leading zero byte.
    serial[0] &= 0x7f;
    if serial.iter().all(|byte| *byte == 0) {
        serial[15] = 1;
    }

    let profile = Profile::Leaf {
        issuer: subject.clone(),
        enable_key_agreement: false,
        enable_key_encipherment: false,
    };
    let builder = CertificateBuilder::new(
        profile,
        SerialNumber::new(&serial)
            .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?,
        validity,
        subject,
        public_key,
        &signing_key,
    )
    .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?;
    let certificate = builder
        .build::<DerSignature>()
        .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?;
    let certificate_der = certificate
        .to_der()
        .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?;
    let private_key_der = Zeroizing::new(
        signing_key
            .to_pkcs8_der()
            .map_err(|e| CredentialGenerationError::Certificate(e.to_string()))?
            .as_bytes()
            .to_vec(),
    );

    let private_key = p12_keystore::PrivateKey::from_der(&private_key_der)
        .map_err(|e| CredentialGenerationError::Pkcs12(format!("{e:?}")))?;
    let p12_certificate = p12_keystore::Certificate::from_der(&certificate_der)
        .map_err(|e| CredentialGenerationError::Pkcs12(format!("{e:?}")))?;
    let chain =
        p12_keystore::PrivateKeyChain::new("pulpit-signing-key", private_key, [p12_certificate]);
    let mut keystore = p12_keystore::KeyStore::new();
    keystore.add_entry(
        "pulpit-signing-identity",
        p12_keystore::KeyStoreEntry::PrivateKeyChain(chain),
    );
    let bytes = Zeroizing::new(
        keystore
            .writer(passphrase.as_str())
            .write()
            .map_err(|e| CredentialGenerationError::Pkcs12(format!("{e:?}")))?,
    );
    let credential = super::load_pkcs12(&bytes, passphrase)
        .map_err(|e| CredentialGenerationError::Verification(e.to_string()))?;
    let summary = credential
        .summary()
        .map_err(|e| CredentialGenerationError::Verification(e.to_string()))?;

    Ok(GeneratedPkcs12 { bytes, summary })
}

#[cfg(not(feature = "p12-keystore"))]
pub fn generate_self_signed_pkcs12(
    _identity: &NewCredentialIdentity,
    _passphrase: Zeroizing<String>,
) -> Result<GeneratedPkcs12, CredentialGenerationError> {
    Err(CredentialGenerationError::Pkcs12(
        "this build does not include PKCS#12 support".to_string(),
    ))
}

#[cfg(feature = "p12-keystore")]
fn subject_name(identity: &NewCredentialIdentity) -> Result<Name, CredentialGenerationError> {
    use std::str::FromStr;
    let mut components = vec![format!("CN={}", escape_name(identity.common_name.trim()))];
    if let Some(organization) = identity
        .organization
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        components.push(format!("O={}", escape_name(organization)));
    }
    // emailAddress is a well-known DN attribute understood by the parser and
    // by common PDF viewers. It is descriptive only; pulpit never treats it
    // as a verified address.
    if let Some(email) = identity
        .email
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        components.push(format!("emailAddress={}", escape_name(email)));
    }
    Name::from_str(&components.join(","))
        .map_err(|e| CredentialGenerationError::InvalidIdentity(e.to_string()))
}

#[cfg(feature = "p12-keystore")]
fn escape_name(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut escaped = String::with_capacity(value.len());
    for (index, ch) in chars.iter().copied().enumerate() {
        let edge_space = ch == ' ' && (index == 0 || index + 1 == chars.len());
        if edge_space
            || (ch == '#' && index == 0)
            || matches!(ch, ',' | '+' | '"' | '\\' | '<' | '>' | ';' | '=')
        {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(all(test, feature = "p12-keystore"))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity(name: &str) -> NewCredentialIdentity {
        NewCredentialIdentity {
            common_name: name.to_string(),
            organization: Some("Pulpit & Co".to_string()),
            email: Some("signer@example.test".to_string()),
            not_before: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            not_after: SystemTime::UNIX_EPOCH + Duration::from_secs(1_900_000_000),
        }
    }

    #[test]
    fn generated_container_round_trips_through_the_production_loader() {
        let generated = generate_self_signed_pkcs12(
            &identity("Ada Lovelace"),
            Zeroizing::new("correct horse battery staple".to_string()),
        )
        .expect("generate credential");

        assert!(
            generated.summary.subject.contains("Ada Lovelace"),
            "subject was {:?}",
            generated.summary.subject
        );
        assert_eq!(generated.summary.key_algorithm, "EC P-256");
        let loaded = super::super::load_pkcs12(
            &generated.bytes,
            Zeroizing::new("correct horse battery staple".to_string()),
        )
        .expect("reload generated PKCS#12");
        assert_eq!(
            loaded.summary().expect("summary").sha256_fingerprint,
            generated.summary.sha256_fingerprint
        );
    }

    #[test]
    fn distinguished_name_metacharacters_are_data_not_structure() {
        let generated = generate_self_signed_pkcs12(
            &identity("Lovelace, Ada + Byron"),
            Zeroizing::new("a sufficiently long passphrase".to_string()),
        )
        .expect("generate credential");
        assert!(generated.summary.subject.contains("Lovelace"));
        assert!(generated.summary.subject.contains("Ada"));
    }

    #[test]
    fn empty_name_and_backwards_validity_are_refused() {
        let password = || Zeroizing::new("passphrase".to_string());
        assert!(matches!(
            generate_self_signed_pkcs12(&identity("  "), password()),
            Err(CredentialGenerationError::MissingCommonName)
        ));
        let mut invalid = identity("Ada");
        invalid.not_after = invalid.not_before;
        assert!(matches!(
            generate_self_signed_pkcs12(&invalid, password()),
            Err(CredentialGenerationError::InvalidValidity)
        ));
    }
}
