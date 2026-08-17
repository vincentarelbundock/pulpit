// Each integration test binary compiles this module separately and uses a
// different subset of it.
#![allow(dead_code)]

//! Shared fixture builder: produces a real signed PDF with the oracle's
//! self-signed test credential, following the §23.3 assembly order.
//!
//! Used by both `sign_spike.rs` (which also feeds the pyHanko oracle) and
//! `verify_cms.rs` (which checks it with pulpit's own §28.3 verifier).

use pulpit_render::sign::{self, SigningProfile};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// The `/M` value written into the signature dictionary, and its unix-seconds
/// equivalent — the two must agree, and `verify_cms.rs` asserts they do.
pub const MOD_DATE: &str = "D:20240820220000+00'00'";
pub const MOD_DATE_UNIX: i64 = 1_724_191_200;

/// The `signing-time` signed attribute value handed to the CMS builder.
pub const SIGNING_TIME_UNIX: i64 = 1_724_166_000;

/// A signed PDF, plus the offsets a tamper test needs.
pub struct SignedFixture {
    pub bytes: Vec<u8>,
    /// Offset of the `<` opening the `/Contents` reservation.
    pub sig_start: u64,
    /// Offset just past the `>` closing it.
    pub sig_end: u64,
    /// Length of the CMS DER actually written into the reservation.
    pub cms_len: usize,
}

/// Load the oracle's test credential, or `None` when it has not been generated.
pub fn load_test_credential() -> Option<sign::Credential> {
    let cred_path = PathBuf::from("../../tools/sign-oracle/credentials/test-self-signed.p12");
    if cred_path.exists() {
        if let Ok(p12_bytes) = std::fs::read(&cred_path) {
            if let Ok(cred) =
                sign::load_pkcs12(&p12_bytes, sign::Zeroizing::new("test".to_string()))
            {
                return Some(cred);
            }
        }
    }
    None
}

/// Skip-with-message helper for tests that need the oracle's credential.
pub fn skip_message() {
    eprintln!("SKIP: tools/sign-oracle/credentials/test-self-signed.p12 not found");
    eprintln!("      Run: make sign-oracle-setup");
    eprintln!("      Then: .venv-sign-oracle/bin/python tools/sign-oracle/gen-credentials.py");
}

/// A credential to sign with: the oracle's PKCS#12 when it has been generated,
/// otherwise a freshly generated self-signed ECDSA P-256 credential, so that
/// verification tests run on every machine.
pub fn any_test_credential() -> sign::Credential {
    if let Some(cred) = load_test_credential() {
        return cred;
    }
    generate_self_signed_credential()
}

/// Generate a self-signed ECDSA P-256 credential with rcgen.
pub fn generate_self_signed_credential() -> sign::Credential {
    let mut params = rcgen::CertificateParams::new(vec!["pulpit-verify-test".to_string()]);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "pulpit verify test");
    params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
    let cert = rcgen::Certificate::from_params(params).expect("rcgen certificate");
    let cert_der = cert.serialize_der().expect("serialize certificate");
    let key_der = cert.serialize_private_key_der();
    sign::credential_from_parts(&cert_der, &key_der, Vec::new()).expect("credential from parts")
}

/// Assemble a single-revision PDF from object bodies given in order, starting
/// at object 1, with a classic cross-reference table and a fixed `/ID`.
pub fn assemble_single_revision(objects: &[String]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(b"%PDF-1.7\n");

    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }

    let xref_start = output.len();
    output.extend_from_slice(b"xref\n");
    output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
    output.extend_from_slice(format!("1 {}\n", objects.len()).as_bytes());
    for offset in &offsets {
        output.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    output.extend_from_slice(b"trailer\n<<\n");
    output.extend_from_slice(format!("/Size {}\n", objects.len() + 1).as_bytes());
    output.extend_from_slice(b"/Root 1 0 R\n");
    output.extend_from_slice(
        b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
    );
    output.extend_from_slice(b">>\nstartxref\n");
    output.extend_from_slice(format!("{}\n", xref_start).as_bytes());
    output.extend_from_slice(b"%%EOF");
    output
}

