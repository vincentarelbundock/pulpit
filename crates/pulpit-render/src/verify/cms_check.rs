#![forbid(unsafe_code)]
//! Cryptographic verification of PDF signatures, per SPEC-signing.md §28.3
//! and the status model of §28.5.
//!
//! This module deliberately does **not** depend on `crate::sign` (§22.2
//! knowledge separation): the small set of OIDs and the ESS
//! `SigningCertificateV2` shape it needs are restated here rather than
//! imported from the producing side. A verifier that shares its constants
//! with the signer cannot catch the signer being wrong.
//!
//! Two things are checked and reported separately (§28.3):
//!
//! - `intact` — the `message-digest` signed attribute equals the digest
//!   recomputed over the declared `/ByteRange`. The covered bytes are
//!   unchanged.
//! - `valid` — the signature over `DER(signedAttrs)` verifies against the
//!   public key of the **embedded** certificate. No chain is built, no trust
//!   store is consulted; `identity` is always `NotVerified`.

use crate::verify::{
    discover_signatures, ByteRange, ContentsExtent, MdpPerm, Result, RevisionMap,
    SignatureCoverage, StructuralReport,
};

use cms::content_info::ContentInfo;
use cms::signed_data::{SignedData, SignerIdentifier, SignerInfo};
use der::asn1::{ObjectIdentifier, OctetString};
use der::{Decode, Encode, Sequence};
use sha2::{Digest as _, Sha256, Sha384, Sha512};
use spki::AlgorithmIdentifierOwned;
use x509_cert::attr::Attribute;
use x509_cert::serial_number::SerialNumber;
use x509_cert::Certificate;

// --- OIDs (restated locally; see the module comment) ---------------------

const OID_ID_DATA: &str = "1.2.840.113549.1.7.1";
const OID_CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
const OID_MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
const OID_SIGNING_TIME: &str = "1.2.840.113549.1.9.5";
const OID_SIGNING_CERTIFICATE_V2: &str = "1.2.840.113549.1.9.16.2.47";
const OID_CMS_ALGORITHM_PROTECTION: &str = "1.2.840.113549.1.9.52";

const OID_SHA1: &str = "1.3.14.3.2.26";
const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";

const OID_RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_ED25519: &str = "1.3.101.112";
const OID_PRIME256V1: &str = "1.2.840.10045.3.1.7";
const OID_SECP384R1: &str = "1.3.132.0.34";
const OID_SUBJECT_KEY_IDENTIFIER: &str = "2.5.29.14";

fn oid(s: &str) -> ObjectIdentifier {
    ObjectIdentifier::new(s).expect("static OID string is valid")
}

// --- Status model (§28.5) -------------------------------------------------

/// Summary of a certificate as embedded in the CMS. Nothing here is
/// validated; it is transcription for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateSummary {
    pub subject: String,
    pub issuer: String,
    /// Serial number, uppercase hex.
    pub serial: String,
    /// `notBefore`, unix seconds.
    pub not_before: i64,
    /// `notAfter`, unix seconds.
    pub not_after: i64,
    /// SHA-256 over the DER encoding of the certificate, lowercase hex.
    pub sha256_fingerprint: String,
}

/// This release has exactly one inhabitant (§20.3, §28.5). The enum exists so
/// that adding chain validation later is an additive change, not a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityAssurance {
    NotVerified { reason: &'static str },
}

/// The signature profile, as declared by the signature dictionary's
/// `/SubFilter` (§21.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadesProfile {
    /// `/adbe.pkcs7.detached`
    AdbePkcs7Detached,
    /// `/ETSI.CAdES.detached`
    EtsiCadesDetached,
}

/// A descriptive finding about an algorithm choice. Reported, not judged
/// (§28.3 step 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlgorithmFinding {
    /// A digest algorithm that is no longer collision resistant was declared.
    WeakDigest { algorithm: String },
}

