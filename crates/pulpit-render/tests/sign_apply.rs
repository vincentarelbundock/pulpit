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
    sign_document_file, sign_document_file_with_tamper, AppearanceContent, AppearancePlacement,
    AppearanceRotation, SignAppearance, SignApplyError, SignRequest, SignTarget,
};
use pulpit_render::verify::{self, SignatureCoverage, SignatureVerification};
use signing_fixture::{
    build_pdf_with_fieldmdp_lock, build_unsigned_pdf, build_unsigned_pdf_multipage,
    build_unsigned_pdf_named, build_unsigned_pdf_pages, load_test_credential, skip_message,
    FixtureField, FixturePage, NameSpelling, SIGNING_TIME_UNIX,
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

/// A real form's field names are UTF-16BE, because that is what Acrobat
/// writes for anything that is not plain ASCII. The application only ever has
/// the UTF-8 name — that is what PDFium reports and what the operator sees —
/// so every step from pre-flight through field lookup to the verified report
/// has to agree that the two are the same name. They did not: the name
/// decoded to nothing, `SignTarget::ExistingField` found no such field, and a
/// document like the one this test is modelled on could not be signed at all.
#[test]
fn signing_into_a_utf16be_named_field_succeeds_and_verifies() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("accented-fields.pdf");
    let destination = directory.path().join("signed.pdf");
    // The same two spellings a producer chooses between, side by side.
    std::fs::write(
        &source,
        build_unsigned_pdf_named(&[
            ("Président-rapporteur", NameSpelling::Utf16Literal),
            ("Membre jury", NameSpelling::Utf16Hex),
        ]),
    )
    .expect("write the source");

    let outcome = sign_document_file(
        &source,
        &destination,
        &credential,
        &request(SignTarget::ExistingField(
            "Président-rapporteur".to_string(),
        )),
    )
    .expect("an accented field name is targetable");
    assert_eq!(outcome.field_name, "Président-rapporteur");
    assert_eq!(outcome.signature_count, 1);

    let output = std::fs::read(&destination).expect("read the output");
    let first_pass = statuses(&output);
    assert_eq!(first_pass.len(), 1);
    assert!(first_pass[0].intact, "the signature is not intact");
    assert!(first_pass[0].valid, "the signature is not valid");
    assert_eq!(first_pass[0].coverage, SignatureCoverage::EntireFile);
    assert_eq!(
        first_pass[0].field_name, "Président-rapporteur",
        "the verified report must name the field the way the operator does"
    );

    // And the field written as a hex string is reachable too, by
    // countersigning it in the same document.
    let countersigned = directory.path().join("countersigned.pdf");
    let second = sign_document_file(
        &destination,
        &countersigned,
        &credential,
        &request(SignTarget::ExistingField("Membre jury".to_string())),
    )
    .expect("a hex-spelled field name is targetable");
    assert_eq!(second.field_name, "Membre jury");
    assert_eq!(second.signature_count, 2);

    let output = std::fs::read(&countersigned).expect("read the countersigned output");
    let mut names: Vec<String> = statuses(&output)
        .into_iter()
        .map(|status| {
            assert!(
                status.intact && status.valid,
                "'{}' failed",
                status.field_name
            );
            status.field_name
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["Membre jury", "Président-rapporteur"]);

    std::fs::write(oracle_fixture_path("apply-utf16-field-name.pdf"), &output)
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

/// §25.5: a visible signature field carrying an ink appearance. Intact,
/// valid, `EntireFile` coverage, and the output bytes contain a
/// `/Subtype /Form` XObject whose stream carries the path operators and
/// whose `/BBox` matches the requested rect.
#[test]
fn a_visible_ink_signature_carries_a_form_xobject_appearance() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("unsigned.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(&source, build_unsigned_pdf(&[])).expect("write the source");

    let mut req = request(SignTarget::NewInvisibleField { name: None });
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::Rect {
            page_index: 0,
            rect: [72.0, 72.0, 272.0, 132.0],
        },
        content: AppearanceContent::Ink {
            strokes: vec![vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]],
            stroke_width: 1.5,
        },
    });

    let report = sign_document_file(&source, &destination, &credential, &req)
        .expect("signing with an ink appearance succeeds");
    assert_eq!(report.field_name, "Signature1");

    let output = std::fs::read(&destination).expect("read the output");
    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].intact);
    assert!(statuses[0].valid);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);

    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("/Subtype /Form"),
        "expected a form XObject in the output"
    );
    assert!(
        text.contains("/BBox [0 0 200 60]"),
        "expected the BBox to match the 200x60 rect, got: {text}"
    );
    assert!(text.contains(" m\n"), "expected a moveto path operator");
    assert!(text.contains(" l\n"), "expected a lineto path operator");
    assert!(text.contains("\nS\n"), "expected a stroke operator");
    assert!(
        text.contains("1 J 1 j 1.5 w"),
        "expected the line style operators"
    );

    std::fs::write(oracle_fixture_path("apply-visible-ink.pdf"), &output)
        .expect("write the oracle fixture");
}

