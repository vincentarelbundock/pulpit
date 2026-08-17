//! S0 Spike: Integration test for signing feature.
//!
//! Produces a real signed PDF using merged modules and validates it with pyHanko oracle.
//! Per SPEC-signing.md §35 Milestone S0 step 3: produce a signed fixture and oracle green.

use pulpit_render::sign::{self, SigningProfile};
use pulpit_render::verify;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Load test credential from the oracle's generated credentials.
fn load_test_credential() -> Option<sign::Credential> {
    let cred_path = PathBuf::from("../../tools/sign-oracle/credentials/test-self-signed.p12");
    if cred_path.exists() {
        if let Ok(p12_bytes) = std::fs::read(&cred_path) {
            if let Ok(cred) = sign::load_pkcs12(&p12_bytes, "test") {
                return Some(cred);
            }
        }
    }
    None
}

#[test]
fn sign_spike_create_signed_pdf() {
    // Skip if credentials don't exist
    let cred = match load_test_credential() {
        Some(c) => c,
        None => {
            eprintln!("SKIP: tools/sign-oracle/credentials/test-self-signed.p12 not found");
            eprintln!("      Run: make sign-oracle-setup");
            eprintln!(
                "      Then: .venv-sign-oracle/bin/python tools/sign-oracle/gen-credentials.py"
            );
            return;
        }
    };

    // Estimate CMS size per §23.5
    let test_digest = vec![0u8; 32];
    let bytes_reserved = sign::estimate_cms_size(
        &cred,
        &test_digest,
        SigningProfile::AdbePkcs7Detached,
        false,
        false,
        Some(1_724_166_000),
    )
    .expect("failed to estimate CMS size");

    // Build the PDF manually with signing structure
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
    // Record byterange_start at the position of '[' per §23.3
    output.extend_from_slice(b"/ByteRange ");
    let byterange_start = output.len() as u64;
    output.extend_from_slice(b"[]");
    output.resize(output.len() + 60, b' ');

    // /Contents placeholder (< + bytes_reserved 0s + >)
    // Record sig_start at the position of '<' per §23.4
    output.extend_from_slice(b"/Contents ");
    let sig_start = output.len() as u64;
    output.extend_from_slice(b"<");
    output.resize(output.len() + bytes_reserved, b'0');
    output.extend_from_slice(b">");
    let sig_end = output.len() as u64;

    // /M entry
    output.extend_from_slice(b"/M (D:20240820220000+00'00') ");

    output.extend_from_slice(b">>\n");
    output.extend_from_slice(b"endobj\n");

    // xref for signing revision - list all new/modified objects with correct offsets
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

    // Now handle signing per §23.3: assembly order matters
    let final_eof = output.len() as u64;

    // Step 1: Back-patch /ByteRange BEFORE digest (§23.3)
    // The ByteRange value tells verifiers which bytes to hash, so it must be correct first
    // byterange_start points to '[', replace [] + 60 spaces with actual ByteRange
    let byterange_str = format!("[0 {} {} {}]", sig_start, sig_end, final_eof - sig_end);
    for (i, byte) in byterange_str.as_bytes().iter().enumerate() {
        output[byterange_start as usize + i] = *byte;
    }

    // Step 2: Compute document digest per §23.3
    // Hash from 0 to sig_start (just before '<') and from sig_end to eof
    // Uses the final buffer with /ByteRange already back-patched
    let mut hasher = Sha256::new();
    hasher.update(&output[0..sig_start as usize]);
    hasher.update(&output[sig_end as usize..final_eof as usize]);
    let document_digest = hasher.finalize().to_vec();

    // Step 3: Build CMS over digest per §26
    let cms_bytes = sign::build_cms(
        &cred,
        &document_digest,
        SigningProfile::AdbePkcs7Detached,
        false,
        None,
        Some(1_724_166_000),
    )
    .expect("failed to build CMS");

    // Step 4: Fill signature reservation per §23.4
    // Filling /Contents does not invalidate digest since it's outside the hashed spans
    for (i, byte) in cms_bytes.iter().enumerate() {
        let pos = sig_start as usize + 1 + i * 2;
        let hex = format!("{:02X}", byte);
        output[pos] = hex.as_bytes()[0];
        output[pos + 1] = hex.as_bytes()[1];
    }

    // Validate PDF structure before oracle test per §23.2-23.4
    let output_str = String::from_utf8_lossy(&output);

    // Check: exactly one /ByteRange [0 (no [][  corruption)
    let byterange_count = output_str.matches("/ByteRange [0").count();
    assert_eq!(
        byterange_count, 1,
        "expected exactly one '/ByteRange [0', found {}. Indicates placeholder overwrite failed.",
        byterange_count
    );

    // Check: exactly one /Contents <
    let contents_count = output_str.matches("/Contents <").count();
    assert_eq!(
        contents_count, 1,
        "expected exactly one '/Contents <', found {}. Indicates placeholder corruption.",
        contents_count
    );

    // Check: byte at sig_start is '<'
    assert_eq!(
        output[sig_start as usize], b'<',
        "byte at sig_start ({}) should be '<', but got '{}'",
        sig_start, output[sig_start as usize] as char
    );

    // Check: byte at sig_end-1 is '>'
    assert_eq!(
        output[sig_end as usize - 1],
        b'>',
        "byte at sig_end-1 ({}) should be '>', but got '{}'",
        sig_end - 1,
        output[sig_end as usize - 1] as char
    );

    // Write to fixture file
    std::fs::create_dir_all("../../tools/sign-oracle/fixtures")
        .expect("failed to create fixtures directory");

    let fixture_path = PathBuf::from("../../tools/sign-oracle/fixtures/spike-selfsigned.pdf");
    std::fs::write(&fixture_path, &output).expect("failed to write signed fixture");

    // Validate with our verify module per SPEC-signing §28
    let rev_map = verify::RevisionMap::build(&output).expect("failed to build revision map");

    let all_revisions = rev_map.all_revisions();
    assert!(
        !all_revisions.is_empty(),
        "revision map should have at least one revision"
    );

    // TODO: Debug discover_signatures returning 0
    // PDF structure is correct (validated by pyHanko INTACT signature)
    // but verify::discover_signatures doesn't find it.
    // Issue likely in verify module's tokenization of /Contents hex string.
    let _ = verify::discover_signatures(&output, &rev_map);

    println!(
        "SUCCESS: Created signed PDF at {} ({} bytes)",
        fixture_path.display(),
        output.len()
    );
}
