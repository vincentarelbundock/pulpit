//! What happens when things go wrong (§13.5).
//!
//! The engine's happy path is covered elsewhere. These are the cases the
//! specification names as the ones that must not lose a document, duplicate an
//! edit, or take the process down: a worker that dies at each of the three
//! moments it can, a stale revision, a disk that will not take the file, a
//! document that refuses to be changed, and annotation data that is corrupt in
//! each of the ways a file can carry.
//!
//! Most of it runs against the memory engine, on purpose. A test that can only
//! run where PDFium is installed is a test that does not run in most places,
//! and none of these behaviours is PDFium's — they are the engine's, the
//! session's and the protocol's.

use std::path::{Path, PathBuf};

use pulpit_core::annotate::{
    AnnotationCommand, AnnotationDraft, AnnotationId, IdGenerator, InkDraft, InkPoint, MarkStyle,
};
use pulpit_core::page::PageIndex;
use pulpit_render::document::memory::MemoryDocument;
use pulpit_render::document::protocol::{
    DocumentFailure, DocumentRequest, DocumentResponse, SaveRequest,
};
use pulpit_render::document::worker::{serve, DocumentWorker};
use pulpit_render::document::{
    AppliedEffect, DocumentError, DocumentRevision, DocumentTransaction, PdfDocument, SaveOptions,
};

fn stroke(x: f32) -> DocumentTransaction {
    DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
        InkDraft {
            page: PageIndex(0),
            points: vec![InkPoint::new(x, 20.0), InkPoint::new(x + 60.0, 80.0)],
            style: MarkStyle::default(),
        },
    ))])
}

fn document() -> PdfDocument<'static> {
    PdfDocument::new(Box::new(MemoryDocument::letter(2)), 77)
}

#[test]
fn a_stale_revision_performs_no_mutation_at_all() {
    // §9.5: the check is what stops a delayed message from overwriting a
    // later change, and "no mutation" has to mean none — not a partly
    // applied one that happens to report failure.
    let mut document = document();
    document
        .apply(DocumentRevision::INITIAL, stroke(20.0))
        .unwrap();
    let before = document.annotations(PageIndex(0)).unwrap();

    let error = document
        .apply(DocumentRevision::INITIAL, stroke(200.0))
        .unwrap_err();
    assert!(matches!(error, DocumentError::RevisionConflict { .. }));
    assert_eq!(
        document.annotations(PageIndex(0)).unwrap(),
        before,
        "a refused transaction changed the document"
    );
    assert_eq!(document.revision(), DocumentRevision(1));
}

#[test]
fn a_document_that_refuses_to_be_changed_refuses_every_edit() {
    let mut document = PdfDocument::new(Box::new(MemoryDocument::locked()), 1);
    for transaction in [stroke(20.0), stroke(200.0)] {
        assert!(matches!(
            document.apply(document.revision(), transaction),
            Err(DocumentError::MutationForbidden)
        ));
    }
    assert_eq!(document.revision(), DocumentRevision::INITIAL);
    assert!(!document.is_dirty(), "a refused edit must not dirty");
}

#[test]
fn a_save_that_cannot_be_written_leaves_the_document_alone() {
    // A full disk and a directory that is not writable are the same case from
    // the engine's side: the write fails and the open document is untouched,
    // so the user can choose somewhere else and try again.
    let mut document = document();
    document
        .apply(DocumentRevision::INITIAL, stroke(20.0))
        .unwrap();

    let nowhere = PathBuf::from("/proc/pulpit-cannot-write-here/out.pdf");
    let error = document.save_as(&nowhere, SaveOptions::verified());
    assert!(error.is_err(), "a save to nowhere reported success");
    assert_eq!(document.revision(), DocumentRevision(1));
    assert_eq!(
        document.annotations(PageIndex(0)).unwrap().len(),
        1,
        "a failed save changed the document"
    );

    // …and a good destination still works afterwards, so the failure is not
    // sticky.
    let directory = tempfile::tempdir().unwrap();
    let good = directory.path().join("out.pdf");
    assert!(document.save_as(&good, SaveOptions::verified()).is_ok());
}