/// The full per-signature status of §28.5.
#[derive(Debug, Clone)]
pub struct SignatureStatus {
    pub field_name: String,
    pub signer_subject: String,
    pub signer_cert: CertificateSummary,
    /// Certificates as embedded in the CMS; unvalidated, in encounter order.
    pub cert_chain: Vec<CertificateSummary>,
    pub coverage: SignatureCoverage,
    pub intact: bool,
    pub valid: bool,
    pub later_revisions: bool,
    /// Displayed, never enforced (§28.4).
    pub declared_docmdp: Option<MdpPerm>,
    /// Unix seconds, from `/M` or the `signing-time` signed attribute.
    pub claimed_time: Option<i64>,
    /// From a timestamp token. Deferred to B-T; always `None` here.
    pub attested_time: Option<()>,
    pub algorithm_findings: Vec<AlgorithmFinding>,
    pub identity: IdentityAssurance,
    pub profile: Option<PadesProfile>,
    /// The declared digest algorithm, for display (§28.3 step 7).
    pub digest_algorithm: String,
    /// The declared signature algorithm, for display (§28.3 step 7).
    pub signature_algorithm: String,
}

/// The outcome of verifying one discovered signature.
#[derive(Debug, Clone)]
pub enum SignatureVerification {
    /// Every check ran; read `intact` and `valid` for the result.
    Checked(Box<SignatureStatus>),
    /// The signature could not be checked at all, or is structurally
    /// disqualified (§28.2, §28.3). Presented as broken.
    Broken { field_name: String, reason: String },
}

impl SignatureVerification {
    fn broken(field_name: &str, reason: impl Into<String>) -> Self {
        SignatureVerification::Broken {
            field_name: field_name.to_string(),
            reason: reason.into(),
        }
    }
}

// --- Digest algorithms ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Digest {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Digest {
    fn from_oid(o: &ObjectIdentifier) -> Option<Digest> {
        let s = o.to_string();
        match s.as_str() {
            OID_SHA1 => Some(Digest::Sha1),
            OID_SHA256 => Some(Digest::Sha256),
            OID_SHA384 => Some(Digest::Sha384),
            OID_SHA512 => Some(Digest::Sha512),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Digest::Sha1 => "SHA-1",
            Digest::Sha256 => "SHA-256",
            Digest::Sha384 => "SHA-384",
            Digest::Sha512 => "SHA-512",
        }
    }

    fn hash(self, parts: &[&[u8]]) -> Vec<u8> {
        fn feed<D: sha2::Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut h = D::new();
            for p in parts {
                h.update(p);
            }
            h.finalize().to_vec()
        }
        match self {
            Digest::Sha1 => {
                let mut h = sha1::Sha1::new();
                for p in parts {
                    sha1::digest::Update::update(&mut h, p);
                }
                sha1::Digest::finalize(h).to_vec()
            }
            Digest::Sha256 => feed::<Sha256>(parts),
            Digest::Sha384 => feed::<Sha384>(parts),
            Digest::Sha512 => feed::<Sha512>(parts),
        }
    }
}

// --- ESS SigningCertificateV2 (RFC 5035), decode side ---------------------

#[derive(Sequence)]
struct SigningCertificateV2 {
    certs: Vec<EssCertIdV2>,
}

#[derive(Sequence)]
struct EssCertIdV2 {
    #[asn1(optional = "true")]
    hash_algorithm: Option<AlgorithmIdentifierOwned>,
    cert_hash: OctetString,
    #[asn1(optional = "true")]
    issuer_serial: Option<der::asn1::Any>,
}

/// RFC 6211 `CMSAlgorithmProtection`.
#[derive(Sequence)]
struct CmsAlgorithmProtection {
    digest_algorithm: AlgorithmIdentifierOwned,
    #[asn1(context_specific = "1", tag_mode = "IMPLICIT", optional = "true")]
    signature_algorithm: Option<AlgorithmIdentifierOwned>,
}