/// An unsigned single-page PDF carrying `fields` empty `/Sig` fields.
///
/// With no field names it has no AcroForm at all, which is the shape the
/// "create a new invisible field" path has to cope with.
pub fn build_unsigned_pdf(fields: &[&str]) -> Vec<u8> {
    let field_first = 5u32;
    let refs: Vec<String> = (0..fields.len())
        .map(|i| format!("{} 0 R", field_first + i as u32))
        .collect();

    let mut objects = Vec::new();
    if fields.is_empty() {
        objects.push("<</Type /Catalog /Pages 2 0 R>>".to_string());
    } else {
        objects.push("<</Type /Catalog /Pages 2 0 R /AcroForm 4 0 R>>".to_string());
    }
    objects.push("<</Type /Pages /Kids [3 0 R] /Count 1>>".to_string());
    if fields.is_empty() {
        objects.push("<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>".to_string());
    } else {
        objects.push(format!(
            "<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [{}]>>",
            refs.join(" ")
        ));
        // Object 4 is the AcroForm even when unused, so that field object
        // numbers do not shift between the two shapes.
        objects.push(format!("<</Fields [{}] /SigFlags 3>>", refs.join(" ")));
        for name in fields {
            objects.push(format!(
                "<</FT /Sig /T ({}) /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R>>",
                name
            ));
        }
    }
    assemble_single_revision(&objects)
}

/// An unsigned two-field PDF in which `Sig1` carries a signature dictionary
/// whose FieldMDP transform locks `Sig2`.
///
/// The signature dictionary is a stub: the pre-flight refusal this fixture
/// exists to trigger happens before any cryptography is consulted.
pub fn build_pdf_with_fieldmdp_lock() -> Vec<u8> {
    let objects = vec![
        "<</Type /Catalog /Pages 2 0 R /AcroForm 4 0 R>>".to_string(),
        "<</Type /Pages /Kids [3 0 R] /Count 1>>".to_string(),
        "<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R 6 0 R]>>".to_string(),
        "<</Fields [5 0 R 6 0 R] /SigFlags 3>>".to_string(),
        "<</FT /Sig /T (Sig1) /V 7 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R>>".to_string(),
        "<</FT /Sig /T (Sig2) /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R>>".to_string(),
        "<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /Contents <00> \
          /ByteRange [0 0 0 0] /Reference [<< /Type /SigRef /TransformMethod /FieldMDP \
          /TransformParams << /Type /TransformParams /Action /Include /Fields [(Sig2)] /V /1.2 >> >>]>>".to_string(),
    ];
    assemble_single_revision(&objects)
}

