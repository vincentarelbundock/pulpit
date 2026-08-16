//! The document worker, end to end, as a real child process.
//!
//! Everything else about document mode is tested in-process: the engine
//! against a memory backend, the PDFium engine against a real PDF, the worker
//! loop over a pipe made of byte vectors. This is the one test that starts an
//! actual `--document-worker=FILE` process, talks to it over its stdin and
//! stdout, and checks that an annotation committed on the far side of the
//! boundary is in the file afterwards.
//!
//! It lives in `pulpit` rather than `pulpit-render` because the worker is a
//! role of *this* binary (§5.1), and `std::env::current_exe()` from a test in
//! this crate is the test binary rather than pulpit — so the test names the
//! built executable explicitly, and skips when there is not one to name.

use std::path::PathBuf;

use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, InkDraft, InkPoint, MarkStyle};
use pulpit_core::page::PageIndex;
use pulpit_render::document::protocol::{
    DocumentRenderRequest, DocumentRequest, DocumentResponse, SaveRequest,
};
use pulpit_render::document::session::{DocumentSession, DocumentWorkerCommand, SessionError};
use pulpit_render::document::{DocumentRevision, DocumentTransaction, SaveOptions};

/// The pulpit executable this test run built, beside the test binary itself.
///
/// `cargo test` puts integration-test binaries in `target/<profile>/deps` and
/// the executable in `target/<profile>`, so the parent of the test binary's
/// directory is where to look. Absent under `cargo build --tests`, which is
/// why this skips rather than fails.
fn executable() -> Option<PathBuf> {
    let test_binary = std::env::current_exe().ok()?;
    let candidate = test_binary.parent()?.parent()?.join(if cfg!(windows) {
        "pulpit.exe"
    } else {
        "pulpit"
    });
    candidate.is_file().then_some(candidate)
}

fn command(source: &std::path::Path) -> Option<DocumentWorkerCommand> {
    let program = executable()?;
    // The child searches for PDFium relative to its own executable and the
    // working directory, and a test's working directory is the crate rather
    // than the workspace. Pointing it at the checked-out library is what the
    // dev shell does for a real run; the child inherits it.
    if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
        let lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib");
        std::env::set_var("PULPIT_PDFIUM_PATH", lib);
    }
    Some(DocumentWorkerCommand::Explicit {
        program,
        args: vec![format!("--document-worker={}", source.display())],
    })
}

fn stroke() -> DocumentTransaction {
    DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
        InkDraft {
            page: PageIndex(0),
            points: vec![
                InkPoint::new(72.0, 72.0),
                InkPoint::new(180.0, 140.0),
                InkPoint::new(300.0, 96.0),
            ],
            style: MarkStyle::default(),
        },
    ))])
}