// --- /Contents extraction (§23.4) ----------------------------------------

/// Hex-decode the interior of the `/Contents` reservation and truncate to the
/// DER-declared length. The reservation is padded with zero bytes after the
/// DER (§23.4); that padding is normal and must not be an error.
pub(crate) fn extract_cms_der(
    bytes: &[u8],
    extent: &ContentsExtent,
) -> std::result::Result<Vec<u8>, String> {
    let start = extent.c_start as usize;
    let end = extent.c_end as usize;
    if end > bytes.len() || start >= end {
        return Err("/Contents extent lies outside the file".to_string());
    }
    let slice = &bytes[start..end];
    if slice.first() != Some(&b'<') || slice.last() != Some(&b'>') {
        return Err("/Contents is not a hex string".to_string());
    }
    let interior = &slice[1..slice.len() - 1];

    let mut decoded = Vec::with_capacity(interior.len() / 2);
    let mut high: Option<u8> = None;
    for &b in interior {
        if b.is_ascii_whitespace() {
            continue;
        }
        let nibble = match (b as char).to_digit(16) {
            Some(n) => n as u8,
            None => return Err("/Contents contains a non-hex character".to_string()),
        };
        match high.take() {
            None => high = Some(nibble),
            Some(h) => decoded.push((h << 4) | nibble),
        }
    }
    if let Some(h) = high {
        // An odd number of hex digits: the last one is an implicit low nibble
        // of zero, per the PDF hex-string rule.
        decoded.push(h << 4);
    }

    let declared =
        der_total_len(&decoded).ok_or_else(|| "CMS DER header is truncated".to_string())?;
    if declared > decoded.len() {
        return Err("CMS DER is longer than the /Contents reservation".to_string());
    }
    decoded.truncate(declared);
    Ok(decoded)
}

/// Total length (header + content) of the DER value at the start of `buf`.
fn der_total_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 2 {
        return None;
    }
    let len_byte = buf[1];
    if len_byte < 0x80 {
        return Some(2 + len_byte as usize);
    }
    let n = (len_byte & 0x7f) as usize;
    if n == 0 || n > 4 || buf.len() < 2 + n {
        return None;
    }
    let mut len: usize = 0;
    for &b in &buf[2..2 + n] {
        len = len.checked_mul(256)?.checked_add(b as usize)?;
    }
    Some(2 + n + len)
}

// --- PDF date parsing -----------------------------------------------------

/// Parse a PDF date string — `D:YYYYMMDDHHmmSSOHH'mm'` and every legal
/// truncation of it — into unix seconds. Written here rather than pulled in
/// with a date crate: the workspace has none, and the grammar is five fields
/// and an offset.
pub(crate) fn parse_pdf_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.strip_prefix("D:").unwrap_or(s);
    let digits: Vec<char> = s.chars().collect();

    fn num(d: &[char], at: usize, len: usize, default: i64) -> Option<i64> {
        if at + len > d.len() {
            return Some(default);
        }
        let chunk: String = d[at..at + len].iter().collect();
        if !chunk.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        chunk.parse().ok()
    }

    let year = num(&digits, 0, 4, -1)?;
    if year < 0 {
        return None;
    }
    let month = num(&digits, 4, 2, 1)?;
    let day = num(&digits, 6, 2, 1)?;
    let hour = num(&digits, 8, 2, 0)?;
    let minute = num(&digits, 10, 2, 0)?;
    let second = num(&digits, 12, 2, 0)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }

    let mut ts = days_from_civil(year, month as u32, day as u32) * 86_400
        + hour * 3600
        + minute * 60
        + second;

    // Optional UT offset: O HH ' mm '
    if digits.len() > 14 {
        let sign = digits[14];
        let sign = match sign {
            '+' => 1,
            '-' => -1,
            'Z' => 0,
            _ => return Some(ts),
        };
        let rest: Vec<char> = digits[15..]
            .iter()
            .copied()
            .filter(|c| c.is_ascii_digit())
            .collect();
        let off_h = num(&rest, 0, 2, 0)?;
        let off_m = num(&rest, 2, 2, 0)?;
        ts -= sign * (off_h * 3600 + off_m * 60);
    }
    Some(ts)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// --- Certificate helpers --------------------------------------------------