/// Build a two-revision PDF whose second revision carries a signature over the
/// whole file.
pub fn build_signed_pdf(cred: &sign::Credential) -> SignedFixture {
    // Estimate CMS size per §23.5
    let test_digest = vec![0u8; 32];
    let bytes_reserved = sign::estimate_cms_size(
        cred,
        &test_digest,
        SigningProfile::AdbePkcs7Detached,
        false,
        false,
        Some(SIGNING_TIME_UNIX),
    )
    .expect("failed to estimate CMS size");

    let mut output = Vec::new();

    output.extend_from_slice(b"%PDF-1.4\n");

    // Object 1: Catalog (initial, no AcroForm yet)
    let obj1_pos = output.len();
    output.extend_from_slice(b"1 0 obj\n");
    output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R>>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 2: Pages
    let obj2_pos = output.len();
    output.extend_from_slice(b"2 0 obj\n");
    output.extend_from_slice(b"<</Type /Pages /Kids [3 0 R] /Count 1>>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 3: Page
    let obj3_pos = output.len();
    output.extend_from_slice(b"3 0 obj\n");
    output.extend_from_slice(b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]>>\n");
    output.extend_from_slice(b"endobj\n");

    // xref and trailer for initial revision
    let xref1_start = output.len();
    output.extend_from_slice(b"xref\n");
    output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
    output.extend_from_slice(format!("1 1\n{:010} 00000 n \n", obj1_pos).as_bytes());
    output.extend_from_slice(format!("2 1\n{:010} 00000 n \n", obj2_pos).as_bytes());
    output.extend_from_slice(format!("3 1\n{:010} 00000 n \n", obj3_pos).as_bytes());
    output.extend_from_slice(b"trailer\n");
    output.extend_from_slice(b"<<\n");
    output.extend_from_slice(b"/Size 4\n");
    output.extend_from_slice(b"/Root 1 0 R\n");
    output.extend_from_slice(
        b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
    );
    output.extend_from_slice(b">>\n");
    output.extend_from_slice(b"startxref\n");
    output.extend_from_slice(format!("{}\n", xref1_start).as_bytes());
    output.extend_from_slice(b"%%EOF");

    // Now start the signing revision

    // Object 4: Updated Catalog with /AcroForm reference (not inline)
    let obj4_start = output.len();
    output.extend_from_slice(b"4 0 obj\n");
    output.extend_from_slice(b"<</Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 5: AcroForm dictionary with /Fields array
    let obj5_start = output.len();
    output.extend_from_slice(b"5 0 obj\n");
    output.extend_from_slice(b"<</Fields [6 0 R] /SigFlags 3 >>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 6: Signature field
    let obj6_start = output.len();
    output.extend_from_slice(b"6 0 obj\n");
    output.extend_from_slice(b"<</FT /Sig /T (Sig1) /V 7 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /F 132 /P 3 0 R >>\n");
    output.extend_from_slice(b"endobj\n");

    // Object 7: Signature dictionary with placeholders per §23.2
    let obj7_start = output.len();
    output.extend_from_slice(b"7 0 obj\n");
    output
        .extend_from_slice(b"<</Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached ");

    // /ByteRange placeholder ([] + 60 spaces = 62 bytes)
    output.extend_from_slice(b"/ByteRange ");
    let byterange_start = output.len() as u64;
    output.extend_from_slice(b"[]");
    output.resize(output.len() + 60, b' ');

    // /Contents placeholder (< + bytes_reserved 0s + >)
    output.extend_from_slice(b"/Contents ");
    let sig_start = output.len() as u64;
    output.extend_from_slice(b"<");
    output.resize(output.len() + bytes_reserved, b'0');
    output.extend_from_slice(b">");
    let sig_end = output.len() as u64;

    // /M entry
    output.extend_from_slice(format!("/M ({}) ", MOD_DATE).as_bytes());

    output.extend_from_slice(b">>\n");
    output.extend_from_slice(b"endobj\n");

    // xref for signing revision - all new/modified objects with correct offsets
    let xref2_start = output.len();
    output.extend_from_slice(b"xref\n");
    output.extend_from_slice(b"0 1\n0000000000 65535 f \n");
    output.extend_from_slice(format!("4 4\n{:010} 00000 n \n", obj4_start).as_bytes());
    output.extend_from_slice(format!("{:010} 00000 n \n", obj5_start).as_bytes());
    output.extend_from_slice(format!("{:010} 00000 n \n", obj6_start).as_bytes());
    output.extend_from_slice(format!("{:010} 00000 n \n", obj7_start).as_bytes());

    output.extend_from_slice(b"trailer\n");
    output.extend_from_slice(b"<<\n");
    output.extend_from_slice(b"/Size 8\n");
    output.extend_from_slice(b"/Root 4 0 R\n");
    output.extend_from_slice(format!("/Prev {}\n", xref1_start).as_bytes());
    output.extend_from_slice(
        b"/ID [<0102030405060708090A0B0C0D0E0F10> <1112131415161718191A1B1C1D1E1F20>]\n",
    );
    output.extend_from_slice(b">>\n");
    output.extend_from_slice(b"startxref\n");
    output.extend_from_slice(format!("{}\n", xref2_start).as_bytes());
    output.extend_from_slice(b"%%EOF");

    // Signing per §23.3: assembly order matters.
    let final_eof = output.len() as u64;

    // Step 1: back-patch /ByteRange BEFORE the digest.
    let byterange_str = format!("[0 {} {} {}]", sig_start, sig_end, final_eof - sig_end);
    for (i, byte) in byterange_str.as_bytes().iter().enumerate() {
        output[byterange_start as usize + i] = *byte;
    }

    // Step 2: compute the document digest.
    let mut hasher = Sha256::new();
    hasher.update(&output[0..sig_start as usize]);
    hasher.update(&output[sig_end as usize..final_eof as usize]);
    let document_digest = hasher.finalize().to_vec();

    // Step 3: build the CMS over the digest.
    let cms_bytes = sign::build_cms(
        cred,
        &document_digest,
        SigningProfile::AdbePkcs7Detached,
        false,
        None,
        Some(SIGNING_TIME_UNIX),
    )
    .expect("failed to build CMS");

    // Step 4: fill the reservation; the remaining hex digits stay '0' (§23.4).
    for (i, byte) in cms_bytes.iter().enumerate() {
        let pos = sig_start as usize + 1 + i * 2;
        let hex = format!("{:02X}", byte);
        output[pos] = hex.as_bytes()[0];
        output[pos + 1] = hex.as_bytes()[1];
    }

    SignedFixture {
        bytes: output,
        sig_start,
        sig_end,
        cms_len: cms_bytes.len(),
    }
}
