#![forbid(unsafe_code)]
//! CMS `SignedData` construction. Per SPEC-signing.md §26, a detached
//! `SignedData` with exactly one `SignerInfo`, no encapsulated content, built
//! with the RustCrypto `cms`/`x509-cert`/`der` stack rather than by hand.
//!
//! The signature input is `DER(signedAttrs)` with the implicit `[0]` context
//! tag replaced by the universal `SET OF` tag (§26.1). Because
//! `cms::signed_data::SignedAttributes` is `x509_cert::attr::Attributes`, a
//! `SetOfVec<Attribute>`, its own `to_der()` already yields that universal
//! `SET OF` encoding (tag `0x31`) — the `[0] IMPLICIT` retagging only happens
//! when it is embedded as a field of `SignerInfo`. So the same value serves
//! both roles: sign `attrs.to_der()` directly, then hand `attrs` to the
//! `SignerInfo` struct, which retags it on encode.

use crate::sign::credential::{Credential, PublicKeyInfo};
use crate::sign::errors::SigningError;
use crate::sign::mechanism::{DigestAlgorithm, SigningMechanism};
use crate::sign::SigningProfile;

use cms::cert::IssuerAndSerialNumber;
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{
    CertificateSet, DigestAlgorithmIdentifiers, EncapsulatedContentInfo, SignedAttributes,
    SignedData, SignerIdentifier, SignerInfo, SignerInfos,
};
use der::asn1::{Any, AnyRef, ObjectIdentifier, OctetString, SetOfVec, UtcTime};
use der::{Encode, Sequence};
use sha2::{Digest as _, Sha256};
use spki::AlgorithmIdentifierOwned;
use x509_cert::attr::Attribute;
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::Certificate;

// --- OIDs used directly (§26.3, §26.1) ---------------------------------

const OID_ID_DATA: &str = "1.2.840.113549.1.7.1";
const OID_ID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
const OID_CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
const OID_MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
const OID_SIGNING_TIME: &str = "1.2.840.113549.1.9.5";
const OID_SIGNING_CERTIFICATE_V2: &str = "1.2.840.113549.1.9.16.2.47";
/// rsaEncryption. Acceptable as the CMS `signatureAlgorithm` for PKCS#1 v1.5
/// signatures regardless of the digest used (§26.2).
const OID_RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";

fn oid(s: &str) -> ObjectIdentifier {
    ObjectIdentifier::new(s).expect("static OID string is valid")
}

// --- RFC 5035 ESS SigningCertificateV2 (not present in x509-cert 0.2) ---

/// `SigningCertificateV2 ::= SEQUENCE { certs SEQUENCE OF ESSCertIDv2, ... }`
/// The `policies` field (RFC 5035) is never populated, so it is omitted from
/// this struct entirely rather than modelled as an always-`None` `Option`.
#[derive(Sequence)]
struct SigningCertificateV2 {
    certs: Vec<EssCertIdV2>,
}

/// `ESSCertIDv2 ::= SEQUENCE { hashAlgorithm AlgorithmIdentifier DEFAULT
/// {algorithm id-sha256}, certHash Hash, issuerSerial IssuerSerial OPTIONAL }`
///
/// This implementation always hashes the certificate with SHA-256, which is
/// the DEFAULT `hashAlgorithm` — per DER DEFAULT-value rules the field is
/// therefore always omitted (encoded as `None`), never explicitly restated.
#[derive(Sequence)]
struct EssCertIdV2 {
    #[asn1(optional = "true")]
    hash_algorithm: Option<AlgorithmIdentifierOwned>,
    cert_hash: OctetString,
    #[asn1(optional = "true")]
    issuer_serial: Option<IssuerSerial>,
}

/// `IssuerSerial ::= SEQUENCE { issuer GeneralNames, serialNumber
/// CertificateSerialNumber }`
#[derive(Sequence)]
struct IssuerSerial {
    issuer: Vec<GeneralName>,
    serial_number: SerialNumber,
}

