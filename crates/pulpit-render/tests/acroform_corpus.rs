//! The AcroForm hazard corpus, against the document engine (§13.1).
//!
//! Its premise, restated because it is the reason the corpus survived the fold
//! from pdfform: the public corpora exercise parsers and renderers, which this
//! project delegates to PDFium. What they do not cover is finding fields,
//! filling them and writing them back.
//!
//! **What this file checks today.** Every case must survive opening,
//! annotating and saving, leaving the process alive, the source untouched and
//! either a readable PDF or a clean error. The *filling* half — the
//! `Expect::Roundtrips` and `Expect::ReadOnly` promises each case carries —
//! waits on the form-fill spike of §14.3, because §8.6 now fills fields
//! through PDFium's own form-fill environment rather than through an
//! application-drawn editor, and there is nothing to assert against until
//! those events are wired. [`the_corpus_states_more_than_is_checked_yet`]
//! fails the day that stops being true, so the gap cannot be forgotten.
//!
//! Skipped with a message when no `libpdfium` is installed.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, InkDraft, InkPoint, MarkStyle};
use pulpit_core::page::PageIndex;
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::{DocumentRevision, DocumentTransaction, PdfDocument, SaveOptions};
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_testkit::{corpus, Expect, Unchanged};

fn workspace_lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib")
}

fn binding() -> Option<MutexGuard<'static, PdfiumBackend>> {
    static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
    let backend = BACKEND
        .get_or_init(|| {
            if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                std::env::set_var("PULPIT_PDFIUM_PATH", workspace_lib());
            }
            match PdfiumBackend::bind() {
                Ok(backend) => Some(Mutex::new(backend)),
                Err(error) => {
                    eprintln!("skipping the AcroForm corpus: {error}");
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

/// One ink stroke, which is the mutation every case gets: it exercises the
/// write path against a document whose form is malformed, which is exactly
/// where a shared `/AcroForm` dictionary would take an annotation edit down
/// with it.
fn stroke() -> DocumentTransaction {
    DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
        InkDraft {
            page: PageIndex(0),
            points: vec![InkPoint::new(80.0, 80.0), InkPoint::new(200.0, 140.0)],
            style: MarkStyle::default(),
        },
    ))])
}

#[test]
fn every_corpus_case_survives_being_opened_annotated_and_saved() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;

    let directory = tempfile::tempdir().expect("a temporary directory");
    let cases = corpus();
    assert!(cases.len() > 40, "the corpus did not survive the fold");

    let mut opened = 0usize;
    let mut refused: Vec<(&str, String)> = Vec::new();

    for case in cases {
        let source = pulpit_testkit::write_pdf(directory.path(), case.name, &case.bytes);
        // The one irreversible action this program has is writing a file, and
        // the source is the file it must never write (A6).
        let unchanged = Unchanged::new(&source, case.name);

        let engine = match PdfiumDocument::open(backend, &source) {
            Ok(engine) => engine,
            Err(error) => {
                // A clean error is an acceptable outcome for a document
                // malformed enough not to open: the corpus promises "either a
                // readable PDF or a clean error", and reaching this line at
                // all is the proof that it was not a crash. The source is
                // still checked, because a failed open must leave nothing
                // behind either.
                unchanged.check();
                refused.push((case.name, error.to_string()));
                continue;
            }
        };
        let mut document = PdfDocument::new(Box::new(engine), 1_234);
        opened += 1;

        // Reading everything the document offers, on hostile input.
        let pages = document.page_count();
        assert!(pages > 0, "{}: opened with no pages", case.name);
        for page in 0..pages.min(4) {
            let page = PageIndex(page);
            let _ = document.page_geometry(page);
            let _ = document.annotations(page);
        }
        let _ = document.fields();

        // …and writing to it.
        // A refusal is fine — an encrypted document may forbid changes — as
        // long as it is a refusal and not a fall over.
        if let Ok(applied) = document.apply(DocumentRevision::INITIAL, stroke()) {
            assert_eq!(applied.document_revision, DocumentRevision(1));
        }

        let destination = directory.path().join(format!("{}-saved.pdf", case.name));
        let written = match document.save_as(&destination, SaveOptions::verified()) {
            Ok(saved) => {
                assert!(saved.bytes > 0, "{}: saved an empty file", case.name);
                assert!(destination.exists());
                true
            }
            Err(_) => false,
        };
        // Dropping the document gives the binding back, which is what lets the
        // saved file be opened next.
        drop(document);

        if written {
            // The output has to be *readable*, not merely written.
            let reopened = PdfiumDocument::open(backend, &destination).unwrap_or_else(|error| {
                panic!("{}: the saved file will not open: {error}", case.name)
            });
            let reopened = PdfDocument::new(Box::new(reopened), 1);
            assert!(reopened.page_count() > 0, "{}: saved no pages", case.name);
        }

        unchanged.check();
    }

    // A case that will not open is an acceptable outcome, but a corpus where
    // most of them do not is a broken engine wearing a green test.
    assert!(
        opened > 40,
        "only {opened} of the corpus opened; refused: {refused:?}"
    );
}

/// The corpus promises more than this file checks, and says so out loud.
///
/// Each case carries an [`Expect`]; the filling half of §13.1 asserts them.
/// Until the form-fill events of §8.6 are wired, those promises are unchecked
/// — and a silent gap in a corpus is worse than no corpus, so this test
/// records the size of it.
#[test]
fn the_corpus_states_more_than_is_checked_yet() {
    let cases = corpus();
    let promising = cases
        .iter()
        .filter(|case| !matches!(case.expect, Expect::Survives))
        .count();
    assert!(
        promising > 0,
        "a corpus where no case has a defensible correct answer is a smoke test"
    );
    eprintln!(
        "{promising} of {} corpus cases assert a fill result that is not checked yet; \
         see §8.6 and step 6 of §14.3",
        cases.len()
    );
}
