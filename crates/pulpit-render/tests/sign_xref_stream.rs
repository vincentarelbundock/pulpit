//! Signing documents whose cross-reference structure is a PDF 1.5+ one:
//! cross-reference streams, and objects packed into object streams.
//!
//! These are not an exotic corner. LaTeX (`pdftex` with compression on),
//! Chrome's print-to-PDF, Ghostscript and Acrobat's "optimized" save all
//! produce them, so a signer that only understands a classic `xref` table
//! cannot sign the majority of the documents it will be handed.
//!
//! The end-to-end tests need a real key and skip with a message when the
//! oracle's credential has not been generated, the same way `sign_apply.rs`
//! does. The parsing tests need nothing and always run.

mod signing_fixture;

use pulpit_render::sign::apply::{sign_document_file, SignRequest, SignTarget};
use pulpit_render::verify::preflight::PreflightRefusal;
use pulpit_render::verify::{self, SignatureCoverage};
use signing_fixture::{
    build_unsigned_pdf_shaped, load_test_credential, oracle_fixture_path, skip_message, statuses,
    XrefShape,
};
use std::path::{Path, PathBuf};

fn request(field: SignTarget) -> SignRequest {
    signing_fixture::request_because(field, "Cross-reference stream test")
}

/// `examples/beamer.pdf` is a committed LaTeX/beamer deck whose last
/// cross-reference section is an xref stream. It carries no form at all, so it
/// exercises the "create a new invisible field in a 1.5+ document" path.
fn beamer_path() -> PathBuf {
    PathBuf::from("../../examples/beamer.pdf")
}

// ---------------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------------

#[test]
fn a_beamer_deck_with_an_xref_stream_signs_and_verifies() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let source = beamer_path();
    assert!(source.exists(), "examples/beamer.pdf is committed");
    let source_bytes = std::fs::read(&source).expect("read the deck");

    let directory = tempfile::tempdir().expect("temporary directory");
    let destination = directory.path().join("beamer-signed.pdf");

    let report = sign_document_file(
        &source,
        &destination,
        &credential,
        &request(SignTarget::NewInvisibleField { name: None }),
    )
    .expect("signing a deck indexed by an xref stream succeeds");
    assert_eq!(report.signature_count, 1);

    let output = std::fs::read(&destination).expect("read the output");
    assert_eq!(
        &output[..source_bytes.len()],
        &source_bytes[..],
        "signing must append, never rewrite"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);
    assert!(statuses[0].intact, "the digest must match");

    std::fs::write(oracle_fixture_path("xref-stream-beamer.pdf"), &output)
        .expect("write the oracle fixture");
}

#[test]
fn a_document_in_object_streams_signs_into_its_existing_field() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("objstm.pdf");
    let destination = directory.path().join("objstm-signed.pdf");
    let source_bytes = build_unsigned_pdf_shaped(&["Signature1"], XrefShape::ObjectStreams);
    std::fs::write(&source, &source_bytes).expect("write the source");

    let report = sign_document_file(
        &source,
        &destination,
        &credential,
        &request(SignTarget::ExistingField("Signature1".to_string())),
    )
    .expect("signing a document whose objects live in an object stream succeeds");
    assert_eq!(report.field_name, "Signature1");

    let output = std::fs::read(&destination).expect("read the output");
    assert_eq!(
        &output[..source_bytes.len()],
        &source_bytes[..],
        "signing must append, never rewrite"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);
    assert!(statuses[0].intact);

    std::fs::write(oracle_fixture_path("objstm-signed.pdf"), &output)
        .expect("write the oracle fixture");
}

#[test]
fn a_plain_xref_stream_document_signs_into_its_existing_field() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("xrefstream.pdf");
    let destination = directory.path().join("xrefstream-signed.pdf");
    std::fs::write(
        &source,
        build_unsigned_pdf_shaped(&["Signature1"], XrefShape::XrefStream),
    )
    .expect("write the source");

    sign_document_file(
        &source,
        &destination,
        &credential,
        &request(SignTarget::ExistingField("Signature1".to_string())),
    )
    .expect("signing a document indexed by an xref stream succeeds");

    let output = std::fs::read(&destination).expect("read the output");
    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].intact);
}