/// `GeneralName ::= CHOICE { ..., directoryName [4] Name, ... }`. Only the
/// `directoryName` choice is needed: the issuer's distinguished name.
#[derive(der::Choice)]
enum GeneralName {
    #[asn1(context_specific = "4", tag_mode = "EXPLICIT", constructed = "true")]
    DirectoryName(Name),
}

fn signing_certificate_v2_attribute(cert: &Certificate) -> Result<Attribute, SigningError> {
    let cert_der = cert
        .to_der()
        .map_err(|e| SigningError::DerEncodingFailed(format!("certificate: {e}")))?;
    let cert_hash = Sha256::digest(&cert_der);

    let issuer_serial = IssuerSerial {
        issuer: vec![GeneralName::DirectoryName(
            cert.tbs_certificate.issuer.clone(),
        )],
        serial_number: cert.tbs_certificate.serial_number.clone(),
    };

    let ess_cert_id = EssCertIdV2 {
        hash_algorithm: None, // SHA-256 is the DEFAULT; always omitted (see doc comment).
        cert_hash: OctetString::new(cert_hash.to_vec())
            .map_err(|e| SigningError::DerEncodingFailed(format!("cert hash: {e}")))?,
        issuer_serial: Some(issuer_serial),
    };

    let signing_cert_v2 = SigningCertificateV2 {
        certs: vec![ess_cert_id],
    };

    attribute(OID_SIGNING_CERTIFICATE_V2, &signing_cert_v2)
}

/// Build a single-valued `Attribute` from an OID and a value encodable as
/// `Any`.
fn attribute<T>(oid_str: &str, value: &T) -> Result<Attribute, SigningError>
where
    T: der::Tagged + der::EncodeValue,
{
    let any = Any::encode_from(value)
        .map_err(|e| SigningError::DerEncodingFailed(format!("attribute value: {e}")))?;
    let mut values = SetOfVec::new();
    values
        .insert(any)
        .map_err(|e| SigningError::DerEncodingFailed(format!("attribute values: {e}")))?;
    Ok(Attribute {
        oid: oid(oid_str),
        values,
    })
}

// --- Signed attributes (§26.3) ------------------------------------------

/// Build the signed attributes: always content-type, message-digest and
/// signing-certificate-v2 (§26.3). `signing-time` is added only for
/// [`SigningProfile::AdbePkcs7Detached`], and only when `signing_time` (unix
/// seconds, caller-supplied — this crate never reads the clock) is `Some`.
/// PAdES (`EtsiCadesDetached`) never carries it.
pub fn build_signed_attributes(
    credential: &Credential,
    document_digest: &[u8],
    profile: SigningProfile,
    signing_time: Option<i64>,
) -> Result<SignedAttributes, SigningError> {
    let mut attrs: SignedAttributes = SetOfVec::new();

    let content_type_oid = oid(OID_ID_DATA);
    attrs
        .insert(attribute(OID_CONTENT_TYPE, &content_type_oid)?)
        .map_err(|e| SigningError::DerEncodingFailed(format!("content-type attr: {e}")))?;

    let digest_octets = OctetString::new(document_digest.to_vec())
        .map_err(|e| SigningError::DerEncodingFailed(format!("message digest: {e}")))?;
    attrs
        .insert(attribute(OID_MESSAGE_DIGEST, &digest_octets)?)
        .map_err(|e| SigningError::DerEncodingFailed(format!("message-digest attr: {e}")))?;

    attrs
        .insert(signing_certificate_v2_attribute(
            &credential.signer_certificate,
        )?)
        .map_err(|e| SigningError::DerEncodingFailed(format!("signing-cert-v2 attr: {e}")))?;

    if profile == SigningProfile::AdbePkcs7Detached {
        if let Some(unix_seconds) = signing_time {
            let duration = std::time::Duration::from_secs(unix_seconds.max(0) as u64);
            let utc_time = UtcTime::from_unix_duration(duration)
                .map_err(|e| SigningError::DerEncodingFailed(format!("signing-time: {e}")))?;
            attrs
                .insert(attribute(OID_SIGNING_TIME, &utc_time)?)
                .map_err(|e| SigningError::DerEncodingFailed(format!("signing-time attr: {e}")))?;
        }
    }

    Ok(attrs)
}