/// §25.5: a visible signature field carrying a text appearance. `BT`/`ET`
/// bracket the text operators and the signer name is present, escaped as a
/// PDF literal string.
#[test]
fn a_visible_text_signature_carries_the_signer_name() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("unsigned.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(&source, build_unsigned_pdf(&[])).expect("write the source");

    let mut req = request(SignTarget::NewInvisibleField { name: None });
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::Rect {
            page_index: 0,
            rect: [72.0, 72.0, 272.0, 132.0],
        },
        content: AppearanceContent::Text {
            signer_name: "Ada Lovelace".to_string(),
            time_label: "2024-08-20 22:00 UTC".to_string(),
        },
    });

    let report = sign_document_file(&source, &destination, &credential, &req)
        .expect("signing with a text appearance succeeds");
    assert_eq!(report.field_name, "Signature1");

    let output = std::fs::read(&destination).expect("read the output");
    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].intact);
    assert!(statuses[0].valid);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);

    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("BT\n"), "expected a text object start");
    assert!(text.contains("ET\n"), "expected a text object end");
    assert!(
        text.contains("(Ada Lovelace) Tj"),
        "expected the signer name to appear as a Tj operand, got: {text}"
    );
    assert!(
        text.contains("/Font << /F0"),
        "expected the Helvetica font resource"
    );
}

/// The invisible path is unchanged when `appearance` is left `None`: no
/// `/AP` and the zero-size `/Rect` from before this feature existed.
#[test]
fn the_invisible_path_is_unchanged_without_an_appearance() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("unsigned.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(&source, build_unsigned_pdf(&[])).expect("write the source");

    let report = sign_document_file(
        &source,
        &destination,
        &credential,
        &request(SignTarget::NewInvisibleField { name: None }),
    )
    .expect("signing succeeds");
    assert_eq!(report.field_name, "Signature1");

    let output = std::fs::read(&destination).expect("read the output");
    let text = String::from_utf8_lossy(&output);
    assert!(
        !text.contains("/Subtype /Form"),
        "an invisible signature must not carry a form XObject appearance"
    );
}

/// §25.5: an appearance on a page that is not page 0. The widget must join
/// *that* page's `/Annots` — the last page of a three-page document — and no
/// other page may be re-emitted at all.
#[test]
fn a_visible_appearance_lands_on_the_page_it_names() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("three-pages.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(&source, build_unsigned_pdf_multipage(3, &[])).expect("write the source");

    let mut req = request(SignTarget::NewInvisibleField { name: None });
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::Rect {
            page_index: 2,
            rect: [300.0, 60.0, 540.0, 150.0],
        },
        content: AppearanceContent::Text {
            signer_name: "Ada Lovelace".to_string(),
            time_label: "2024-08-20 22:00 UTC".to_string(),
        },
    });

    let report = sign_document_file(&source, &destination, &credential, &req)
        .expect("signing on the last page succeeds");
    assert_eq!(report.field_name, "Signature1");

    let output = std::fs::read(&destination).expect("read the output");
    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].intact);
    assert!(statuses[0].valid);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);

    // The fixture numbers its pages 3, 4, 5 in order, so page index 2 is
    // object 5. That is the page the widget must have joined, resolved
    // through the output's own cross-reference chain.
    let third_page = String::from_utf8_lossy(
        pulpit_render::verify::find_object(&output, 5).expect("the third page resolves"),
    )
    .into_owned();
    assert!(
        third_page.contains("/Annots"),
        "the widget must join the third page's /Annots, got: {third_page}"
    );
    let first_page = String::from_utf8_lossy(
        pulpit_render::verify::find_object(&output, 3).expect("the first page resolves"),
    )
    .into_owned();
    assert!(
        !first_page.contains("/Annots"),
        "page 0 must be untouched, got: {first_page}"
    );

    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("/P 5 0 R"),
        "the widget's /P must name the third page"
    );
    assert!(
        text.contains("/BBox [0 0 240 90]"),
        "the BBox must match the 240x90 rect, got: {text}"
    );

    std::fs::write(oracle_fixture_path("apply-visible-last-page.pdf"), &output)
        .expect("write the oracle fixture");
}