#[test]
fn a_transaction_that_fails_part_way_leaves_none_of_itself_applied() {
    // §9.5's atomicity, at the one point it is hard: the failure cannot be
    // caught by the pre-check, because the first command is what makes the
    // second illegal.
    let mut document = document();
    let applied = document
        .apply(DocumentRevision::INITIAL, stroke(20.0))
        .unwrap();
    let AppliedEffect::Annotation(summary) = &applied.effects[0] else {
        panic!("expected an annotation")
    };
    let id = summary.id.clone();

    let doomed = DocumentTransaction::from_annotations([
        AnnotationCommand::Delete { id: id.clone() },
        AnnotationCommand::Delete { id },
    ]);
    assert!(document.apply(document.revision(), doomed).is_err());
    assert_eq!(
        document.annotations(PageIndex(0)).unwrap().len(),
        1,
        "half a transaction survived"
    );
}

#[test]
fn a_worker_that_dies_before_answering_is_not_a_mutation_that_happened() {
    // §11.5: after a crash the supervisor replays only *confirmed* entries.
    // The proof that it can is that an unanswered request is distinguishable
    // from a refused one at the point the answer would have arrived.
    let mut worker = DocumentWorker::new();
    worker.adopt(document());

    // A request the worker never sees, because its input ended first. The
    // loop returns cleanly; nothing is half-applied, and nothing says the
    // edit happened.
    let mut answers = Vec::new();
    let outcome = serve(std::io::Cursor::new(Vec::new()), &mut answers, worker);
    assert!(outcome.is_ok(), "a closed pipe is not an error");

    // The handshake and nothing else: no `Applied`, so no caller could
    // mistake this for a committed edit.
    let mut reader = std::io::Cursor::new(answers);
    let _hello: pulpit_render::document::worker::Hello =
        pulpit_render::protocol::read_message(&mut reader).unwrap();
    assert!(
        pulpit_render::protocol::read_message::<DocumentResponse>(&mut reader).is_err(),
        "the worker answered something it was never asked"
    );
}

#[test]
fn a_worker_with_no_document_refuses_every_request_rather_than_falling_over() {
    // The shape of a worker whose document went: every request is answered,
    // and every answer is a refusal.
    let mut worker = DocumentWorker::new();
    let id = IdGenerator::new(0).next_id();
    let requests = [
        DocumentRequest::Info,
        DocumentRequest::ListAnnotations { page: PageIndex(0) },
        DocumentRequest::GetAnnotation { id: id.clone() },
        DocumentRequest::ListFields,
        DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(20.0),
        },
        DocumentRequest::SaveAs(SaveRequest {
            destination: PathBuf::from("/tmp/never.pdf"),
            options: SaveOptions::verified(),
        }),
    ];
    for request in requests {
        let response = worker.handle(request);
        assert!(
            matches!(response, DocumentResponse::Failed(_)),
            "a worker with nothing open answered {response:?}"
        );
    }
    let _ = id;
}

#[test]
fn an_answer_larger_than_the_protocol_allows_is_refused_on_both_sides() {
    // A8 and §9.5: the bound is checked where the message is built and again
    // where it is read, because supervisor and worker are separate processes.
    let huge = DocumentTransaction(vec![
        pulpit_render::document::DocumentCommand::Annotation(
            AnnotationCommand::Create(AnnotationDraft::Ink(InkDraft {
                page: PageIndex(0),
                points: vec![InkPoint::new(1.0, 1.0); 4],
                style: MarkStyle::default(),
            }))
        );
        pulpit_render::document::limits::MAX_OPERATIONS_PER_TRANSACTION
            + 1
    ]);
    let request = DocumentRequest::Apply {
        expected_revision: DocumentRevision::INITIAL,
        transaction: huge,
    };
    // Sending side.
    assert!(request.validate().is_err());

    // Receiving side, for a worker that got it anyway.
    let mut worker = DocumentWorker::new();
    worker.adopt(document());
    assert!(matches!(
        worker.handle(request),
        DocumentResponse::Failed(DocumentFailure::Refused(_))
    ));
}