fn summarize(cert: &Certificate) -> std::result::Result<CertificateSummary, String> {
    let der_bytes = cert.to_der().map_err(|e| format!("certificate DER: {e}"))?;
    Ok(CertificateSummary {
        subject: cert.tbs_certificate.subject.to_string(),
        issuer: cert.tbs_certificate.issuer.to_string(),
        serial: hex::encode_upper(cert.tbs_certificate.serial_number.as_bytes()),
        not_before: cert
            .tbs_certificate
            .validity
            .not_before
            .to_unix_duration()
            .as_secs() as i64,
        not_after: cert
            .tbs_certificate
            .validity
            .not_after
            .to_unix_duration()
            .as_secs() as i64,
        sha256_fingerprint: hex::encode(Sha256::digest(&der_bytes)),
    })
}

fn embedded_certificates(signed_data: &SignedData) -> Vec<Certificate> {
    let mut out = Vec::new();
    if let Some(set) = &signed_data.certificates {
        for choice in set.0.iter() {
            if let cms::cert::CertificateChoices::Certificate(c) = choice {
                out.push(c.clone());
            }
        }
    }
    out
}

fn subject_key_identifier(cert: &Certificate) -> Option<Vec<u8>> {
    let target = oid(OID_SUBJECT_KEY_IDENTIFIER);
    let exts = cert.tbs_certificate.extensions.as_ref()?;
    let ext = exts.iter().find(|e| e.extn_id == target)?;
    // The extension value is a DER OCTET STRING wrapping the key identifier.
    OctetString::from_der(ext.extn_value.as_bytes())
        .ok()
        .map(|o| o.as_bytes().to_vec())
}

fn select_signer_certificate<'a>(
    sid: &SignerIdentifier,
    certs: &'a [Certificate],
) -> Option<&'a Certificate> {
    match sid {
        SignerIdentifier::IssuerAndSerialNumber(ias) => certs.iter().find(|c| {
            c.tbs_certificate.issuer == ias.issuer
                && serial_eq(&c.tbs_certificate.serial_number, &ias.serial_number)
        }),
        SignerIdentifier::SubjectKeyIdentifier(skid) => {
            let want = skid.0.as_bytes();
            certs
                .iter()
                .find(|c| subject_key_identifier(c).as_deref() == Some(want))
        }
    }
}

fn serial_eq(a: &SerialNumber, b: &SerialNumber) -> bool {
    a.as_bytes() == b.as_bytes()
}

// --- Signed attribute access ---------------------------------------------

fn find_attribute<'a>(signer: &'a SignerInfo, oid_str: &str) -> Option<&'a Attribute> {
    let target = oid(oid_str);
    signer
        .signed_attrs
        .as_ref()?
        .iter()
        .find(|a| a.oid == target)
}

fn single_value(attr: &Attribute) -> Option<&der::asn1::Any> {
    let mut it = attr.values.iter();
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(first)
}

// --- Signature verification (§28.3 step 6) -------------------------------