/// §25.5 on a quarter-turned page: the box is the caller's, and the content
/// inside it has to be turned back, or the signature reads sideways.
///
/// A viewer draws the page's user space and then rotates the sheet. Content
/// dropped straight into the widget's box therefore arrives rotated with it.
/// The appearance stream answers with a `/Matrix` and a `/BBox` measured in
/// the box's *displayed* orientation — width and height exchanged for a
/// quarter turn — so the transformed bounding box measures the rect exactly
/// and the fit onto `/Rect` neither squashes nor stretches the mark.
#[test]
fn a_visible_appearance_counter_rotates_on_a_rotated_page() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("rotated.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(
        &source,
        build_unsigned_pdf_pages(&[FixturePage::rotated(90)], &[]),
    )
    .expect("write the source");

    // 70 wide by 200 tall in user space, which is 200 by 70 as displayed.
    let mut req = request(SignTarget::NewInvisibleField { name: None });
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::Cw90,
        placement: AppearancePlacement::Rect {
            page_index: 0,
            rect: [271.0, 296.0, 341.0, 496.0],
        },
        content: AppearanceContent::Ink {
            strokes: vec![vec![(0.05, 0.5), (0.95, 0.5)]],
            stroke_width: 6.0,
        },
    });

    let report = sign_document_file(&source, &destination, &credential, &req)
        .expect("signing a rotated page succeeds");
    assert_eq!(report.field_name, "Signature1");

    let output = std::fs::read(&destination).expect("read the output");
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("/BBox [0 0 200 70]"),
        "the BBox must be the box as displayed, 200x70, got: {text}"
    );
    assert!(
        text.contains("/Matrix [0 1 -1 0 0 0]"),
        "a /Rotate 90 page needs a +90° /Matrix so the mark stands upright, got: {text}"
    );
    // The stroke is normalized against the displayed box, so its x runs to
    // 0.95 * 200 = 190, not to 0.95 * 70.
    assert!(
        text.contains("190 35 l"),
        "the ink must be scaled against the displayed box, got: {text}"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].intact);
    assert!(statuses[0].valid);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);

    std::fs::write(
        oracle_fixture_path("apply-visible-rotated-page.pdf"),
        &output,
    )
    .expect("write the oracle fixture");
}

/// The other two quarter turns, and the upright page that must stay exactly
/// as it was: a rotation nobody asked for would change every existing file.
#[test]
fn the_appearance_matrix_matches_each_rotation() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let cases = [
        (AppearanceRotation::None, 0, None, "[0 0 70 200]"),
        (
            AppearanceRotation::Cw180,
            180,
            Some("/Matrix [-1 0 0 -1 0 0]"),
            "[0 0 70 200]",
        ),
        (
            AppearanceRotation::Cw270,
            270,
            Some("/Matrix [0 -1 1 0 0 0]"),
            "[0 0 200 70]",
        ),
    ];
    for (rotation, degrees, matrix, bbox) in cases {
        let directory = tempfile::tempdir().expect("temporary directory");
        let source = directory.path().join("page.pdf");
        let destination = directory.path().join("signed.pdf");
        std::fs::write(
            &source,
            build_unsigned_pdf_pages(&[FixturePage::rotated(degrees)], &[]),
        )
        .expect("write the source");

        let mut req = request(SignTarget::NewInvisibleField { name: None });
        req.appearance = Some(SignAppearance {
            page_rotation: rotation,
            placement: AppearancePlacement::Rect {
                page_index: 0,
                rect: [271.0, 296.0, 341.0, 496.0],
            },
            content: AppearanceContent::Ink {
                strokes: vec![vec![(0.05, 0.5), (0.95, 0.5)]],
                stroke_width: 6.0,
            },
        });
        sign_document_file(&source, &destination, &credential, &req).expect("signing succeeds");
        let output = std::fs::read(&destination).expect("read the output");
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains(&format!("/BBox {bbox}")),
            "{rotation:?} wanted /BBox {bbox}, got: {text}"
        );
        match matrix {
            Some(matrix) => assert!(
                text.contains(matrix),
                "{rotation:?} wanted {matrix}, got: {text}"
            ),
            None => assert!(
                !text.contains("/Matrix"),
                "an upright page must carry no /Matrix at all, got: {text}"
            ),
        }
    }
}