/// The bytes actually signed: `DER(signedAttrs)`, universal `SET OF` tag
/// (`0x31`), per §26.1.
pub fn signed_attrs_tbs(attrs: &SignedAttributes) -> Result<Vec<u8>, SigningError> {
    attrs
        .to_der()
        .map_err(|e| SigningError::DerEncodingFailed(format!("signed attrs: {e}")))
}

// --- Algorithm identifiers (§26.2) ---------------------------------------

fn digest_algorithm_identifier(digest: DigestAlgorithm) -> AlgorithmIdentifierOwned {
    AlgorithmIdentifierOwned {
        oid: oid(digest.oid()),
        parameters: Some(Any::null()),
    }
}

fn signature_algorithm_identifier(mechanism: SigningMechanism) -> AlgorithmIdentifierOwned {
    match mechanism {
        SigningMechanism::Rsa2048Sha256
        | SigningMechanism::Rsa3072Sha384
        | SigningMechanism::Rsa4096Sha512 => AlgorithmIdentifierOwned {
            oid: oid(OID_RSA_ENCRYPTION),
            parameters: Some(Any::null()),
        },
        SigningMechanism::EcdsaP256Sha256
        | SigningMechanism::EcdsaP384Sha384
        | SigningMechanism::EcdsaP521Sha512 => AlgorithmIdentifierOwned {
            oid: oid(mechanism.signature_algorithm_oid()),
            parameters: None,
        },
        SigningMechanism::Ed25519 => AlgorithmIdentifierOwned {
            oid: oid(mechanism.signature_algorithm_oid()),
            parameters: None,
        },
    }
}

// --- Assembly (§26.1) -----------------------------------------------------

fn is_self_signed(cert: &Certificate) -> bool {
    cert.tbs_certificate.issuer == cert.tbs_certificate.subject
}

/// Assemble the full `ContentInfo(SignedData(...))` DER given already-built
/// signed attributes and an already-computed signature. Used both by the
/// real signing path and by the dry run in `estimate_cms_size` (§23.5),
/// which supplies a placeholder signature.
pub fn assemble(
    credential: &Credential,
    mechanism: SigningMechanism,
    embed_roots: bool,
    signed_attrs: SignedAttributes,
    signature_bytes: Vec<u8>,
) -> Result<Vec<u8>, SigningError> {
    let digest_alg = digest_algorithm_identifier(mechanism.digest_algorithm());

    let mut cert_choices = Vec::new();
    cert_choices.push(cms::cert::CertificateChoices::Certificate(
        credential.signer_certificate.clone(),
    ));
    if embed_roots {
        for cert in &credential.cert_chain {
            cert_choices.push(cms::cert::CertificateChoices::Certificate(cert.clone()));
        }
    } else {
        for cert in &credential.cert_chain {
            if !is_self_signed(cert) {
                cert_choices.push(cms::cert::CertificateChoices::Certificate(cert.clone()));
            }
        }
    }
    let certificates = CertificateSet::try_from(cert_choices)
        .map_err(|e| SigningError::DerEncodingFailed(format!("certificate set: {e}")))?;

    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: credential.signer_certificate.tbs_certificate.issuer.clone(),
        serial_number: credential
            .signer_certificate
            .tbs_certificate
            .serial_number
            .clone(),
    });

    let signature = OctetString::new(signature_bytes)
        .map_err(|e| SigningError::DerEncodingFailed(format!("signature value: {e}")))?;

    let signer_info = SignerInfo {
        version: CmsVersion::V1,
        sid,
        digest_alg: digest_alg.clone(),
        signed_attrs: Some(signed_attrs),
        signature_algorithm: signature_algorithm_identifier(mechanism),
        signature,
        unsigned_attrs: None,
    };

    let signer_infos = SignerInfos::try_from(vec![signer_info])
        .map_err(|e| SigningError::DerEncodingFailed(format!("signer infos: {e}")))?;

    let mut digest_algorithms: DigestAlgorithmIdentifiers = SetOfVec::new();
    digest_algorithms
        .insert(digest_alg)
        .map_err(|e| SigningError::DerEncodingFailed(format!("digest algorithms: {e}")))?;

    let signed_data = SignedData {
        version: CmsVersion::V1,
        digest_algorithms,
        encap_content_info: EncapsulatedContentInfo {
            econtent_type: oid(OID_ID_DATA),
            econtent: None,
        },
        certificates: Some(certificates),
        crls: None,
        signer_infos,
    };

    let signed_data_der = signed_data
        .to_der()
        .map_err(|e| SigningError::DerEncodingFailed(format!("signed data: {e}")))?;
    let content = AnyRef::try_from(signed_data_der.as_slice())
        .map_err(|e| SigningError::DerEncodingFailed(format!("signed data any: {e}")))?;

    let content_info = ContentInfo {
        content_type: oid(OID_ID_SIGNED_DATA),
        content: Any::from(content),
    };

    content_info
        .to_der()
        .map_err(|e| SigningError::DerEncodingFailed(format!("content info: {e}")))
}