fn verify_signature(cert: &Certificate, tbs: &[u8], signature: &[u8], digest: Digest) -> bool {
    let spki = &cert.tbs_certificate.subject_public_key_info;
    let alg = spki.algorithm.oid.to_string();
    match alg.as_str() {
        OID_RSA_ENCRYPTION => verify_rsa(spki, tbs, signature, digest),
        OID_EC_PUBLIC_KEY => {
            let curve = match spki.algorithm.parameters.as_ref() {
                Some(p) => match p.decode_as::<ObjectIdentifier>() {
                    Ok(c) => c.to_string(),
                    Err(_) => return false,
                },
                None => return false,
            };
            let point = spki.subject_public_key.raw_bytes();
            match curve.as_str() {
                OID_PRIME256V1 => {
                    use p256::ecdsa::signature::Verifier;
                    let key = match p256::ecdsa::VerifyingKey::from_sec1_bytes(point) {
                        Ok(k) => k,
                        Err(_) => return false,
                    };
                    match p256::ecdsa::Signature::from_der(signature) {
                        Ok(sig) => key.verify(tbs, &sig).is_ok(),
                        Err(_) => false,
                    }
                }
                OID_SECP384R1 => {
                    use p384::ecdsa::signature::Verifier;
                    let key = match p384::ecdsa::VerifyingKey::from_sec1_bytes(point) {
                        Ok(k) => k,
                        Err(_) => return false,
                    };
                    match p384::ecdsa::Signature::from_der(signature) {
                        Ok(sig) => key.verify(tbs, &sig).is_ok(),
                        Err(_) => false,
                    }
                }
                _ => false,
            }
        }
        OID_ED25519 => {
            use ed25519_dalek::{Signature, Verifier, VerifyingKey};
            let point = spki.subject_public_key.raw_bytes();
            let bytes: [u8; 32] = match point.try_into() {
                Ok(b) => b,
                Err(_) => return false,
            };
            let key = match VerifyingKey::from_bytes(&bytes) {
                Ok(k) => k,
                Err(_) => return false,
            };
            let sig: [u8; 64] = match signature.try_into() {
                Ok(s) => s,
                Err(_) => return false,
            };
            key.verify(tbs, &Signature::from_bytes(&sig)).is_ok()
        }
        _ => false,
    }
}

fn verify_rsa(
    spki: &spki::SubjectPublicKeyInfoOwned,
    tbs: &[u8],
    signature: &[u8],
    digest: Digest,
) -> bool {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::pkcs8::DecodePublicKey;
    use rsa::RsaPublicKey;
    use signature::Verifier;

    let spki_der = match spki.to_der() {
        Ok(d) => d,
        Err(_) => return false,
    };
    let key = match RsaPublicKey::from_public_key_der(&spki_der) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let sig = match Signature::try_from(signature) {
        Ok(s) => s,
        Err(_) => return false,
    };
    match digest {
        Digest::Sha1 => VerifyingKey::<sha1::Sha1>::new(key)
            .verify(tbs, &sig)
            .is_ok(),
        Digest::Sha256 => VerifyingKey::<Sha256>::new(key).verify(tbs, &sig).is_ok(),
        Digest::Sha384 => VerifyingKey::<Sha384>::new(key).verify(tbs, &sig).is_ok(),
        Digest::Sha512 => VerifyingKey::<Sha512>::new(key).verify(tbs, &sig).is_ok(),
    }
}

// --- The checks (§28.3) ---------------------------------------------------

