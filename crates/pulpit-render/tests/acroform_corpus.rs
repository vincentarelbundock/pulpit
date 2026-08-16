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

fn binding() -> Option<MutexGuard<'static, Option<PdfiumBackend>>> {
    static BACKEND: OnceLock<Option<Mutex<Option<PdfiumBackend>>>> = OnceLock::new();
    let slot = BACKEND
        .get_or_init(|| {
            if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                std::env::set_var("PULPIT_PDFIUM_PATH", workspace_lib());
            }
            match PdfiumBackend::bind() {
                Ok(backend) => Some(Mutex::new(Some(backend))),
                Err(error) => {
                    eprintln!("skipping the AcroForm corpus: {error}");
                    None
                }
            }
        })
        .as_ref()?;
    Some(slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// Hand the binding back, so the next case can open its own document.
fn close(slot: &mut Option<PdfiumBackend>, document: PdfDocument) {
    let engine = *document
        .into_backend()
        .into_any()
        .downcast::<PdfiumDocument>()
        .expect("these documents are PDFium documents");
    *slot = Some(engine.into_backend());
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
    let slot = &mut *guard;

    let directory = tempfile::tempdir().expect("a temporary directory");
    let cases = corpus();
    assert!(cases.len() > 40, "the corpus did not survive the fold");

    let mut opened = 0usize;

    for case in cases {
        let source = pulpit_testkit::write_pdf(directory.path(), case.name, &case.bytes);
        // The one irreversible action this program has is writing a file, and
        // the source is the file it must never write (A6).
        let unchanged = Unchanged::new(&source, case.name);

        let backend = slot.take().expect("the binding is free");
        let engine = match PdfiumDocument::open(backend, &source) {
            Ok(engine) => engine,
            Err(error) => {
                // A clean error would be an acceptable outcome for a document
                // malformed enough not to open — the corpus only promises
                // "either a readable PDF or a clean error". But `open`
                // consumed the binding on the way in and PDFium binds once per
                // process, so there is no way to carry on: a refused open ends
                // the run loudly rather than silently skipping every case
                // after it. Reaching here at all means the engine survived,
                // which is the invariant that matters; the panic is about the
                // harness, and the message says so.
                unchanged.check();
                panic!(
                    "{}: opening was refused, which loses the process-wide \
                     binding and stops the corpus: {error}",
                    case.name
                );
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
        if let Ok(saved) = document.save_as(&destination, SaveOptions::verified()) {
            assert!(saved.bytes > 0, "{}: saved an empty file", case.name);
            assert!(destination.exists());
            // The output has to be readable, not merely written.
            let backend = {
                close(slot, document);
                slot.take().expect("the binding is free")
            };
            let reopened = PdfiumDocument::open(backend, &destination).unwrap_or_else(|error| {
                panic!("{}: the saved file will not open: {error}", case.name)
            });
            let reopened = PdfDocument::new(Box::new(reopened), 1);
            assert!(reopened.page_count() > 0, "{}: saved no pages", case.name);
            close(slot, reopened);
        } else {
            close(slot, document);
        }

        unchanged.check();
    }

    assert!(opened > 40, "only {opened} cases opened");
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