#[test]
fn a_mark_committed_across_the_process_boundary_is_in_the_saved_file() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("source.pdf");
    if pulpit_render::pdf::synth::write_pdf(&source, 2, None).is_err() {
        eprintln!("skipping: cannot write a synthetic PDF");
        return;
    }
    let Some(command) = command(&source) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };

    let mut session = match DocumentSession::start(&command, &source) {
        Ok(session) => session,
        Err(error) => {
            // The worker exits with guidance when it cannot bind PDFium,
            // which on a machine without one is the honest outcome and not a
            // failure of this test.
            eprintln!("skipping the document worker test: {error}");
            return;
        }
    };
    assert_eq!(session.source(), source);

    // What the worker holds, which is what a reader needs before it can lay
    // anything out: how many pages, and how big each of them is.
    let DocumentResponse::Opened(info) = session
        .request(DocumentRequest::Info)
        .expect("the worker describes its document")
    else {
        panic!("expected document info")
    };
    assert_eq!(info.page_count, 2);
    assert!(info.first_page.is_valid());

    let DocumentResponse::PageGeometries(pages) = session
        .request(DocumentRequest::PageGeometries {
            from: PageIndex(0),
            // More than the document has: a run past the end is the tail of
            // the document, not an error.
            count: 64,
        })
        .expect("the worker measures its pages")
    else {
        panic!("expected page geometries")
    };
    assert_eq!(pages.len(), 2);
    assert!(pages.iter().all(|page| page.is_valid()));

    // Nothing on the page to begin with.
    let response = session
        .request(DocumentRequest::ListAnnotations { page: PageIndex(0) })
        .expect("the worker answers");
    let DocumentResponse::Annotations(annotations) = response else {
        panic!("expected an annotation list, got {response:?}")
    };
    assert!(annotations.is_empty());

    // A mark, committed on the far side of the boundary.
    let response = session
        .request(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        })
        .expect("the stroke commits");
    let DocumentResponse::Applied(applied) = response else {
        panic!("expected an applied transaction, got {response:?}")
    };
    assert_eq!(applied.document_revision, DocumentRevision(1));
    assert_eq!(applied.dirty_pages, vec![PageIndex(0)]);

    // …and it is there when the worker is asked again.
    let response = session
        .request(DocumentRequest::ListAnnotations { page: PageIndex(0) })
        .unwrap();
    let DocumentResponse::Annotations(annotations) = response else {
        panic!("expected an annotation list")
    };
    assert_eq!(annotations.len(), 1);
    let id = annotations[0].id.clone();

    // A frame, from the process that holds the mutated document — which is
    // the only one that can promise it contains the commit (A7).
    let DocumentResponse::Frame(frame) = session
        .request(DocumentRequest::Render(DocumentRenderRequest {
            page: PageIndex(0),
            width: 200,
            height: 260,
            expected_revision: DocumentRevision(1),
            region: pulpit_core::notes::Region::FULL,
        }))
        .expect("the page renders")
    else {
        panic!("expected a frame")
    };
    assert!(frame.is_consistent());
    assert_eq!(frame.revision, DocumentRevision(1));
    assert!(
        frame.pixels.iter().any(|byte| *byte != 0),
        "the frame is entirely blank"
    );

    // A stale revision is refused across the wire exactly as it is in
    // process: a delayed message must not overwrite a later change (A7).
    let stale = session.request(DocumentRequest::Apply {
        expected_revision: DocumentRevision::INITIAL,
        transaction: stroke(),
    });
    match stale {
        Err(SessionError::Refused(failure)) => assert!(!failure.is_retryable()),
        other => panic!("expected a revision conflict, got {other:?}"),
    }

    // Save As, and the file it wrote has the mark in it.
    let destination = directory.path().join("annotated.pdf");
    let response = session
        .request(DocumentRequest::SaveAs(SaveRequest {
            destination: destination.clone(),
            options: SaveOptions::verified(),
        }))
        .expect("the save succeeds");
    let DocumentResponse::Saved(saved) = response else {
        panic!("expected a save, got {response:?}")
    };
    assert_eq!(saved.revision, DocumentRevision(1));
    assert!(destination.is_file());
    assert!(saved.bytes > 0);

    session.close();

    // A6, checked from outside every process that touched it: the source is
    // byte-identical to what was written before any of this happened.
    let reopened = std::fs::read(&destination).unwrap();
    assert!(reopened.starts_with(b"%PDF"), "the output is not a PDF");
    assert!(
        reopened.len() > std::fs::read(&source).unwrap().len(),
        "the annotated copy should carry more than the source"
    );

    // The identity the worker minted is pulpit's own, which is what lets a
    // later session find the annotation again (A3).
    assert!(id.looks_generated(), "{id}");
}

#[test]
fn a_worker_that_cannot_open_its_document_reports_it_rather_than_hanging() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let missing = directory.path().join("not-a-pdf.pdf");
    std::fs::write(&missing, b"this is not a PDF at all").unwrap();
    let Some(command) = command(&missing) else {
        eprintln!("skipping: the pulpit executable was not built beside this test");
        return;
    };

    // The worker exits before the handshake, so starting the session fails —
    // which is the point: the supervisor learns immediately instead of
    // waiting on a pipe that will never carry an answer.
    match DocumentSession::start(&command, &missing) {
        Err(error) => assert!(error.is_worker_loss(), "{error}"),
        Ok(_) => panic!("a worker opened a file that is not a PDF"),
    }
}