/// Run the §28.3 cryptographic checks for one signature, given the file bytes
/// and its structural report. Coverage is *not* consulted here; see
/// [`verify_signatures`], which applies the §28.2 short circuit first.
pub fn check_signature(bytes: &[u8], report: &StructuralReport) -> SignatureVerification {
    let field = report.field_name.as_str();

    let cms_der = match extract_cms_der(bytes, &report.contents_extent) {
        Ok(d) => d,
        Err(e) => return SignatureVerification::broken(field, e),
    };

    let content_info = match ContentInfo::from_der(&cms_der) {
        Ok(ci) => ci,
        Err(e) => return SignatureVerification::broken(field, format!("malformed CMS: {e}")),
    };
    let signed_data: SignedData = match content_info.content.decode_as() {
        Ok(sd) => sd,
        Err(e) => {
            return SignatureVerification::broken(field, format!("malformed SignedData: {e}"))
        }
    };

    let signers: Vec<&SignerInfo> = signed_data.signer_infos.0.iter().collect();
    if signers.len() != 1 {
        return SignatureVerification::broken(
            field,
            format!("unsupported signature structure: {} signers", signers.len()),
        );
    }
    let signer = signers[0];

    let certs = embedded_certificates(&signed_data);
    let signer_cert = match select_signer_certificate(&signer.sid, &certs) {
        Some(c) => c.clone(),
        None => {
            return SignatureVerification::broken(
                field,
                "signer certificate not present in the signature",
            )
        }
    };

    // Step 1: recompute the document digest over the declared /ByteRange.
    let digest_alg = match Digest::from_oid(&signer.digest_alg.oid) {
        Some(d) => d,
        None => {
            return SignatureVerification::broken(
                field,
                format!("unsupported digest algorithm {}", signer.digest_alg.oid),
            )
        }
    };
    let spans = match byte_range_spans(bytes, &report.byte_range) {
        Ok(s) => s,
        Err(e) => return SignatureVerification::broken(field, e),
    };
    let recomputed = digest_alg.hash(&[spans.0, spans.1]);

    let mut findings = Vec::new();
    if digest_alg == Digest::Sha1 {
        findings.push(AlgorithmFinding::WeakDigest {
            algorithm: digest_alg.name().to_string(),
        });
    }

    // Step 2: content-type must be id-data.
    match find_attribute(signer, OID_CONTENT_TYPE)
        .and_then(single_value)
        .and_then(|v| v.decode_as::<ObjectIdentifier>().ok())
    {
        Some(ct) if ct == oid(OID_ID_DATA) => {}
        Some(ct) => {
            return SignatureVerification::broken(
                field,
                format!("content-type attribute is {ct}, expected id-data"),
            )
        }
        None => {
            return SignatureVerification::broken(field, "content-type signed attribute is missing")
        }
    }

    // Step 3: message-digest must equal the recomputed digest -> intact.
    let claimed_digest = match find_attribute(signer, OID_MESSAGE_DIGEST)
        .and_then(single_value)
        .and_then(|v| v.decode_as::<OctetString>().ok())
    {
        Some(d) => d.as_bytes().to_vec(),
        None => {
            return SignatureVerification::broken(
                field,
                "message-digest signed attribute is missing",
            )
        }
    };
    let intact = claimed_digest == recomputed;

    // Step 4: cms-algorithm-protection, if present, must agree.
    if let Some(attr) = find_attribute(signer, OID_CMS_ALGORITHM_PROTECTION) {
        let parsed = single_value(attr).and_then(|v| v.decode_as::<CmsAlgorithmProtection>().ok());
        match parsed {
            None => {
                return SignatureVerification::broken(
                    field,
                    "cms-algorithm-protection attribute is malformed",
                )
            }
            Some(cap) => {
                if cap.digest_algorithm.oid != signer.digest_alg.oid {
                    return SignatureVerification::broken(
                        field,
                        "cms-algorithm-protection disagrees with the declared digest algorithm",
                    );
                }
                if let Some(sa) = cap.signature_algorithm {
                    if sa.oid != signer.signature_algorithm.oid {
                        return SignatureVerification::broken(
                            field,
                            "cms-algorithm-protection disagrees with the declared signature algorithm",
                        );
                    }
                }
            }
        }
    }

    // Step 5: signing-certificate-v2, if present, must match the signer cert.
    if let Some(attr) = find_attribute(signer, OID_SIGNING_CERTIFICATE_V2) {
        match single_value(attr).and_then(|v| v.decode_as::<SigningCertificateV2>().ok()) {
            None => {
                return SignatureVerification::broken(
                    field,
                    "signing-certificate-v2 attribute is malformed",
                )
            }
            Some(scv2) => {
                let ess = match scv2.certs.first() {
                    Some(e) => e,
                    None => {
                        return SignatureVerification::broken(
                            field,
                            "signing-certificate-v2 attribute names no certificate",
                        )
                    }
                };
                let hash_alg = match &ess.hash_algorithm {
                    None => Digest::Sha256, // the RFC 5035 DEFAULT
                    Some(ai) => match Digest::from_oid(&ai.oid) {
                        Some(d) => d,
                        None => {
                            return SignatureVerification::broken(
                                field,
                                format!(
                                    "signing-certificate-v2 declares unsupported hash {}",
                                    ai.oid
                                ),
                            )
                        }
                    },
                };
                let cert_der = match signer_cert.to_der() {
                    Ok(d) => d,
                    Err(e) => {
                        return SignatureVerification::broken(
                            field,
                            format!("signer certificate could not be re-encoded: {e}"),
                        )
                    }
                };
                if hash_alg.hash(&[&cert_der]) != ess.cert_hash.as_bytes() {
                    return SignatureVerification::broken(
                        field,
                        "signing-certificate-v2 does not match the signer certificate",
                    );
                }
            }
        }
    }

    // Step 6: verify over DER(signedAttrs) re-tagged as a universal SET OF.
    // `SignedAttributes` is a `SetOfVec`, so its own `to_der()` already emits
    // the universal tag; the `[0] IMPLICIT` retagging happens only inside
    // `SignerInfo`.
    let valid = match signer.signed_attrs.as_ref().map(|a| a.to_der()) {
        Some(Ok(tbs)) => {
            verify_signature(&signer_cert, &tbs, signer.signature.as_bytes(), digest_alg)
        }
        _ => false,
    };

    // Claimed time: /M if the dictionary carried one, else signing-time.
    let claimed_time = report
        .mod_date
        .as_deref()
        .and_then(parse_pdf_date)
        .or_else(|| signing_time(signer));

    let signer_summary = match summarize(&signer_cert) {
        Ok(s) => s,
        Err(e) => return SignatureVerification::broken(field, e),
    };
    let mut chain = Vec::new();
    for c in &certs {
        match summarize(c) {
            Ok(s) => chain.push(s),
            Err(e) => return SignatureVerification::broken(field, e),
        }
    }

    SignatureVerification::Checked(Box::new(SignatureStatus {
        field_name: report.field_name.clone(),
        signer_subject: signer_summary.subject.clone(),
        signer_cert: signer_summary,
        cert_chain: chain,
        coverage: report.coverage,
        intact,
        valid,
        later_revisions: report.later_revisions,
        declared_docmdp: report.declared_docmdp,
        claimed_time,
        attested_time: None,
        algorithm_findings: findings,
        identity: IdentityAssurance::NotVerified {
            reason: "pulpit does not perform certificate path validation",
        },
        profile: profile_from_subfilter(report.sub_filter.as_deref()),
        digest_algorithm: digest_alg.name().to_string(),
        signature_algorithm: signer.signature_algorithm.oid.to_string(),
    }))
}