/// A page index the document does not have is a typed refusal naming the real
/// page count, not a silently wrong page.
#[test]
fn an_appearance_on_a_page_the_document_lacks_is_refused() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("two-pages.pdf");
    let refused = directory.path().join("refused.pdf");
    std::fs::write(&source, build_unsigned_pdf_multipage(2, &[])).expect("write the source");

    let mut req = request(SignTarget::NewInvisibleField { name: None });
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::Rect {
            page_index: 7,
            rect: [10.0, 10.0, 110.0, 60.0],
        },
        content: AppearanceContent::Text {
            signer_name: "Ada Lovelace".to_string(),
            time_label: "2024".to_string(),
        },
    });

    let error = sign_document_file(&source, &refused, &credential, &req)
        .expect_err("page 7 of a two-page document does not exist");
    match error {
        SignApplyError::AppearancePlacement(detail) => {
            assert!(detail.contains("page index 7"), "{detail}");
            assert!(detail.contains("2 page(s)"), "{detail}");
            assert!(detail.contains("between 0 and 1"), "{detail}");
        }
        other => panic!("expected an appearance-placement refusal, got {other:?}"),
    }
    assert!(!refused.exists(), "nothing may be written on a refusal");
}

/// `FieldRect` means "inside the box the field already has". A field being
/// created has none, so asking for it is caller misuse and is refused before
/// anything is assembled.
#[test]
fn a_field_rect_appearance_is_refused_for_a_new_field() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("unsigned.pdf");
    let refused = directory.path().join("refused.pdf");
    std::fs::write(&source, build_unsigned_pdf(&[])).expect("write the source");

    let mut req = request(SignTarget::NewInvisibleField { name: None });
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::FieldRect,
        content: AppearanceContent::Text {
            signer_name: "Ada Lovelace".to_string(),
            time_label: "2024".to_string(),
        },
    });

    let error = sign_document_file(&source, &refused, &credential, &req)
        .expect_err("a new field has no rect to draw inside");
    match error {
        SignApplyError::AppearancePlacement(detail) => {
            assert!(detail.contains("FieldRect"), "{detail}");
            assert!(detail.contains("AppearancePlacement::Rect"), "{detail}");
        }
        other => panic!("expected an appearance-placement refusal, got {other:?}"),
    }
    assert!(!refused.exists());
}