// --- Signature primitives (§26.2) -----------------------------------------

/// Sign `tbs` (the SET OF-tagged signed attributes DER) with the
/// credential's private key, per the mechanism's rules.
pub fn sign_tbs(
    credential: &Credential,
    tbs: &[u8],
    mechanism: SigningMechanism,
) -> Result<Vec<u8>, SigningError> {
    match mechanism {
        SigningMechanism::Rsa2048Sha256
        | SigningMechanism::Rsa3072Sha384
        | SigningMechanism::Rsa4096Sha512 => sign_with_rsa(credential, tbs, mechanism),
        SigningMechanism::EcdsaP256Sha256 => sign_with_ecdsa_p256(credential, tbs),
        SigningMechanism::EcdsaP384Sha384 => sign_with_ecdsa_p384(credential, tbs),
        SigningMechanism::EcdsaP521Sha512 => Err(SigningError::UnsupportedKeyAlgorithm {
            algorithm: "P-521 key parsing deferred (see mechanism.rs)".to_string(),
        }),
        SigningMechanism::Ed25519 => sign_with_ed25519(credential, tbs),
    }
}

fn sign_with_rsa(
    credential: &Credential,
    tbs: &[u8],
    mechanism: SigningMechanism,
) -> Result<Vec<u8>, SigningError> {
    use pkcs8::DecodePrivateKey;
    use rsa::pkcs1v15::SigningKey;
    use rsa::RsaPrivateKey;
    use sha2::{Sha256, Sha384, Sha512};
    use signature::Signer;

    let pkey_der = credential.private_key_der();
    let private_key = RsaPrivateKey::from_pkcs8_der(pkey_der).map_err(|e| {
        SigningError::SignatureOperationFailed(format!("Failed to parse RSA key: {e}"))
    })?;

    // `SigningKey<D>` adds the PKCS#1 `DigestInfo` prefix and hashes
    // internally; the raw SET OF bytes go in unhashed.
    let signature_der: Vec<u8> = match mechanism.digest_algorithm() {
        DigestAlgorithm::Sha256 => {
            let key = SigningKey::<Sha256>::new(private_key);
            let sig: rsa::pkcs1v15::Signature = key.try_sign(tbs).map_err(|e| {
                SigningError::SignatureOperationFailed(format!("RSA signature failed: {e}"))
            })?;
            Vec::from(Box::<[u8]>::from(sig))
        }
        DigestAlgorithm::Sha384 => {
            let key = SigningKey::<Sha384>::new(private_key);
            let sig: rsa::pkcs1v15::Signature = key.try_sign(tbs).map_err(|e| {
                SigningError::SignatureOperationFailed(format!("RSA signature failed: {e}"))
            })?;
            Vec::from(Box::<[u8]>::from(sig))
        }
        DigestAlgorithm::Sha512 => {
            let key = SigningKey::<Sha512>::new(private_key);
            let sig: rsa::pkcs1v15::Signature = key.try_sign(tbs).map_err(|e| {
                SigningError::SignatureOperationFailed(format!("RSA signature failed: {e}"))
            })?;
            Vec::from(Box::<[u8]>::from(sig))
        }
    };

    Ok(signature_der)
}