fn profile_from_subfilter(sub_filter: Option<&str>) -> Option<PadesProfile> {
    match sub_filter? {
        "adbe.pkcs7.detached" => Some(PadesProfile::AdbePkcs7Detached),
        "ETSI.CAdES.detached" => Some(PadesProfile::EtsiCadesDetached),
        _ => None,
    }
}

fn signing_time(signer: &SignerInfo) -> Option<i64> {
    let any = single_value(find_attribute(signer, OID_SIGNING_TIME)?)?;
    if let Ok(t) = any.decode_as::<der::asn1::UtcTime>() {
        return Some(t.to_unix_duration().as_secs() as i64);
    }
    if let Ok(t) = any.decode_as::<der::asn1::GeneralizedTime>() {
        return Some(t.to_unix_duration().as_secs() as i64);
    }
    None
}

fn byte_range_spans<'a>(
    bytes: &'a [u8],
    br: &ByteRange,
) -> std::result::Result<(&'a [u8], &'a [u8]), String> {
    let end1 =
        br.z.checked_add(br.len1)
            .ok_or_else(|| "/ByteRange overflows".to_string())?;
    let end2 = br
        .start2
        .checked_add(br.len2)
        .ok_or_else(|| "/ByteRange overflows".to_string())?;
    if end1 > bytes.len() as u64 || end2 > bytes.len() as u64 || br.start2 < end1 {
        return Err("/ByteRange lies outside the file".to_string());
    }
    Ok((
        &bytes[br.z as usize..end1 as usize],
        &bytes[br.start2 as usize..end2 as usize],
    ))
}