/// The main sign-here flow: an existing empty field on the last page, with a
/// box its author drew. The appearance is drawn inside that box and `/Rect`
/// is left exactly as the sender wrote it — the field does not move.
#[test]
fn a_field_rect_appearance_keeps_the_fields_own_box() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("contract.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(
        &source,
        build_unsigned_pdf_multipage(
            3,
            &[FixtureField {
                name: "Signature",
                page: 2,
                rect: [100.0, 100.0, 340.0, 190.0],
            }],
        ),
    )
    .expect("write the source");

    let mut req = request(SignTarget::ExistingField("Signature".to_string()));
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::FieldRect,
        content: AppearanceContent::InkAndText {
            strokes: vec![vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]],
            stroke_width: 1.5,
            signer_name: "Ada Lovelace".to_string(),
            time_label: "2024-08-20 22:00 UTC".to_string(),
        },
    });

    let report = sign_document_file(&source, &destination, &credential, &req)
        .expect("signing into the sender's box succeeds");
    assert_eq!(report.field_name, "Signature");

    let output = std::fs::read(&destination).expect("read the output");
    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].intact);
    assert!(statuses[0].valid);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);

    // The fixture's field is object 7: 1 catalog, 2 page tree, 3..5 pages,
    // 6 AcroForm, 7 the field.
    let field = String::from_utf8_lossy(
        pulpit_render::verify::find_object(&output, 7).expect("the field resolves"),
    )
    .into_owned();
    assert!(
        field.contains("/Rect [100 100 340 190]"),
        "the sender's own rect must survive signing, got: {field}"
    );
    assert!(
        field.contains("/AP <</N "),
        "the widget must point at the appearance stream, got: {field}"
    );

    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("/BBox [0 0 240 90]"),
        "the BBox must match the field's own 240x90 box, got: {text}"
    );
    assert!(
        text.contains("(Ada Lovelace) Tj"),
        "expected the signer name inside the appearance"
    );
    // The page is not re-emitted at all: the widget is already in its /Annots.
    let appended =
        String::from_utf8_lossy(&output[std::fs::metadata(&source).unwrap().len() as usize..]);
    assert!(
        !appended.contains("5 0 obj"),
        "an existing field's page must not be rewritten, got: {appended}"
    );

    std::fs::write(oracle_fixture_path("apply-field-rect.pdf"), &output)
        .expect("write the oracle fixture");
}

/// The same placement on the countersigning path: a second empty field on a
/// signed document, drawn inside its own box, with the first signature still
/// intact afterwards.
#[test]
fn a_field_rect_countersignature_keeps_the_first_signature_valid() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("contract.pdf");
    let first_pass = directory.path().join("signed-once.pdf");
    let second_pass = directory.path().join("signed-twice.pdf");
    std::fs::write(
        &source,
        build_unsigned_pdf_multipage(
            2,
            &[
                FixtureField {
                    name: "Sender",
                    page: 0,
                    rect: [72.0, 600.0, 272.0, 660.0],
                },
                FixtureField {
                    name: "Recipient",
                    page: 1,
                    rect: [72.0, 100.0, 312.0, 190.0],
                },
            ],
        ),
    )
    .expect("write the source");

    sign_document_file(
        &source,
        &first_pass,
        &credential,
        &request(SignTarget::ExistingField("Sender".to_string())),
    )
    .expect("the first pass signs");

    let mut req = request(SignTarget::ExistingField("Recipient".to_string()));
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::FieldRect,
        content: AppearanceContent::Ink {
            strokes: vec![vec![(0.1, 0.1), (0.9, 0.9)]],
            stroke_width: 2.0,
        },
    });
    let second = sign_document_file(&first_pass, &second_pass, &credential, &req)
        .expect("the countersignature draws into the recipient's box");
    assert_eq!(second.field_name, "Recipient");
    assert_eq!(second.signature_count, 2);

    let after_first = std::fs::read(&first_pass).expect("read the first output");
    let output = std::fs::read(&second_pass).expect("read the second output");
    assert_eq!(
        &output[..after_first.len()],
        &after_first[..],
        "the countersignature must append byte for byte"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 2);
    for status in &statuses {
        assert!(status.intact, "'{}' is not intact", status.field_name);
        assert!(status.valid, "'{}' is not valid", status.field_name);
    }
    let sender = statuses.iter().find(|s| s.field_name == "Sender").unwrap();
    let recipient = statuses
        .iter()
        .find(|s| s.field_name == "Recipient")
        .unwrap();
    assert_eq!(recipient.coverage, SignatureCoverage::EntireFile);
    assert_eq!(sender.coverage, SignatureCoverage::EntireRevision);

    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("/BBox [0 0 240 90]"),
        "the BBox must match the recipient field's own box, got: {text}"
    );

    std::fs::write(
        oracle_fixture_path("apply-field-rect-countersigned.pdf"),
        &output,
    )
    .expect("write the oracle fixture");
}

