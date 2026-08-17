//! Integration tests for `sign::apply`: signing a file on disk end to end.
//!
//! These cover the §34.2 scenarios that the S0 spike only rehearsed by hand —
//! a fresh invisible signature, the countersigning round trip, the §31.3
//! refusals, and the §32 gate refusing to promote a corrupted candidate.
//!
//! Every test that needs a real key skips with a message when the oracle's
//! credential has not been generated, the same way `sign_spike.rs` does.

mod signing_fixture;

use pulpit_render::sign::apply::{
    sign_document_file, sign_document_file_with_tamper, SignApplyError, SignRequest, SignTarget,
};
use pulpit_render::verify::{self, SignatureCoverage, SignatureVerification};
use signing_fixture::{
    build_pdf_with_fieldmdp_lock, build_unsigned_pdf, load_test_credential, skip_message,
    SIGNING_TIME_UNIX,
};
use std::path::{Path, PathBuf};

/// A request with the fixed inputs the app layer would otherwise supply: the
/// signing time and the trailer's new `/ID` randomness. `pulpit-render` reads
/// neither a clock nor an entropy source, so tests pin both.
fn request(field: SignTarget) -> SignRequest {
    SignRequest {
        signing_time: SIGNING_TIME_UNIX,
        field,
        reason: Some("Integration test".to_string()),
        location: Some("Montréal".to_string()),
        id2: [
            0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18, 0x29, 0x3A, 0x4B, 0x5C, 0x6D, 0x7E,
            0x8F, 0x90,
        ],
        ..SignRequest::default()
    }
}

fn statuses(bytes: &[u8]) -> Vec<pulpit_render::verify::SignatureStatus> {
    verify::verify_signatures(bytes)
        .expect("verification runs")
        .into_iter()
        .map(|v| match v {
            SignatureVerification::Checked(status) => *status,
            SignatureVerification::Broken { field_name, reason } => {
                panic!("signature '{field_name}' is broken: {reason}")
            }
        })
        .collect()
}

/// Where `make sign-oracle` looks for PDFs to hand to pyHanko.
fn oracle_fixture_path(name: &str) -> PathBuf {
    let directory = Path::new("../../tools/sign-oracle/fixtures");
    std::fs::create_dir_all(directory).expect("create the oracle fixtures directory");
    directory.join(name)
}

#[test]
fn a_fresh_invisible_signature_covers_the_entire_file() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("unsigned.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(&source, build_unsigned_pdf(&[])).expect("write the source");
    let source_bytes = std::fs::read(&source).expect("read the source");

    let report = sign_document_file(
        &source,
        &destination,
        &credential,
        &request(SignTarget::NewInvisibleField { name: None }),
    )
    .expect("signing succeeds");

    assert_eq!(report.field_name, "Signature1");
    assert_eq!(report.signature_count, 1);
    assert_eq!(report.attempts, 1);

    let output = std::fs::read(&destination).expect("read the output");
    assert_eq!(
        &output[..source_bytes.len()],
        &source_bytes[..],
        "signing must append, never rewrite"
    );
    assert_eq!(
        std::fs::read(&source).expect("re-read the source"),
        source_bytes,
        "the source is left untouched"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].field_name, "Signature1");
    assert!(statuses[0].intact);
    assert!(statuses[0].valid);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);
    assert!(!statuses[0].later_revisions);

    std::fs::write(oracle_fixture_path("apply-invisible-field.pdf"), &output)
        .expect("write the oracle fixture");
}

#[test]
fn countersigning_a_second_field_keeps_the_first_signature_valid() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("two-fields.pdf");
    let first_pass = directory.path().join("signed-once.pdf");
    let second_pass = directory.path().join("signed-twice.pdf");
    std::fs::write(&source, build_unsigned_pdf(&["Sig1", "Sig2"])).expect("write the source");

    let first = sign_document_file(
        &source,
        &first_pass,
        &credential,
        &request(SignTarget::ExistingField("Sig1".to_string())),
    )
    .expect("the first pass signs");
    assert_eq!(first.field_name, "Sig1");
    assert_eq!(first.signature_count, 1);

    // Pass two: countersigning. The document already carries a signature, and
    // the still-empty second field is the only thing that may be touched.
    let second = sign_document_file(
        &first_pass,
        &second_pass,
        &credential,
        &request(SignTarget::ExistingField("Sig2".to_string())),
    )
    .expect("the second pass countersigns");
    assert_eq!(second.field_name, "Sig2");
    assert_eq!(second.signature_count, 2);

    let after_first = std::fs::read(&first_pass).expect("read the first output");
    let output = std::fs::read(&second_pass).expect("read the second output");
    assert_eq!(
        &output[..after_first.len()],
        &after_first[..],
        "the countersignature must append to the first pass byte for byte"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 2);
    for status in &statuses {
        assert!(status.intact, "'{}' is not intact", status.field_name);
        assert!(status.valid, "'{}' is not valid", status.field_name);
    }
    let sig1 = statuses.iter().find(|s| s.field_name == "Sig1").unwrap();
    let sig2 = statuses.iter().find(|s| s.field_name == "Sig2").unwrap();
    assert_eq!(sig2.coverage, SignatureCoverage::EntireFile);
    assert_eq!(sig1.coverage, SignatureCoverage::EntireRevision);
    assert!(
        sig1.later_revisions,
        "the earlier signature must report that revisions follow it (§28.4)"
    );

    std::fs::write(oracle_fixture_path("apply-countersigned.pdf"), &output)
        .expect("write the oracle fixture");
}