// --- Entry point ----------------------------------------------------------

/// Discover every signature, classify its coverage (§28.2) and run the
/// cryptographic checks (§28.3).
///
/// Coverage below `EntireRevision` short-circuits to `Broken`: "anything below
/// `EntireRevision` is presented as a broken signature regardless of what the
/// cryptography says" (§28.2), so the cryptography is not run at all.
pub fn verify_signatures(bytes: &[u8]) -> Result<Vec<SignatureVerification>> {
    let revisions = RevisionMap::build(bytes)?;
    let reports = discover_signatures(bytes, &revisions)?;
    Ok(reports
        .iter()
        .map(|report| match report.coverage {
            SignatureCoverage::Unclear => SignatureVerification::broken(
                &report.field_name,
                "signature coverage is unclear: the byte range does not describe this file",
            ),
            SignatureCoverage::ContiguousBlockFromStart => SignatureVerification::broken(
                &report.field_name,
                "signature does not cover its own revision's cross-reference table",
            ),
            SignatureCoverage::EntireRevision | SignatureCoverage::EntireFile => {
                check_signature(bytes, report)
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_date_full_form_with_offset() {
        // 2024-08-20 22:00:00 UTC
        assert_eq!(
            parse_pdf_date("D:20240820220000+00'00'"),
            Some(1_724_191_200)
        );
        // Same instant expressed as 23:00 at +01'00'.
        assert_eq!(
            parse_pdf_date("D:20240820230000+01'00'"),
            Some(1_724_191_200)
        );
        // And as 21:00 at -01'00'.
        assert_eq!(
            parse_pdf_date("D:20240820210000-01'00'"),
            Some(1_724_191_200)
        );
    }

    #[test]
    fn pdf_date_truncated_forms_default_to_start_of_period() {
        assert_eq!(parse_pdf_date("D:2024"), Some(1_704_067_200)); // 2024-01-01T00:00Z
        assert_eq!(parse_pdf_date("D:20240820"), Some(1_724_112_000));
        assert_eq!(parse_pdf_date("20240820"), Some(1_724_112_000));
    }

    #[test]
    fn pdf_date_rejects_nonsense() {
        assert_eq!(parse_pdf_date("D:20241320"), None); // month 13
        assert_eq!(parse_pdf_date("hello"), None);
    }

    #[test]
    fn der_total_len_reads_short_and_long_forms() {
        assert_eq!(der_total_len(&[0x30, 0x03, 1, 2, 3]), Some(5));
        assert_eq!(der_total_len(&[0x30, 0x82, 0x01, 0x00]), Some(4 + 256));
        assert_eq!(der_total_len(&[0x30]), None);
    }

    #[test]
    fn extract_cms_der_stops_at_declared_length_and_ignores_padding() {
        let mut hex = String::from("<3003010203");
        hex.push_str(&"0".repeat(40)); // reservation padding (§23.4)
        hex.push('>');
        let bytes = hex.as_bytes().to_vec();
        let extent = ContentsExtent {
            c_start: 0,
            c_end: bytes.len() as u64,
        };
        let der = extract_cms_der(&bytes, &extent).unwrap();
        assert_eq!(der, vec![0x30, 0x03, 0x01, 0x02, 0x03]);
    }
}