#[test]
fn countersigning_an_object_stream_document_leaves_the_first_signature_intact() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("two-fields.pdf");
    let first = directory.path().join("first.pdf");
    let second = directory.path().join("second.pdf");
    std::fs::write(
        &source,
        build_unsigned_pdf_shaped(&["Sig1", "Sig2"], XrefShape::ObjectStreams),
    )
    .expect("write the source");

    sign_document_file(
        &source,
        &first,
        &credential,
        &request(SignTarget::ExistingField("Sig1".to_string())),
    )
    .expect("the first signature succeeds");
    let first_bytes = std::fs::read(&first).expect("read the first output");

    sign_document_file(
        &first,
        &second,
        &credential,
        &request(SignTarget::ExistingField("Sig2".to_string())),
    )
    .expect("countersigning succeeds");

    let output = std::fs::read(&second).expect("read the countersigned output");
    assert_eq!(
        &output[..first_bytes.len()],
        &first_bytes[..],
        "countersigning must append, never rewrite"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 2, "both signatures are discovered");
    for status in &statuses {
        assert!(
            status.intact,
            "signature '{}' must stay intact",
            status.field_name
        );
    }
    // The first signature covers only its own revision once a second is
    // appended; the second covers the whole file.
    assert_eq!(statuses[1].coverage, SignatureCoverage::EntireFile);

    std::fs::write(oracle_fixture_path("objstm-countersigned.pdf"), &output)
        .expect("write the oracle fixture");
}

// ---------------------------------------------------------------------------
// Pre-flight, which runs before any cryptography and needs no credential
// ---------------------------------------------------------------------------

#[test]
fn preflight_sees_a_field_that_lives_in_an_object_stream() {
    let bytes = build_unsigned_pdf_shaped(&["Signature1"], XrefShape::ObjectStreams);
    let ok = verify::preflight::preflight_sign(&bytes, Some("Signature1"))
        .expect("pre-flight reads a form packed into an object stream");
    assert_eq!(ok.target_field, "Signature1");
}

#[test]
fn preflight_sees_a_field_indexed_by_an_xref_stream() {
    let bytes = build_unsigned_pdf_shaped(&["Signature1"], XrefShape::XrefStream);
    let ok = verify::preflight::preflight_sign(&bytes, Some("Signature1"))
        .expect("pre-flight reads a form indexed by an xref stream");
    assert_eq!(ok.target_field, "Signature1");
}

/// A deck with no form has no empty signature field to target, so pre-flight
/// refuses — but it must refuse *on that ground*. The failure this replaces
/// was `InvalidState("the catalog object could not be read")`: pre-flight
/// could not read the catalog at all, because it lived in an object stream.
/// The distinction is the whole point — one refusal describes the document,
/// the other admits the reader could not parse it.
#[test]
fn preflight_reads_a_beamer_deck_and_refuses_on_the_document_not_the_parse() {
    let bytes = std::fs::read(beamer_path()).expect("read the deck");
    match verify::preflight::preflight_sign(&bytes, None) {
        Err(PreflightRefusal::NoEmptySignatureField) => {}
        other => panic!("expected NoEmptySignatureField, got {other:?}"),
    }
}

/// The three cross-reference shapes describe the same document, so pre-flight
/// must reach the same conclusion about all three.
#[test]
fn every_cross_reference_shape_yields_the_same_preflight() {
    for shape in [
        XrefShape::ClassicTable,
        XrefShape::XrefStream,
        XrefShape::ObjectStreams,
    ] {
        let bytes = build_unsigned_pdf_shaped(&["Signature1"], shape);
        let ok = verify::preflight::preflight_sign(&bytes, Some("Signature1"))
            .unwrap_or_else(|e| panic!("pre-flight fails for {shape:?}: {e:?}"));
        assert_eq!(ok.target_field, "Signature1", "{shape:?}");
    }
}

// ---------------------------------------------------------------------------
// Fuzz seeds
// ---------------------------------------------------------------------------