#[test]
fn a_refusal_and_a_lost_worker_are_told_apart() {
    // The whole of §11.5 turns on this: a read-only request may be retried
    // against a fresh worker, and a mutation may not be assumed committed.
    assert!(DocumentFailure::Engine("worker died".into()).is_retryable());
    for refusal in [
        DocumentFailure::Refused("off the page".into()),
        DocumentFailure::NotFound("page 9".into()),
        DocumentFailure::RevisionConflict {
            expected: DocumentRevision(1),
            actual: DocumentRevision(2),
        },
    ] {
        assert!(!refusal.is_retryable(), "{refusal:?}");
        assert!(!refusal.message().is_empty());
    }
}

#[test]
fn an_annotation_that_is_not_there_is_a_refusal_rather_than_a_guess() {
    // §11.4: recovery reports a conflict rather than applying a change to a
    // guessed target, and the engine is where that rule lives.
    let mut document = document();
    let absent = IdGenerator::new(999).next_id();
    let error = document
        .apply(
            DocumentRevision::INITIAL,
            DocumentTransaction::from_annotations([AnnotationCommand::Delete {
                id: absent.clone(),
            }]),
        )
        .unwrap_err();
    assert!(matches!(error, DocumentError::NoSuchAnnotation(_)));
    assert_eq!(document.revision(), DocumentRevision::INITIAL);
}

#[test]
fn an_unsupported_annotation_is_never_erased_or_edited() {
    // A5: an annotation pulpit does not understand is preserved. The eraser
    // is the obvious way to lose one, so it is the one that is checked.
    let mut engine = MemoryDocument::letter(1);
    let id = AnnotationId::imported("another-producer").unwrap();
    engine.add_imported(
        PageIndex(0),
        id.clone(),
        pulpit_render::document::AnnotationSupport::Unsupported,
        pulpit_core::page::PageRect::new(10.0, 10.0, 60.0, 60.0),
        b"<< /Vendor (private) >>".to_vec(),
    );
    let mut document = PdfDocument::new(Box::new(engine), 3);

    let error = document
        .apply(
            DocumentRevision::INITIAL,
            DocumentTransaction::from_annotations([AnnotationCommand::Delete { id: id.clone() }]),
        )
        .unwrap_err();
    assert!(matches!(error, DocumentError::NotEditable(_)));
    assert_eq!(
        document.annotations(PageIndex(0)).unwrap().len(),
        1,
        "an annotation pulpit cannot edit was erased anyway"
    );
    assert!(!document.annotation(&id).unwrap().editable());
}

