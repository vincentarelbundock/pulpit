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
const OID_ID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
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
const OID_MGF1: &str = "1.2.840.113549.1.1.8";
const OID_RSASSA_PSS: &str = "1.2.840.113549.1.1.10";
const OID_SHA1_WITH_RSA: &str = "1.2.840.113549.1.1.5";
const OID_SHA256_WITH_RSA: &str = "1.2.840.113549.1.1.11";
const OID_SHA384_WITH_RSA: &str = "1.2.840.113549.1.1.12";
const OID_SHA512_WITH_RSA: &str = "1.2.840.113549.1.1.13";
const OID_ECDSA_WITH_SHA1: &str = "1.2.840.10045.4.1";
const OID_ECDSA_WITH_SHA256: &str = "1.2.840.10045.4.3.2";
const OID_ECDSA_WITH_SHA384: &str = "1.2.840.10045.4.3.3";
const OID_ECDSA_WITH_SHA512: &str = "1.2.840.10045.4.3.4";
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_ED25519: &str = "1.3.101.112";
const OID_PRIME256V1: &str = "1.2.840.10045.3.1.7";
const OID_SECP384R1: &str = "1.3.132.0.34";
/// `secp521r1` / NIST P-521 (`1.3.132.0.35`).
///
/// The signer has always been able to produce these — `SPEC-signing.md` lists
/// P-521 as implemented, and `sign::mechanism` selects it — while this file
/// knew only the other two curves. A P-521 signature therefore fell to the
/// catch-all below and came back `valid: false`, which the reader is shown as
/// "the cryptographic signature does not verify": a real signature reported as
/// a forged one.
const OID_SECP521R1: &str = "1.3.132.0.35";
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
///
/// Reported and not refused, deliberately. A weak algorithm makes a signature
/// *worth less*, not invalid: the bytes it covers are still the bytes that were
/// signed, and refusing to verify would replace a signature the reader can
/// judge with no signature at all, which reads as an unsigned document — the
/// same failure this crate closes elsewhere. pulpit also does no certificate
/// path validation, so it is in no position to make a trust decision on the
/// reader's behalf; what it *can* do honestly is say which algorithm was used
/// and how large the key was. A viewer that gains path validation should
/// revisit this and decide whether a floor becomes a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlgorithmFinding {
    /// A digest algorithm that is no longer collision resistant was declared.
    WeakDigest { algorithm: String },
    /// An RSA key below the package's 2048-bit signing floor was used.
    WeakRsaKey { bits: usize },
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

// --- Raw SignedAttributes recovery (RFC 5652 §5.4) ------------------------

/// One DER tag-length-value, borrowed from the buffer it was read out of.
struct Tlv<'a> {
    tag: u8,
    /// Tag and length octets only.
    header: &'a [u8],
    /// Content octets only.
    content: &'a [u8],
    /// Header and content together: the encoding as it appears on the wire.
    full: &'a [u8],
}

/// Read the single DER value at the start of `buf`. Only low-tag-number forms
/// and definite lengths are accepted, which is all DER permits here.
fn read_tlv(buf: &[u8]) -> Option<Tlv<'_>> {
    let tag = *buf.first()?;
    if tag & 0x1f == 0x1f {
        return None; // high-tag-number form: not used in CMS
    }
    let len_byte = *buf.get(1)?;
    let (len, header_len) = if len_byte < 0x80 {
        (len_byte as usize, 2)
    } else {
        let n = (len_byte & 0x7f) as usize;
        if n == 0 || n > 4 || buf.len() < 2 + n {
            return None; // indefinite length is forbidden in DER
        }
        let mut len: usize = 0;
        for &b in &buf[2..2 + n] {
            len = len.checked_mul(256)?.checked_add(b as usize)?;
        }
        (len, 2 + n)
    };
    let end = header_len.checked_add(len)?;
    if end > buf.len() {
        return None;
    }
    Some(Tlv {
        tag,
        header: &buf[..header_len],
        content: &buf[header_len..end],
        full: &buf[..end],
    })
}

/// Split `buf` into the sequence of DER values it contains, end to end.
fn tlv_children(buf: &[u8]) -> Option<Vec<Tlv<'_>>> {
    let mut out = Vec::new();
    let mut rest = buf;
    while !rest.is_empty() {
        let tlv = read_tlv(rest)?;
        rest = &rest[tlv.full.len()..];
        out.push(tlv);
    }
    Some(out)
}