/// A placeholder `/Rect [0 0 0 0]` would produce an appearance stream nothing
/// can render. That is refused, with the invisible fallback named, rather
/// than written out as a signature that shows nothing.
#[test]
fn a_degenerate_field_rect_is_refused_with_the_invisible_fallback_named() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("placeholder-field.pdf");
    let refused = directory.path().join("refused.pdf");
    // `build_unsigned_pdf` writes the invisible placeholder rect [0 0 0 0].
    std::fs::write(&source, build_unsigned_pdf(&["Sig1"])).expect("write the source");

    let mut req = request(SignTarget::ExistingField("Sig1".to_string()));
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::FieldRect,
        content: AppearanceContent::Text {
            signer_name: "Ada Lovelace".to_string(),
            time_label: "2024".to_string(),
        },
    });

    let error = sign_document_file(&source, &refused, &credential, &req)
        .expect_err("a zero-area rect cannot carry a visible appearance");
    match error {
        SignApplyError::AppearancePlacement(detail) => {
            assert!(detail.contains("Sig1"), "{detail}");
            assert!(detail.contains("zero-area"), "{detail}");
            assert!(detail.contains("without an appearance"), "{detail}");
        }
        other => panic!("expected an appearance-placement refusal, got {other:?}"),
    }
    assert!(!refused.exists(), "nothing may be written on a refusal");
}

/// `Rect` keeps its old meaning for an existing field: the box moves to where
/// the caller says, which is what the preset-placement flow relies on.
#[test]
fn an_explicit_rect_still_overwrites_an_existing_fields_box() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("contract.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(
        &source,
        build_unsigned_pdf_multipage(
            1,
            &[FixtureField {
                name: "Sig1",
                page: 0,
                rect: [100.0, 100.0, 340.0, 190.0],
            }],
        ),
    )
    .expect("write the source");

    let mut req = request(SignTarget::ExistingField("Sig1".to_string()));
    req.appearance = Some(SignAppearance {
        page_rotation: AppearanceRotation::None,
        placement: AppearancePlacement::Rect {
            page_index: 0,
            rect: [10.0, 20.0, 110.0, 70.0],
        },
        content: AppearanceContent::Text {
            signer_name: "Ada Lovelace".to_string(),
            time_label: "2024".to_string(),
        },
    });

    sign_document_file(&source, &destination, &credential, &req).expect("signing succeeds");
    let output = std::fs::read(&destination).expect("read the output");
    assert!(statuses(&output)[0].valid);

    // 1 catalog, 2 page tree, 3 page, 4 AcroForm, 5 the field.
    let field = String::from_utf8_lossy(
        pulpit_render::verify::find_object(&output, 5).expect("the field resolves"),
    )
    .into_owned();
    assert!(
        field.contains("/Rect [10 20 110 70]"),
        "an explicit rect must overwrite the field's box, got: {field}"
    );
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("/BBox [0 0 100 50]"),
        "the BBox must match the explicit rect, got: {text}"
    );
}

/// `SignRequest::default()` leaves `/ID`'s second element all zero, because
/// this crate never draws randomness and a `Default` has to put something
/// there. Writing it would give every document signed from a defaulted request
/// the same `/ID` — which is what §14.4 uses to tell revisions apart. A caller
/// that forgot to set it is refused, not obliged.
#[test]
fn an_unset_id_second_element_is_refused_rather_than_written() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("unsigned.pdf");
    let destination = directory.path().join("signed.pdf");
    std::fs::write(&source, build_unsigned_pdf(&[])).expect("write the source");

    let mut unset = request(SignTarget::NewInvisibleField { name: None });
    unset.id2 = [0u8; 16];

    let error = sign_document_file(&source, &destination, &credential, &unset)
        .expect_err("an all-zero /ID second element must be refused");
    let message = error.to_string();
    assert!(
        message.contains("all zero") && message.contains("unchanged"),
        "the refusal must name the unset /ID and say the source is unchanged, got: {message}"
    );
    assert!(
        !destination.exists(),
        "nothing may be written when the request is refused"
    );

    // The same request with real randomness signs.
    let set = request(SignTarget::NewInvisibleField { name: None });
    sign_document_file(&source, &destination, &credential, &set).expect("a set /ID signs");
    assert!(destination.exists());
}