/// Write the cross-reference-stream and object-stream seed corpus.
///
/// The seeds are generated from the same builders the tests use rather than
/// committed as opaque blobs, so a seed can never drift into describing a
/// shape the fixture builder no longer produces. Truncated and cyclic
/// variants are derived from them: the two shapes that turn a chain-follower
/// into a non-terminating one.
#[test]
fn fuzz_seeds_for_the_new_cross_reference_shapes_are_written() {
    let seeds = Path::new("fuzz/seeds");
    if !seeds.is_dir() {
        eprintln!("SKIP: fuzz/seeds is absent");
        return;
    }

    let xref_stream = build_unsigned_pdf_shaped(&["Signature1"], XrefShape::XrefStream);
    let objstm = build_unsigned_pdf_shaped(&["Signature1"], XrefShape::ObjectStreams);

    // Truncated: the cross-reference stream's startxref points at bytes that
    // are no longer there.
    let truncated = objstm[..objstm.len() * 3 / 4].to_vec();

    // Cyclic: an xref stream whose /Prev points at itself.
    let mut cyclic = Vec::from(&b"%PDF-1.5\n"[..]);
    let at = cyclic.len();
    let body = vec![0u8; 7];
    cyclic.extend_from_slice(
        format!(
            "1 0 obj\n<</Type /XRef /Size 2 /W [1 4 2] /Root 1 0 R /Prev {at} /Length {}>>\n\
             stream\n",
            body.len()
        )
        .as_bytes(),
    );
    cyclic.extend_from_slice(&body);
    cyclic.extend_from_slice(b"\nendstream\nendobj\n");
    cyclic.extend_from_slice(format!("startxref\n{at}\n%%EOF").as_bytes());

    let corpus: [(&str, &[u8]); 4] = [
        ("xref-stream.pdf", &xref_stream),
        ("objstm.pdf", &objstm),
        ("objstm-truncated.pdf", &truncated),
        ("xref-stream-cyclic-prev.pdf", &cyclic),
    ];

    for target in ["fuzz_revision_map", "fuzz_discover", "fuzz_verify_full"] {
        let directory = seeds.join(target);
        std::fs::create_dir_all(&directory).expect("create the seed directory");
        for (name, bytes) in &corpus {
            std::fs::write(directory.join(name), bytes).expect("write the seed");
        }
    }

    // Every seed must leave the parsers terminating rather than panicking,
    // which is the property the fuzzer is there to keep.
    for (_, bytes) in &corpus {
        let _ = verify::verify_signatures(bytes);
    }
}

// ---------------------------------------------------------------------------
// A real producer's output
// ---------------------------------------------------------------------------

/// `mutool clean -Z` rewrites a document into object streams the way a real
/// optimizer does. The hand-written fixtures above pin the shape pulpit's own
/// builder produces; this pins the shape somebody else's does, which is the
/// only way to catch a reader that has quietly been fitted to its own writer.
///
/// Skipped with a message when mutool is not installed, so the suite stays
/// green on a machine without it — at the cost of not running this check.
#[test]
fn a_document_rewritten_by_mutool_into_object_streams_signs_and_verifies() {
    let Some(credential) = load_test_credential() else {
        skip_message();
        return;
    };
    let directory = tempfile::tempdir().expect("temporary directory");
    let plain = directory.path().join("plain.pdf");
    let packed = directory.path().join("packed.pdf");
    let destination = directory.path().join("packed-signed.pdf");
    std::fs::write(
        &plain,
        build_unsigned_pdf_shaped(&["Signature1"], XrefShape::ClassicTable),
    )
    .expect("write the source");

    match std::process::Command::new("mutool")
        .arg("clean")
        .arg("-Z")
        .arg(&plain)
        .arg(&packed)
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            eprintln!(
                "SKIP: mutool clean -Z failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        Err(e) => {
            eprintln!("SKIP: mutool is not installed ({e}); install mupdf-tools to run this");
            return;
        }
    }

    let packed_bytes = std::fs::read(&packed).expect("read mutool's output");
    // mutool writes `/Type/ObjStm` with no space. It may also decline to pack
    // a document this small, in which case there is nothing here to test and
    // saying so is better than asserting on another tool's heuristics.
    if !packed_bytes.windows(6).any(|w| w == b"ObjStm") {
        eprintln!("SKIP: mutool clean -Z did not pack this document into object streams");
        return;
    }

    // Pre-flight must see the field through mutool's packing.
    let ok = verify::preflight::preflight_sign(&packed_bytes, Some("Signature1"))
        .expect("pre-flight reads a mutool-packed form");
    assert_eq!(ok.target_field, "Signature1");

    sign_document_file(
        &packed,
        &destination,
        &credential,
        &request(SignTarget::ExistingField("Signature1".to_string())),
    )
    .expect("signing a mutool-packed document succeeds");

    let output = std::fs::read(&destination).expect("read the output");
    assert_eq!(
        &output[..packed_bytes.len()],
        &packed_bytes[..],
        "signing must append, never rewrite"
    );

    let statuses = statuses(&output);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].coverage, SignatureCoverage::EntireFile);
    assert!(statuses[0].intact);

    std::fs::write(oracle_fixture_path("mutool-objstm-signed.pdf"), &output)
        .expect("write the oracle fixture");
}