fn sign_with_ecdsa_p256(credential: &Credential, tbs: &[u8]) -> Result<Vec<u8>, SigningError> {
    use pkcs8::DecodePrivateKey;
    use signature::Signer;

    let pkey_der = credential.private_key_der();
    let key = p256::ecdsa::SigningKey::from_pkcs8_der(pkey_der).map_err(|e| {
        SigningError::SignatureOperationFailed(format!("Failed to parse P-256 key: {e}"))
    })?;

    // The key hashes `tbs` internally (SHA-256); passing raw bytes avoids
    // double-hashing.
    let sig: p256::ecdsa::Signature = key.sign(tbs);
    Ok(sig.to_der().as_bytes().to_vec())
}

fn sign_with_ecdsa_p384(credential: &Credential, tbs: &[u8]) -> Result<Vec<u8>, SigningError> {
    use pkcs8::DecodePrivateKey;
    use signature::Signer;

    let pkey_der = credential.private_key_der();
    let key = p384::ecdsa::SigningKey::from_pkcs8_der(pkey_der).map_err(|e| {
        SigningError::SignatureOperationFailed(format!("Failed to parse P-384 key: {e}"))
    })?;

    let sig: p384::ecdsa::Signature = key.sign(tbs);
    Ok(sig.to_der().as_bytes().to_vec())
}

fn sign_with_ed25519(credential: &Credential, tbs: &[u8]) -> Result<Vec<u8>, SigningError> {
    use ed25519_dalek::pkcs8::DecodePrivateKey;
    use ed25519_dalek::SigningKey;
    use signature::Signer;

    let pkey_der = credential.private_key_der();
    let signing_key = SigningKey::from_pkcs8_der(pkey_der).map_err(|e| {
        SigningError::SignatureOperationFailed(format!("Failed to parse Ed25519 key: {e}"))
    })?;

    // Ed25519 signs the raw message directly (SHA-512 is internal to the
    // algorithm, not a pre-hash the caller performs).
    let signature = signing_key.sign(tbs);
    Ok(signature.to_bytes().to_vec())
}

// --- Size estimation (§23.5) ----------------------------------------------

/// Worst-case DER-encoded signature length for a mechanism, given the
/// signer's actual key size where that matters (RSA: modulus bytes; ECDSA:
/// `2 * coordinate_bytes + 9` covers the SEQUENCE/INTEGER overhead and a
/// possible leading zero on each of `r` and `s`; Ed25519: fixed 64 bytes).
pub fn max_signature_len(mechanism: SigningMechanism, key_info: &PublicKeyInfo) -> usize {
    match mechanism {
        SigningMechanism::Rsa2048Sha256
        | SigningMechanism::Rsa3072Sha384
        | SigningMechanism::Rsa4096Sha512 => {
            let bits = key_info.bits().unwrap_or(2048);
            bits.div_ceil(8)
        }
        SigningMechanism::EcdsaP256Sha256 => 2 * 32 + 9,
        SigningMechanism::EcdsaP384Sha384 => 2 * 48 + 9,
        SigningMechanism::EcdsaP521Sha512 => 2 * 66 + 9,
        SigningMechanism::Ed25519 => 64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_attrs_tbs_starts_with_universal_set_of_tag() {
        // A minimal SetOfVec<Attribute> with one attribute is enough to
        // exercise the tag; the exact content doesn't matter for this
        // structural assertion.
        let mut attrs: SignedAttributes = SetOfVec::new();
        let content_type_oid = oid(OID_ID_DATA);
        attrs
            .insert(attribute(OID_CONTENT_TYPE, &content_type_oid).unwrap())
            .unwrap();

        let tbs = signed_attrs_tbs(&attrs).unwrap();
        assert_eq!(
            tbs[0], 0x31,
            "signed attrs DER must start with universal SET OF tag (0x31), not [0] IMPLICIT"
        );
    }
}
