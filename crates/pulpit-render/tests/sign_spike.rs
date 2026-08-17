//! S0 Spike: Integration test for signing feature.
//!
//! Produces a real signed PDF using merged modules and validates it with pyHanko oracle.
//! Per SPEC-signing.md §35 Milestone S0 step 3: produce a signed fixture and oracle green.
//!
//! The PDF assembly itself lives in `signing_fixture`, shared with
//! `verify_cms.rs`.

mod signing_fixture;

use pulpit_render::verify;
use signing_fixture::{build_signed_pdf, load_test_credential, skip_message};
use std::path::PathBuf;

#[test]
fn sign_spike_create_signed_pdf() {
    // Skip if credentials don't exist
    let cred = match load_test_credential() {
        Some(c) => c,
        None => {
            skip_message();
            return;
        }
    };

    let fixture = build_signed_pdf(&cred);
    let output = fixture.bytes;
    let (sig_start, sig_end) = (fixture.sig_start, fixture.sig_end);

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

    // Discover signatures in the PDF
    let signatures =
        verify::discover_signatures(&output, &rev_map).expect("failed to discover signatures");

    // HARD ASSERTIONS: exactly 1 signature discovered
    assert_eq!(
        signatures.len(),
        1,
        "expected exactly 1 signature, found {}",
        signatures.len()
    );

    let sig = &signatures[0];
    assert_eq!(
        sig.coverage,
        verify::SignatureCoverage::EntireFile,
        "signature should cover EntireFile, but got {:?}",
        sig.coverage
    );

    // Verify that /Contents extent is correct per §28.2:
    // c_start should be '<', c_end-1 should be '>'
    assert_eq!(
        output[sig.contents_extent.c_start as usize], b'<',
        "byte at c_start should be '<'"
    );
    assert_eq!(
        output[sig.contents_extent.c_end as usize - 1],
        b'>',
        "byte at c_end-1 should be '>'"
    );

    println!(
        "SUCCESS: Created signed PDF at {} ({} bytes)",
        fixture_path.display(),
        output.len()
    );
    println!(
        "Discovered signature: field={}, coverage={:?}, c_start={}, c_end={}",
        sig.field_name, sig.coverage, sig.contents_extent.c_start, sig.contents_extent.c_end
    );
}