/// The exact bytes that were signed, per RFC 5652 §5.4: the *original* encoded
/// `SignedAttributes` with the `[0] IMPLICIT` tag replaced by the universal
/// `SET OF` tag `0x31`, and nothing else changed.
///
/// Re-serialising the decoded attributes is not the same thing. A signer that
/// emitted a technically non-canonical but self-consistent encoding — an
/// unsorted SET, a long-form length — would have its own valid signature
/// rejected, and worse, the bytes we check would not be the bytes the signer
/// committed to. So the slice is recovered from the CMS blob by walking it.
///
/// The walk is: ContentInfo SEQUENCE -> [0] EXPLICIT content -> SignedData
/// SEQUENCE -> its last child, `signerInfos` SET OF -> the first SignerInfo
/// SEQUENCE -> its first `[0]` constructed child, `signedAttrs`. The `sid`
/// alternative that could be confused with it, `subjectKeyIdentifier`, is
/// `[0] IMPLICIT OCTET STRING` and so primitive (`0x80`), not `0xa0`.
fn raw_signed_attrs(cms_der: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let err = || "signed attributes could not be located in the CMS encoding".to_string();

    let content_info = read_tlv(cms_der).ok_or_else(err)?;
    let ci_children = tlv_children(content_info.content).ok_or_else(err)?;
    let explicit = ci_children.get(1).ok_or_else(err)?;
    if explicit.tag != 0xa0 {
        return Err(err());
    }
    let signed_data = read_tlv(explicit.content).ok_or_else(err)?;
    let sd_children = tlv_children(signed_data.content).ok_or_else(err)?;
    let signer_infos = sd_children.last().ok_or_else(err)?;
    if signer_infos.tag != 0x31 {
        return Err(err());
    }
    let signer_info = read_tlv(signer_infos.content).ok_or_else(err)?;
    let si_children = tlv_children(signer_info.content).ok_or_else(err)?;
    let attrs = si_children
        .iter()
        .find(|c| c.tag == 0xa0)
        .ok_or_else(|| "the signature carries no signed attributes".to_string())?;

    let mut out = Vec::with_capacity(attrs.full.len());
    out.push(0x31);
    out.extend_from_slice(&attrs.header[1..]);
    out.extend_from_slice(attrs.content);
    Ok(out)
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
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // The day must be legal for *this* month of *this* year: without the
    // month-aware bound, `days_from_civil` silently normalises 2024-02-31 into
    // early March and a nonsense date parses as a real instant.
    if !(1..=days_in_month(year, month as u32)).contains(&day) {
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

/// Number of days in `month` (1-12) of the proleptic Gregorian year `year`.
fn days_in_month(year: i64, month: u32) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
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

/// The public-key primitive a signature is to be verified with. It is derived
/// from `SignerInfo.signatureAlgorithm` — never guessed from the certificate —
/// and then required to agree with the certificate and the digest algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SigPrimitive {
    RsaPkcs1v15,
    RsaPss { salt_len: u8 },
    Ecdsa,
    Ed25519,
}

fn rsa_pss_parameters(ai: &AlgorithmIdentifierOwned) -> std::result::Result<(Digest, u8), String> {
    use rsa::pkcs1::RsaPssParams;

    let encoded = ai
        .parameters
        .as_ref()
        .ok_or_else(|| "RSASSA-PSS parameters are required".to_string())?
        .to_der()
        .map_err(|e| format!("RSASSA-PSS parameters could not be encoded: {e}"))?;
    let params = RsaPssParams::from_der(&encoded)
        .map_err(|e| format!("malformed RSASSA-PSS parameters: {e}"))?;
    let digest = Digest::from_oid(&params.hash.oid)
        .ok_or_else(|| format!("unsupported RSASSA-PSS digest {}", params.hash.oid))?;
    if params.mask_gen.oid.to_string() != OID_MGF1 {
        return Err(format!(
            "unsupported RSASSA-PSS mask generation algorithm {}",
            params.mask_gen.oid
        ));
    }
    let mgf_digest = params
        .mask_gen
        .parameters
        .as_ref()
        .and_then(|algorithm| Digest::from_oid(&algorithm.oid))
        .ok_or_else(|| "unsupported RSASSA-PSS MGF1 digest".to_string())?;
    if mgf_digest != digest {
        return Err(format!(
            "RSASSA-PSS uses {} but MGF1 uses {}",
            digest.name(),
            mgf_digest.name()
        ));
    }
    Ok((digest, params.salt_len))
}

/// Decompose `SignerInfo.signatureAlgorithm` into its primitive and, where the
/// OID names one, the digest it is bound to.
///
/// An unrecognised or unsupported algorithm is an error, not a licence to fall
/// back on whatever primitive the certificate happens to carry: verifying an
/// RSA-PSS signature as PKCS#1 v1.5 would be exactly that mistake.
fn classify_signature_algorithm(
    ai: &AlgorithmIdentifierOwned,
) -> std::result::Result<(SigPrimitive, Option<Digest>), String> {
    let s = ai.oid.to_string();
    match s.as_str() {
        // Plain rsaEncryption: legal in a SignerInfo, and names no digest.
        OID_RSA_ENCRYPTION => Ok((SigPrimitive::RsaPkcs1v15, None)),
        OID_SHA1_WITH_RSA => Ok((SigPrimitive::RsaPkcs1v15, Some(Digest::Sha1))),
        OID_SHA256_WITH_RSA => Ok((SigPrimitive::RsaPkcs1v15, Some(Digest::Sha256))),
        OID_SHA384_WITH_RSA => Ok((SigPrimitive::RsaPkcs1v15, Some(Digest::Sha384))),
        OID_SHA512_WITH_RSA => Ok((SigPrimitive::RsaPkcs1v15, Some(Digest::Sha512))),
        OID_RSASSA_PSS => {
            let (digest, salt_len) = rsa_pss_parameters(ai)?;
            Ok((SigPrimitive::RsaPss { salt_len }, Some(digest)))
        }
        OID_ECDSA_WITH_SHA1 => Ok((SigPrimitive::Ecdsa, Some(Digest::Sha1))),
        OID_ECDSA_WITH_SHA256 => Ok((SigPrimitive::Ecdsa, Some(Digest::Sha256))),
        OID_ECDSA_WITH_SHA384 => Ok((SigPrimitive::Ecdsa, Some(Digest::Sha384))),
        OID_ECDSA_WITH_SHA512 => Ok((SigPrimitive::Ecdsa, Some(Digest::Sha512))),
        // RFC 8419: Ed25519 in CMS is always paired with SHA-512.
        OID_ED25519 => Ok((SigPrimitive::Ed25519, Some(Digest::Sha512))),
        other => Err(format!("unsupported signature algorithm {other}")),
    }
}

/// The primitive implied by the certificate's SubjectPublicKeyInfo.
fn certificate_primitive(cert: &Certificate) -> Option<SigPrimitive> {
    let alg = cert
        .tbs_certificate
        .subject_public_key_info
        .algorithm
        .oid
        .to_string();
    match alg.as_str() {
        OID_RSA_ENCRYPTION => Some(SigPrimitive::RsaPkcs1v15),
        OID_EC_PUBLIC_KEY => Some(SigPrimitive::Ecdsa),
        OID_ED25519 => Some(SigPrimitive::Ed25519),
        _ => None,
    }
}

/// Check that the declared signature algorithm, the declared digest algorithm
/// and the certificate's key type describe one coherent scheme, and return the
/// primitive to verify with. §28.3 step 6 must never guess.
fn resolve_primitive(
    cert: &Certificate,
    sig_alg: &AlgorithmIdentifierOwned,
    digest: Digest,
) -> std::result::Result<SigPrimitive, String> {
    let (primitive, bound_digest) = classify_signature_algorithm(sig_alg)?;
    if let Some(bound) = bound_digest {
        if bound != digest {
            return Err(format!(
                "signature algorithm {} names {}, but the declared digest algorithm is {}",
                sig_alg.oid,
                bound.name(),
                digest.name()
            ));
        }
    }
    match certificate_primitive(cert) {
        Some(SigPrimitive::RsaPkcs1v15)
            if matches!(
                primitive,
                SigPrimitive::RsaPkcs1v15 | SigPrimitive::RsaPss { .. }
            ) =>
        {
            Ok(primitive)
        }
        Some(cert_primitive) if cert_primitive == primitive => Ok(primitive),
        Some(_) => Err(format!(
            "signature algorithm {} does not match the signer certificate's key type {}",
            sig_alg.oid, cert.tbs_certificate.subject_public_key_info.algorithm.oid
        )),
        None => Err(format!(
            "unsupported signer certificate key type {}",
            cert.tbs_certificate.subject_public_key_info.algorithm.oid
        )),
    }
}

/// The named curves this file can check a signature on.
///
/// Stated as a list rather than left implicit in the match below, because the
/// list has to stay in step with what `sign::mechanism` is willing to *produce*
/// and nothing was checking that it did. P-521 was signable and unverifiable
/// for as long as both existed: the match fell through to its catch-all, which
/// is indistinguishable from a bad signature, so a real signature was reported
/// to the reader as one that does not verify.
const VERIFIABLE_CURVES: [&str; 3] = [OID_PRIME256V1, OID_SECP384R1, OID_SECP521R1];

/// Is this named curve one we can check, rather than one we would silently
/// fail?
fn can_verify_curve(oid: &str) -> bool {
    VERIFIABLE_CURVES.contains(&oid)
}

fn verify_signature(
    cert: &Certificate,
    tbs: &[u8],
    signature: &[u8],
    digest: Digest,
    primitive: SigPrimitive,
) -> bool {
    let spki = &cert.tbs_certificate.subject_public_key_info;
    match primitive {
        SigPrimitive::RsaPkcs1v15 => verify_rsa(spki, tbs, signature, digest),
        SigPrimitive::RsaPss { salt_len } => verify_rsa_pss(spki, tbs, signature, digest, salt_len),
        SigPrimitive::Ecdsa => {
            let curve = match spki.algorithm.parameters.as_ref() {
                Some(p) => match p.decode_as::<ObjectIdentifier>() {
                    Ok(c) => c.to_string(),
                    Err(_) => return false,
                },
                None => return false,
            };
            let point = spki.subject_public_key.raw_bytes();
            if !can_verify_curve(&curve) {
                return false;
            }
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
                OID_SECP521R1 => {
                    use p521::ecdsa::signature::Verifier;
                    let key = match p521::ecdsa::VerifyingKey::from_sec1_bytes(point) {
                        Ok(k) => k,
                        Err(_) => return false,
                    };
                    match p521::ecdsa::Signature::from_der(signature) {
                        Ok(sig) => key.verify(tbs, &sig).is_ok(),
                        Err(_) => false,
                    }
                }
                // An unknown curve is indistinguishable here from a bad
                // signature, which is why the list above has to stay in step
                // with what `sign::mechanism` can produce.
                _ => false,
            }
        }
        SigPrimitive::Ed25519 => {
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

fn verify_rsa_pss(
    spki: &spki::SubjectPublicKeyInfoOwned,
    tbs: &[u8],
    signature: &[u8],
    digest: Digest,
    salt_len: u8,
) -> bool {
    use rsa::pkcs8::DecodePublicKey;
    use rsa::{Pss, RsaPublicKey};

    let spki_der = match spki.to_der() {
        Ok(d) => d,
        Err(_) => return false,
    };
    let key = match RsaPublicKey::from_public_key_der(&spki_der) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let hashed = digest.hash(&[tbs]);
    let padding = match digest {
        Digest::Sha1 => Pss::new_with_salt::<sha1::Sha1>(salt_len.into()),
        Digest::Sha256 => Pss::new_with_salt::<Sha256>(salt_len.into()),
        Digest::Sha384 => Pss::new_with_salt::<Sha384>(salt_len.into()),
        Digest::Sha512 => Pss::new_with_salt::<Sha512>(salt_len.into()),
    };
    key.verify(padding, &hashed, signature).is_ok()
}

fn rsa_modulus_bits(spki: &spki::SubjectPublicKeyInfoOwned) -> Option<usize> {
    use rsa::pkcs8::DecodePublicKey;
    use rsa::traits::PublicKeyParts;

    if spki.algorithm.oid != oid(OID_RSA_ENCRYPTION) {
        return None;
    }
    let encoded = spki.to_der().ok()?;
    let key = rsa::RsaPublicKey::from_public_key_der(&encoded).ok()?;
    Some(key.n().bits())
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
    if content_info.content_type != oid(OID_ID_SIGNED_DATA) {
        return SignatureVerification::broken(
            field,
            format!(
                "CMS content type is {}, expected id-signedData",
                content_info.content_type
            ),
        );
    }
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
    // RFC 5652 §5.1: every digest algorithm used by a SignerInfo MUST also
    // appear in SignedData.digestAlgorithms. A signer whose two declarations
    // disagree is not describing one coherent signature.
    if !signed_data
        .digest_algorithms
        .iter()
        .any(|ai| ai.oid == signer.digest_alg.oid)
    {
        return SignatureVerification::broken(
            field,
            format!(
                "digest algorithm {} is not listed in SignedData.digestAlgorithms",
                signer.digest_alg.oid
            ),
        );
    }

    // No security-critical signed attribute may appear twice: a verifier that
    // reads the first and a signer that meant the second disagree about what
    // was signed.
    //
    // Every attribute this file goes on to *read* belongs on this list, and two
    // did not. `find_attribute` takes the first match, out of a set the DER
    // layer has already re-sorted, so a signer could bind pulpit to one ESS
    // certificate while a verifier reading wire order bound to another — or
    // could carry one `cms-algorithm-protection` that agrees with the
    // SignerInfo and a second that does not. The whole set is signed, so this
    // is not something a third party can inject; it is the same "two readers,
    // one document, different answers" divergence, made by the signer.
    if let Some(attrs) = signer.signed_attrs.as_ref() {
        for (name, oid_str) in [
            ("content-type", OID_CONTENT_TYPE),
            ("message-digest", OID_MESSAGE_DIGEST),
            ("signing-time", OID_SIGNING_TIME),
            ("signing-certificate-v2", OID_SIGNING_CERTIFICATE_V2),
            ("cms-algorithm-protection", OID_CMS_ALGORITHM_PROTECTION),
        ] {
            let target = oid(oid_str);
            if attrs.iter().filter(|a| a.oid == target).count() > 1 {
                return SignatureVerification::broken(
                    field,
                    format!("the {name} signed attribute appears more than once"),
                );
            }
        }
    }

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
    if let Some(bits) = rsa_modulus_bits(&signer_cert.tbs_certificate.subject_public_key_info) {
        if bits < 2048 {
            findings.push(AlgorithmFinding::WeakRsaKey { bits });
        }
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

    // Step 6a: the declared signature algorithm, the declared digest algorithm
    // and the certificate's key type must describe one scheme. If they do not,
    // there is no primitive to fall back on — guessing one from the
    // certificate would verify an RSA-PSS signature as PKCS#1 v1.5.
    let primitive = match resolve_primitive(&signer_cert, &signer.signature_algorithm, digest_alg) {
        Ok(p) => p,
        Err(e) => return SignatureVerification::broken(field, e),
    };

    // Step 6b: verify over the *original* encoded signedAttrs, re-tagged as a
    // universal SET OF (RFC 5652 §5.4). See `raw_signed_attrs` for why the
    // decoded structure must not be re-serialised here.
    let valid = match raw_signed_attrs(&cms_der) {
        Ok(tbs) => verify_signature(
            &signer_cert,
            &tbs,
            signer.signature.as_bytes(),
            digest_alg,
            primitive,
        ),
        Err(e) => return SignatureVerification::broken(field, e),
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
    /// Every curve the signer will sign with is a curve this file can check.
    ///
    /// The two lists are deliberately separate — a verifier that shares its
    /// constants with the signer cannot catch the signer being wrong, which is
    /// why the OID tables in this file are not the ones in `sign::` — but
    /// "separate" has to mean *checked against each other*, not *unrelated*.
    /// They were unrelated, and P-521 fell down the gap: `sign::mechanism`
    /// selected `EcdsaP521Sha512`, this file had no `secp521r1` arm, and the
    /// catch-all reported a real signature as one that does not verify.
    #[test]
    fn every_curve_the_signer_accepts_can_be_verified() {
        for curve in crate::sign::credential::SIGNABLE_CURVES {
            assert!(
                super::can_verify_curve(curve),
                "the signer accepts {curve} but this file cannot check a signature on it, \
                 so such a signature would be reported as failing rather than as unsupported"
            );
        }
    }

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
    fn pdf_date_rejects_out_of_range_seconds() {
        assert_eq!(parse_pdf_date("D:20240820220099"), None, "SS=99");
        assert_eq!(parse_pdf_date("D:20240820220060"), None, "leap second");
        assert_eq!(parse_pdf_date("D:20240820229900"), None, "mm=99");
        assert_eq!(parse_pdf_date("D:20240820990000"), None, "HH=99");
        assert_eq!(
            parse_pdf_date("D:20240820220059"),
            Some(1_724_191_259),
            "SS=59 is legal"
        );
    }

    #[test]
    fn pdf_date_validates_the_day_against_its_month() {
        // Without a month-aware bound these normalise into the next month
        // instead of being rejected.
        assert_eq!(parse_pdf_date("D:20240231"), None, "31 February");
        assert_eq!(parse_pdf_date("D:20240431"), None, "31 April");
        assert_eq!(
            parse_pdf_date("D:20230229"),
            None,
            "2023 is not a leap year"
        );
        assert_eq!(
            parse_pdf_date("D:19000229"),
            None,
            "1900 is not a leap year"
        );
        assert_eq!(parse_pdf_date("D:20240800"), None, "day zero");

        // The legal neighbours of each of those still parse.
        assert!(
            parse_pdf_date("D:20240229").is_some(),
            "2024 is a leap year"
        );
        assert!(
            parse_pdf_date("D:20000229").is_some(),
            "2000 is a leap year"
        );
        assert!(parse_pdf_date("D:20240430").is_some());
        assert!(parse_pdf_date("D:20240131").is_some());
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

    #[test]
    fn rsa_pss_parameters_drive_pss_verification() {
        use der::asn1::Any;
        use rsa::pkcs1::RsaPssParams;
        use rsa::pkcs8::EncodePublicKey;
        use rsa::{Pss, RsaPrivateKey, RsaPublicKey};

        let params = RsaPssParams::new::<Sha256>(32);
        let algorithm = AlgorithmIdentifierOwned {
            oid: oid(OID_RSASSA_PSS),
            parameters: Some(Any::encode_from(&params).unwrap()),
        };
        assert_eq!(
            classify_signature_algorithm(&algorithm).unwrap(),
            (SigPrimitive::RsaPss { salt_len: 32 }, Some(Digest::Sha256))
        );

        let mut rng = rand::rngs::OsRng;
        let private = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public = RsaPublicKey::from(&private);
        let public_der = public.to_public_key_der().unwrap();
        let spki = spki::SubjectPublicKeyInfoOwned::from_der(public_der.as_bytes()).unwrap();
        let message = b"PSS parameters, not the certificate, choose the primitive";
        let digest = Digest::Sha256.hash(&[message]);
        let signature = private
            .sign_with_rng(&mut rng, Pss::new_with_salt::<Sha256>(32), &digest)
            .unwrap();

        assert!(verify_rsa_pss(
            &spki,
            message,
            &signature,
            Digest::Sha256,
            32
        ));
        assert!(!verify_rsa(&spki, message, &signature, Digest::Sha256));
    }

    #[test]
    fn rsa_pss_requires_the_mgf_digest_to_match() {
        use der::asn1::Any;
        use rsa::pkcs1::RsaPssParams;

        let mut params = RsaPssParams::new::<Sha256>(32);
        params.mask_gen.parameters = RsaPssParams::new::<Sha384>(48).mask_gen.parameters;
        let algorithm = AlgorithmIdentifierOwned {
            oid: oid(OID_RSASSA_PSS),
            parameters: Some(Any::encode_from(&params).unwrap()),
        };
        let reason = classify_signature_algorithm(&algorithm).unwrap_err();
        assert!(
            reason.contains("MGF1 uses SHA-384"),
            "unexpected reason: {reason}"
        );
    }

    #[test]
    fn rsa_modulus_strength_is_measured_from_the_public_key() {
        use rsa::pkcs8::EncodePublicKey;

        let mut rng = rand::rngs::OsRng;
        let private = rsa::RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let public = rsa::RsaPublicKey::from(&private);
        let public_der = public.to_public_key_der().unwrap();
        let spki = spki::SubjectPublicKeyInfoOwned::from_der(public_der.as_bytes()).unwrap();
        assert_eq!(rsa_modulus_bits(&spki), Some(1024));
    }

    // --- Algorithm-consistency and CMS-shape checks ----------------------
    //
    // These build a minimal signed "file" — a `/Contents` reservation between
    // two covered spans — and a CMS blob to drop into it, so that
    // `check_signature` runs end to end against a genuinely valid ECDSA P-256
    // signature that individual knobs can then be turned against.

    mod cms {
        use super::*;
        use ::cms::cert::CertificateChoices;
        use ::cms::content_info::{CmsVersion, ContentInfo};
        use ::cms::signed_data::{
            CertificateSet, DigestAlgorithmIdentifiers, EncapsulatedContentInfo, SignedData,
            SignerIdentifier, SignerInfo, SignerInfos,
        };
        use der::asn1::{Any, SetOfVec};
        use p256::ecdsa::signature::Signer;
        use p256::pkcs8::DecodePrivateKey;
        use x509_cert::attr::{Attribute, AttributeValue};
        use x509_cert::Certificate;

        const RESERVATION_BYTES: usize = 2048;

        fn any_of<T: Encode>(v: &T) -> Any {
            Any::from_der(&v.to_der().unwrap()).unwrap()
        }

        fn alg(o: &str) -> AlgorithmIdentifierOwned {
            AlgorithmIdentifierOwned {
                oid: oid(o),
                parameters: None,
            }
        }

        /// A self-signed P-256 certificate and the key that goes with it.
        fn credential() -> (Certificate, p256::ecdsa::SigningKey) {
            let mut params = rcgen::CertificateParams::new(vec!["cms-check-test".to_string()]);
            params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
            let generated = rcgen::Certificate::from_params(params).unwrap();
            let cert = Certificate::from_der(&generated.serialize_der().unwrap()).unwrap();
            let key =
                p256::ecdsa::SigningKey::from_pkcs8_der(&generated.serialize_private_key_der())
                    .unwrap();
            (cert, key)
        }

        /// The covered spans and the report describing them. The reservation
        /// interior is excluded from the byte range, so the document digest
        /// does not depend on the CMS that will be written into it.
        fn scaffold() -> (Vec<u8>, StructuralReport) {
            let mut bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Sig /Contents ".to_vec();
            let c_start = bytes.len() as u64;
            bytes.push(b'<');
            bytes.extend(std::iter::repeat_n(b'0', RESERVATION_BYTES * 2));
            bytes.push(b'>');
            let c_end = bytes.len() as u64;
            bytes.extend_from_slice(b" /SubFilter /adbe.pkcs7.detached >>\nendobj\n%%EOF\n");

            let report = StructuralReport {
                field_name: "Sig1".to_string(),
                coverage: SignatureCoverage::EntireFile,
                later_revisions: false,
                contents_extent: ContentsExtent { c_start, c_end },
                byte_range: ByteRange {
                    z: 0,
                    len1: c_start,
                    start2: c_end,
                    len2: bytes.len() as u64 - c_end,
                },
                sig_dict_revision: 0,
                declared_docmdp: None,
                sub_filter: Some("adbe.pkcs7.detached".to_string()),
                mod_date: None,
            };
            (bytes, report)
        }

        fn write_cms(bytes: &mut [u8], report: &StructuralReport, cms_der: &[u8]) {
            let start = report.contents_extent.c_start as usize + 1;
            assert!(cms_der.len() <= RESERVATION_BYTES, "CMS must fit");
            for (i, byte) in cms_der.iter().enumerate() {
                let hex = format!("{byte:02X}");
                bytes[start + i * 2] = hex.as_bytes()[0];
                bytes[start + i * 2 + 1] = hex.as_bytes()[1];
            }
        }

        /// Everything a test might want to bend away from the valid case.
        struct Knobs {
            content_type: &'static str,
            digest_set: &'static str,
            signer_digest: &'static str,
            signature_alg: &'static str,
            duplicate_content_type: bool,
        }

        impl Default for Knobs {
            fn default() -> Self {
                Knobs {
                    content_type: OID_ID_SIGNED_DATA,
                    digest_set: OID_SHA256,
                    signer_digest: OID_SHA256,
                    signature_alg: OID_ECDSA_WITH_SHA256,
                    duplicate_content_type: false,
                }
            }
        }

        /// Build a CMS blob over `scaffold()`'s covered bytes, signed for real.
        fn build(knobs: Knobs) -> (Vec<u8>, StructuralReport) {
            let (mut bytes, report) = scaffold();
            let (cert, key) = credential();

            let spans = byte_range_spans(&bytes, &report.byte_range).unwrap();
            let message_digest = Digest::Sha256.hash(&[spans.0, spans.1]);

            let mut attrs: SetOfVec<Attribute> = SetOfVec::new();
            attrs
                .insert(Attribute {
                    oid: oid(OID_CONTENT_TYPE),
                    values: SetOfVec::try_from(vec![any_of(&oid(OID_ID_DATA))]).unwrap(),
                })
                .unwrap();
            attrs
                .insert(Attribute {
                    oid: oid(OID_MESSAGE_DIGEST),
                    values: SetOfVec::try_from(vec![any_of(
                        &OctetString::new(message_digest.clone()).unwrap(),
                    )])
                    .unwrap(),
                })
                .unwrap();
            if knobs.duplicate_content_type {
                // A second content-type attribute carrying a different value:
                // distinct as a SET element, indistinguishable to a verifier
                // that reads only the first one it finds.
                let values: SetOfVec<AttributeValue> =
                    SetOfVec::try_from(vec![any_of(&oid(OID_ID_SIGNED_DATA))]).unwrap();
                attrs
                    .insert(Attribute {
                        oid: oid(OID_CONTENT_TYPE),
                        values,
                    })
                    .unwrap();
            }

            let tbs = attrs.to_der().unwrap();
            let sig: p256::ecdsa::Signature = key.sign(&tbs);

            let signer = SignerInfo {
                version: CmsVersion::V1,
                sid: SignerIdentifier::IssuerAndSerialNumber(::cms::cert::IssuerAndSerialNumber {
                    issuer: cert.tbs_certificate.issuer.clone(),
                    serial_number: cert.tbs_certificate.serial_number.clone(),
                }),
                digest_alg: alg(knobs.signer_digest),
                signed_attrs: Some(attrs),
                signature_algorithm: alg(knobs.signature_alg),
                signature: OctetString::new(sig.to_der().as_bytes().to_vec()).unwrap(),
                unsigned_attrs: None,
            };

            let mut digest_algorithms: DigestAlgorithmIdentifiers = SetOfVec::new();
            digest_algorithms.insert(alg(knobs.digest_set)).unwrap();

            let signed_data = SignedData {
                version: CmsVersion::V1,
                digest_algorithms,
                encap_content_info: EncapsulatedContentInfo {
                    econtent_type: oid(OID_ID_DATA),
                    econtent: None,
                },
                certificates: Some(
                    CertificateSet::try_from(vec![CertificateChoices::Certificate(cert)]).unwrap(),
                ),
                crls: None,
                signer_infos: SignerInfos::try_from(vec![signer]).unwrap(),
            };
            let content_info = ContentInfo {
                content_type: oid(knobs.content_type),
                content: any_of(&signed_data),
            };
            let cms_der = content_info.to_der().unwrap();
            write_cms(&mut bytes, &report, &cms_der);
            (bytes, report)
        }

        fn broken_reason(knobs: Knobs) -> String {
            let (bytes, report) = build(knobs);
            match check_signature(&bytes, &report) {
                SignatureVerification::Broken { reason, .. } => reason,
                other => panic!("expected broken, got {other:?}"),
            }
        }

        #[test]
        fn the_unmodified_fixture_is_intact_and_valid() {
            let (bytes, report) = build(Knobs::default());
            match check_signature(&bytes, &report) {
                SignatureVerification::Checked(status) => {
                    assert!(status.intact, "message-digest should match");
                    assert!(status.valid, "signature should verify");
                    assert_eq!(status.digest_algorithm, "SHA-256");
                    assert_eq!(status.signature_algorithm, OID_ECDSA_WITH_SHA256);
                }
                other => panic!("expected checked, got {other:?}"),
            }
        }

        #[test]
        fn signature_algorithm_naming_another_digest_is_rejected() {
            // ecdsa-with-SHA512 declared while digestAlgorithm says SHA-256:
            // the tuple does not describe one scheme, so there is nothing to
            // verify. Previously the declaration was display text only and
            // SHA-256 was used anyway.
            let reason = broken_reason(Knobs {
                signature_alg: OID_ECDSA_WITH_SHA512,
                ..Knobs::default()
            });
            assert!(
                reason.contains("names SHA-512") && reason.contains("declared digest"),
                "unexpected reason: {reason}"
            );
        }

        #[test]
        fn signature_algorithm_disagreeing_with_the_key_type_is_rejected() {
            // sha256WithRSAEncryption declared over an EC certificate.
            let reason = broken_reason(Knobs {
                signature_alg: OID_SHA256_WITH_RSA,
                ..Knobs::default()
            });
            assert!(
                reason.contains("does not match the signer certificate's key type"),
                "unexpected reason: {reason}"
            );
        }

        #[test]
        fn rsa_pss_without_required_parameters_is_refused() {
            let reason = broken_reason(Knobs {
                signature_alg: OID_RSASSA_PSS,
                ..Knobs::default()
            });
            assert!(reason.contains("RSASSA-PSS"), "unexpected reason: {reason}");
        }

        #[test]
        fn wrong_content_info_content_type_is_rejected() {
            // id-data where id-signedData is required.
            let reason = broken_reason(Knobs {
                content_type: OID_ID_DATA,
                ..Knobs::default()
            });
            assert!(
                reason.contains("expected id-signedData"),
                "unexpected reason: {reason}"
            );
        }

        #[test]
        fn digest_algorithm_missing_from_the_signed_data_set_is_rejected() {
            let reason = broken_reason(Knobs {
                digest_set: OID_SHA512,
                ..Knobs::default()
            });
            assert!(
                reason.contains("not listed in SignedData.digestAlgorithms"),
                "unexpected reason: {reason}"
            );
        }

        #[test]
        fn a_repeated_content_type_attribute_is_rejected() {
            let reason = broken_reason(Knobs {
                duplicate_content_type: true,
                ..Knobs::default()
            });
            assert!(
                reason.contains("content-type signed attribute appears more than once"),
                "unexpected reason: {reason}"
            );
        }

        #[test]
        fn signed_attrs_are_taken_from_the_original_encoding() {
            let (bytes, report) = build(Knobs::default());
            let cms_der = extract_cms_der(&bytes, &report.contents_extent).unwrap();
            let recovered = raw_signed_attrs(&cms_der).unwrap();

            // The universal SET OF tag replaces [0] IMPLICIT, and nothing else
            // changes: the recovered body must appear verbatim in the blob,
            // tag included, at the position the walk found it.
            assert_eq!(recovered[0], 0x31);
            let body = &recovered[1..];
            let found = cms_der
                .windows(body.len())
                .position(|w| w == body)
                .expect("the recovered bytes must be a slice of the CMS blob");
            assert_eq!(
                cms_der[found - 1],
                0xa0,
                "and must sit directly behind the [0] IMPLICIT tag"
            );
        }
    }
}