#[test]
fn a_new_field_is_refused_on_an_already_signed_document() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("two-fields.pdf");
    let signed = directory.path().join("signed.pdf");
    let refused = directory.path().join("refused.pdf");
    std::fs::write(&source, build_unsigned_pdf(&["Sig1", "Sig2"])).expect("write the source");
    sign_document_file(
        &source,
        &signed,
        &credential,
        &request(SignTarget::ExistingField("Sig1".to_string())),
    )
    .expect("the first pass signs");

    let error = sign_document_file(
        &signed,
        &refused,
        &credential,
        &request(SignTarget::NewInvisibleField { name: None }),
    )
    .expect_err("creating a field on a signed document is a content change");

    assert!(
        matches!(error, SignApplyError::ContentChangeInAppendOnlyMode { .. }),
        "expected a content-change refusal, got {error:?}"
    );
    assert!(!refused.exists(), "nothing may be written on a refusal");
}

#[test]
fn signing_an_already_signed_field_is_refused_by_preflight() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("two-fields.pdf");
    let signed = directory.path().join("signed.pdf");
    let refused = directory.path().join("refused.pdf");
    std::fs::write(&source, build_unsigned_pdf(&["Sig1", "Sig2"])).expect("write the source");
    sign_document_file(
        &source,
        &signed,
        &credential,
        &request(SignTarget::ExistingField("Sig1".to_string())),
    )
    .expect("the first pass signs");

    let error = sign_document_file(
        &signed,
        &refused,
        &credential,
        &request(SignTarget::ExistingField("Sig1".to_string())),
    )
    .expect_err("a field that already has a /V may not be re-signed");

    match error {
        SignApplyError::Refused(verify::preflight::PreflightRefusal::FieldAlreadySigned {
            field,
        }) => assert_eq!(field, "Sig1"),
        other => panic!("expected FieldAlreadySigned, got {other:?}"),
    }
    assert!(!refused.exists());
}

#[test]
fn a_field_locked_by_fieldmdp_refuses_through_to_the_caller() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("locked.pdf");
    let refused = directory.path().join("refused.pdf");
    std::fs::write(&source, build_pdf_with_fieldmdp_lock()).expect("write the source");

    let error = sign_document_file(
        &source,
        &refused,
        &credential,
        &request(SignTarget::ExistingField("Sig2".to_string())),
    )
    .expect_err("a FieldMDP-locked field may not be signed");

    match error {
        SignApplyError::Refused(
            verify::preflight::PreflightRefusal::FieldLockedByPriorSignature { field, locked_by },
        ) => {
            assert_eq!(field, "Sig2");
            assert_eq!(locked_by, "Sig1");
        }
        other => panic!("expected FieldLockedByPriorSignature, got {other:?}"),
    }
    assert!(!refused.exists());
}

#[test]
fn a_corrupted_candidate_is_never_promoted() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("unsigned.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(&source, build_unsigned_pdf(&[])).expect("write the source");
    let source_bytes = std::fs::read(&source).expect("read the source");

    // Corrupt the CMS after signing and before the gate: flip one hex digit
    // inside the /Contents reservation, which is exactly the damage the gate
    // exists to catch.
    let error = sign_document_file_with_tamper(
        &source,
        &destination,
        &credential,
        &request(SignTarget::NewInvisibleField { name: None }),
        &|bytes: &mut Vec<u8>| {
            let start = bytes
                .windows(11)
                .position(|w| w == b"/Contents <")
                .expect("the reservation is there")
                + 11;
            bytes[start + 40] = if bytes[start + 40] == b'A' {
                b'B'
            } else {
                b'A'
            };
        },
    )
    .expect_err("a corrupted candidate must not be promoted");

    assert!(
        matches!(error, SignApplyError::PostSignVerificationFailed { .. }),
        "expected a gate failure, got {error:?}"
    );
    assert!(
        !destination.exists(),
        "the destination must not exist after a refused promotion"
    );
    assert_eq!(
        std::fs::read(&source).expect("re-read the source"),
        source_bytes,
        "the source is untouched"
    );
    // The temporary file was cleaned up: nothing hidden is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(directory.path())
        .expect("list the directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".pulpit-sign-"))
        .collect();
    assert!(leftovers.is_empty(), "a temporary file was left behind");
}