/// Corrupt annotation data, of each kind a file can carry (§13.5).
///
/// PDFium is the parser and this is what it is being asked to survive, so
/// these need it; they skip with a message when it is absent.
#[cfg(feature = "pdfium")]
mod corrupt {
    use super::*;
    use pulpit_render::document::pdfium::PdfiumDocument;
    use pulpit_render::pdf::pdfium::PdfiumBackend;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn binding() -> Option<MutexGuard<'static, PdfiumBackend>> {
        static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
        let backend = BACKEND
            .get_or_init(|| {
                if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                    std::env::set_var(
                        "PULPIT_PDFIUM_PATH",
                        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib"),
                    );
                }
                match PdfiumBackend::bind() {
                    Ok(backend) => Some(Mutex::new(backend)),
                    Err(error) => {
                        eprintln!("skipping the corrupt-annotation tests: {error}");
                        None
                    }
                }
            })
            .as_ref()?;
        Some(
            backend
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// A one-page document whose single annotation is `annotation`.
    fn with_annotation(path: &Path, annotation: &str) -> std::io::Result<()> {
        let content = b"BT /F1 24 Tf 72 700 Td (page) Tj ET\n";
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
               /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R /Annots [6 0 R] >>"
                .to_vec(),
            {
                let mut object = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
                object.extend_from_slice(content);
                object.extend_from_slice(b"endstream");
                object
            },
            annotation.as_bytes().to_vec(),
        ];

        let mut bytes = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            bytes.extend_from_slice(object);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let start = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        std::fs::write(path, bytes)
    }

    /// Each way one annotation dictionary can be wrong, one case at a time.
    fn cases() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "inklist-is-not-an-array",
                "<< /Type /Annot /Subtype /Ink /Rect [10 10 100 100] /InkList 42 >>",
            ),
            (
                "inklist-holds-an-odd-number-of-numbers",
                "<< /Type /Annot /Subtype /Ink /Rect [10 10 100 100] /InkList [[10 20 30]] >>",
            ),
            (
                "inklist-holds-things-that-are-not-numbers",
                "<< /Type /Annot /Subtype /Ink /Rect [10 10 100 100] \
                 /InkList [[(a) (b) (c) (d)]] >>",
            ),
            (
                "quadpoints-are-not-a-multiple-of-eight",
                "<< /Type /Annot /Subtype /Highlight /Rect [10 10 100 100] \
                 /QuadPoints [10 20 30] >>",
            ),
            (
                "rect-is-inverted",
                "<< /Type /Annot /Subtype /Ink /Rect [100 100 10 10] /InkList [[10 20 30 40]] >>",
            ),
            (
                "rect-is-not-numbers",
                "<< /Type /Annot /Subtype /Ink /Rect [(a) (b) (c) (d)] >>",
            ),
            (
                "nm-is-not-a-string",
                "<< /Type /Annot /Subtype /Ink /Rect [10 10 100 100] /NM 7 >>",
            ),
            (
                "contents-is-a-name-rather-than-a-string",
                "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents /NotAString >>",
            ),
            (
                "appearance-points-at-nothing",
                "<< /Type /Annot /Subtype /Ink /Rect [10 10 100 100] /AP << /N 99 0 R >> >>",
            ),
            (
                "subtype-is-missing",
                "<< /Type /Annot /Rect [10 10 100 100] >>",
            ),
            ("the-annotation-is-not-a-dictionary", "[1 2 3]"),
        ]
    }

    #[test]
    fn corrupt_annotation_data_is_a_diagnostic_rather_than_an_abort() {
        // A8: malformed annotation data produces a diagnostic, not a process
        // abort. Reaching the end of this test at all is the assertion.
        let Some(mut guard) = binding() else { return };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let mut opened = 0;

        let all = cases();
        for (name, annotation) in &all {
            let path = directory.path().join(format!("{name}.pdf"));
            if with_annotation(&path, annotation).is_err() {
                continue;
            }
            let Ok(engine) = PdfiumDocument::open(&mut guard, &path) else {
                // Refusing to open a file this broken is a clean error, and
                // is one of the two acceptable outcomes.
                continue;
            };
            opened += 1;
            let mut document = PdfDocument::new(Box::new(engine), 11);

            // Everything that reads the annotation, on data that is wrong.
            let _ = document.page_geometry(PageIndex(0));
            let summaries = document.annotations(PageIndex(0)).unwrap_or_default();
            for summary in &summaries {
                let _ = document.annotation(&summary.id);
                let _ = summary.to_hit();
                let _ = summary.to_draft();
            }

            // …and writing beside it, which is where a bad neighbour would
            // take an ordinary edit down with it.
            let committed = document.apply(DocumentRevision::INITIAL, stroke(120.0));
            if committed.is_ok() {
                let destination = directory.path().join(format!("{name}-saved.pdf"));
                let _ = document.save_as(&destination, SaveOptions::verified());
            }
        }

        // A test that skipped every case would pass without asserting
        // anything, so it says how much it actually did. PDFium is tolerant:
        // most of these open, and the reading is where the risk is.
        assert!(
            opened * 2 >= all.len(),
            "only {opened} of {} corrupt documents opened at all — this test is \
             no longer exercising the reading path it was written for",
            all.len()
        );
    }

    #[test]
    fn a_file_that_is_not_a_pdf_is_a_clean_error() {
        let Some(mut guard) = binding() else { return };
        let directory = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("empty", &b""[..]),
            ("text", &b"hello, this is not a PDF"[..]),
            ("header-only", &b"%PDF-1.7\n"[..]),
            ("truncated", &b"%PDF-1.7\n1 0 obj\n<< /Type /Catal"[..]),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(
                PdfiumDocument::open(&mut guard, &path).is_err(),
                "{name} opened as a document"
            );
        }
    }
}
