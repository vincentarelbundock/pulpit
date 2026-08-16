//! Native annotations in a real PDF, through real PDFium.
//!
//! These are the round-trip tests `SPEC-document.md` §13.3 asks for, and the
//! evidence for acceptance criteria 1, 2, 6, 8 and 11: a completed gesture
//! becomes an `/Ink` annotation *in the document*, saving and reopening
//! preserves its identity and geometry, and the source file is never written.
//!
//! Skipped with a message when no `libpdfium` is installed, so a machine
//! without one still gets a green, honest run — a green run there has skipped
//! the meaningful tests, which is what the message says.

#![cfg(feature = "pdfium")]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use pulpit_core::annotate::{
    AnnotationCommand, AnnotationDraft, AnnotationId, HighlightDraft, InkDraft, InkPoint,
    MarkStyle, NoteDraft,
};
use pulpit_core::page::{PageIndex, PagePoint, PageQuad, PageRect, PageRotation};
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::{
    AppliedEffect, DocumentCommand, DocumentRevision, DocumentTransaction, PdfDocument,
    SaveOptions, TextSelection,
};
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_render::pdf::synth::write_pdf;

fn workspace_lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib")
}

/// PDFium binds once per process, so every section of this binary shares one
/// binding.
///
/// A document borrows it for as long as the document is open, which is
/// exactly the lifetime the type asks for: the next section cannot open
/// anything until the previous document has been dropped, and the compiler
/// says so rather than a comment.
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
                    eprintln!("skipping the PDFium document tests: {error}");
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

fn temp_dir(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("pulpit-document-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// A three-page synthetic deck to annotate.
fn source(directory: &Path) -> PathBuf {
    let path = directory.join("source.pdf");
    write_pdf(&path, 3, None).unwrap();
    path
}

/// Open a document, borrowing the process-wide binding for its lifetime.
fn open<'a>(backend: &'a mut PdfiumBackend, path: &Path) -> PdfDocument<'a> {
    let engine = PdfiumDocument::open(backend, path).expect("the document opens");
    PdfDocument::new(Box::new(engine), 4_242)
}

fn ink(page: usize) -> DocumentCommand {
    DocumentCommand::Annotation(AnnotationCommand::Create(AnnotationDraft::Ink(InkDraft {
        page: PageIndex(page),
        points: vec![
            InkPoint::new(72.0, 72.0),
            InkPoint::new(160.0, 130.0),
            InkPoint::new(260.0, 90.0),
        ],
        style: MarkStyle::default(),
    })))
}

fn created_id(applied: &pulpit_render::document::Applied) -> AnnotationId {
    match &applied.effects[0] {
        AppliedEffect::Annotation(summary) => summary.id.clone(),
        other => panic!("expected a created annotation, got {other:?}"),
    }
}

#[test]
fn a_completed_gesture_becomes_an_ink_annotation_in_the_open_document() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("create");
    let path = source(&directory);
    let mut document = open(backend, &path);

    assert_eq!(document.revision(), DocumentRevision::INITIAL);
    assert!(!document.is_dirty());
    assert!(document.annotations(PageIndex(0)).unwrap().is_empty());

    let applied = document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .expect("the stroke commits");
    assert_eq!(applied.document_revision, DocumentRevision(1));
    assert!(document.is_dirty());

    let annotations = document.annotations(PageIndex(0)).unwrap();
    assert_eq!(annotations.len(), 1, "one gesture, one annotation");
    let summary = &annotations[0];
    assert_eq!(summary.kind, pulpit_core::annotate::AnnotationKind::Ink);
    assert_eq!(summary.id, created_id(&applied));
    assert!(
        summary.id.looks_generated(),
        "a created annotation carries pulpit's /NM"
    );
    assert!(
        summary.path.len() >= 3,
        "the /InkList came back: {:?}",
        summary.path
    );
    // Canonical geometry survives the trip through PDF user space and back.
    assert!(
        (summary.path[0].x - 72.0).abs() < 0.5,
        "{:?}",
        summary.path[0]
    );
    assert!(
        (summary.path[0].y - 72.0).abs() < 0.5,
        "{:?}",
        summary.path[0]
    );

    // The other pages are untouched.
    assert!(document.annotations(PageIndex(1)).unwrap().is_empty());
}

#[test]
fn saving_and_reopening_preserves_identity_geometry_and_style() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("roundtrip");
    let path = source(&directory);
    let destination = directory.join("annotated.pdf");

    let (id, bounds) = {
        let mut document = open(backend, &path);
        let applied = document
            .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(1)))
            .unwrap();
        let id = created_id(&applied);
        let bounds = document.annotation(&id).unwrap().bounds;
        let saved = document
            .save_as(&destination, SaveOptions::verified())
            .expect("Save As writes the file");
        assert_eq!(saved.revision, DocumentRevision(1));
        assert!(saved.bytes > 0);
        (id, bounds)
    };

    // A fresh engine over the saved file: nothing of the first session is
    // carried across except what is in the PDF (A1).
    let reopened = PdfiumDocument::open(backend, &destination).unwrap();
    let document = PdfDocument::new(Box::new(reopened), 7);
    let annotations = document.annotations(PageIndex(1)).unwrap();
    assert_eq!(annotations.len(), 1, "the mark is in the saved file");
    let summary = &annotations[0];
    assert_eq!(summary.id, id, "the /NM survived the save");
    assert!(
        (summary.bounds.left - bounds.left).abs() < 1.0
            && (summary.bounds.top - bounds.top).abs() < 1.0,
        "geometry moved: {:?} became {:?}",
        bounds,
        summary.bounds
    );
    assert!(summary.editable());
}

#[test]
fn the_source_file_is_never_written() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("immutable");
    let path = source(&directory);
    let before = std::fs::read(&path).unwrap();

    let mut document = open(backend, &path);
    document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .unwrap();
    // A6: the source is refused as a destination, by name.
    assert!(document.save_as(&path, SaveOptions::verified()).is_err());
    document
        .save_as(&directory.join("copy.pdf"), SaveOptions::verified())
        .unwrap();

    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the source changed under an edit and a save"
    );
}

#[test]
fn an_erased_mark_comes_back_under_its_own_name() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("undo");
    let path = source(&directory);
    let mut document = open(backend, &path);

    let applied = document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .unwrap();
    let id = created_id(&applied);

    let erased = document
        .apply(
            document.revision(),
            DocumentTransaction::from_annotations([AnnotationCommand::Delete { id: id.clone() }]),
        )
        .unwrap();
    assert!(document.annotations(PageIndex(0)).unwrap().is_empty());

    // A3: the undo restores rather than recreates, so the identity holds.
    let undone = document.undo(document.revision(), erased.undo).unwrap();
    let annotations = document.annotations(PageIndex(0)).unwrap();
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].id, id, "undo renamed the annotation");

    // …and redo is the undo request carrying what the undo handed back.
    document.undo(document.revision(), undone.undo).unwrap();
    assert!(document.annotations(PageIndex(0)).unwrap().is_empty());
}

#[test]
fn several_kinds_of_mark_round_trip_through_a_saved_file() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("kinds");
    let path = source(&directory);
    let destination = directory.join("marks.pdf");

    {
        let mut document = open(backend, &path);
        let transaction = DocumentTransaction::from_annotations([
            AnnotationCommand::Create(AnnotationDraft::Highlight(HighlightDraft {
                page: PageIndex(0),
                quads: vec![PageQuad::from_rect(PageRect::new(
                    72.0, 200.0, 300.0, 216.0,
                ))],
                text: "the marked words".into(),
                style: MarkStyle::highlighter(),
            })),
            AnnotationCommand::Create(AnnotationDraft::Note(NoteDraft {
                page: PageIndex(0),
                at: PagePoint::new(400.0, 100.0),
                text: "a note to self".into(),
                open: false,
                style: MarkStyle::default(),
            })),
        ]);
        document
            .apply(DocumentRevision::INITIAL, transaction)
            .expect("both marks commit as one action");
        document
            .save_as(&destination, SaveOptions::verified())
            .unwrap();
    }

    let reopened = PdfiumDocument::open(backend, &destination).unwrap();
    let document = PdfDocument::new(Box::new(reopened), 8);
    let annotations = document.annotations(PageIndex(0)).unwrap();
    assert_eq!(annotations.len(), 2);

    let highlight = annotations
        .iter()
        .find(|summary| summary.kind == pulpit_core::annotate::AnnotationKind::Highlight)
        .expect("the highlight is in the file");
    assert_eq!(
        highlight.quads.len(),
        1,
        "/QuadPoints came back as normative geometry"
    );
    assert_eq!(
        highlight.contents.text, "the marked words",
        "the selected text is recoverable from /Contents"
    );

    let note = annotations
        .iter()
        .find(|summary| summary.kind == pulpit_core::annotate::AnnotationKind::Note)
        .expect("the note is in the file");
    assert_eq!(note.contents.text, "a note to self");
}

#[test]
fn a_stale_revision_cannot_overwrite_a_later_change() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("conflict");
    let path = source(&directory);
    let mut document = open(backend, &path);

    document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .unwrap();
    // A delayed message from before that commit.
    let error = document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .unwrap_err();
    assert!(matches!(
        error,
        pulpit_render::document::DocumentError::RevisionConflict { .. }
    ));
    assert_eq!(document.annotations(PageIndex(0)).unwrap().len(), 1);
}

/// §7.2 and §8.2, against real text: the highlighter is a *text* tool, and its
/// `/QuadPoints` have to describe the text that was actually selected.
#[test]
fn selecting_real_text_resolves_to_quads_that_become_a_highlight() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("selection");
    let path = source(&directory);
    let mut document = open(backend, &path);

    // The synthetic page carries one run of 96-point text with its baseline at
    // (300, 180) in user space, on a 720 × 405 page. In canonical space that
    // is near x = 300, y = 405 − 180.
    let geometry = document.page_geometry(PageIndex(0)).unwrap();
    let at = geometry.from_user_space(320.0, 200.0);

    let word = document
        .select_text(PageIndex(0), TextSelection::Word { at })
        .expect("the page has a text layer");
    assert!(
        !word.is_empty(),
        "no text was found where the fixture writes some"
    );
    assert!(
        !word.text.is_empty(),
        "the selection came back with no text"
    );
    assert!(
        word.quads.iter().all(|quad| !quad.is_degenerate()),
        "a quad with no area marks nothing"
    );

    // The quads mark the text: their bounds sit around where it was drawn.
    let bounds = word.quads[0].bounds();
    assert!(
        bounds.left > 250.0 && bounds.left < 400.0,
        "the quad is not over the text: {bounds:?}"
    );

    // …and those quads are what a highlight is written from.
    let applied = document
        .apply(
            DocumentRevision::INITIAL,
            DocumentTransaction::from_annotations([AnnotationCommand::Create(
                AnnotationDraft::Highlight(HighlightDraft {
                    page: PageIndex(0),
                    quads: word.quads.clone(),
                    text: word.text.clone(),
                    style: MarkStyle::highlighter(),
                }),
            )]),
        )
        .expect("the highlight commits");
    assert_eq!(applied.document_revision, DocumentRevision(1));

    let annotations = document.annotations(PageIndex(0)).unwrap();
    assert_eq!(annotations.len(), 1);
    assert_eq!(
        annotations[0].quads.len(),
        word.quads.len(),
        "the /QuadPoints that came back are not the ones that went in"
    );
    assert_eq!(
        annotations[0].contents.text, word.text,
        "the selected text is recoverable from /Contents"
    );

    // A point with no text under it is an empty answer, not an error (§6.3).
    let empty = document
        .select_text(
            PageIndex(0),
            TextSelection::Word {
                at: PagePoint::new(10.0, 10.0),
            },
        )
        .expect("an empty selection is not a failure");
    assert!(empty.is_empty());
}

/// A7, and the reason document mode renders through the engine that holds the
/// document rather than through the render worker pool: the frame drawn after
/// a commit contains the commit.
#[test]
fn a_frame_rendered_after_a_commit_contains_the_mark() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("frame");
    let path = source(&directory);
    let mut document = open(backend, &path);

    let (width, height) = (306u32, 396u32);
    let before = document
        .render_page(PageIndex(0), width, height)
        .expect("the page renders");
    assert_eq!(before.len(), width as usize * height as usize * 4);

    document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .expect("the stroke commits");

    let after = document
        .render_page(PageIndex(0), width, height)
        .expect("the page renders again");
    assert_ne!(
        before, after,
        "the frame drawn after the commit does not contain it — either the \
         annotation was not written or the renderer is not drawing /Annots"
    );

    // …and it is the *annotation* that changed the picture, not the render
    // being non-deterministic.
    let again = document.render_page(PageIndex(0), width, height).unwrap();
    assert_eq!(after, again, "two renders of one revision differ");
}

#[test]
fn a_pages_geometry_is_read_from_its_crop_box_and_rotation() {
    let Some(mut guard) = binding() else { return };
    let backend = &mut *guard;
    let directory = temp_dir("geometry");
    let path = source(&directory);
    let document = open(backend, &path);

    let geometry = document.page_geometry(PageIndex(0)).unwrap();
    assert!(geometry.is_valid());
    assert!(geometry.width > 0.0 && geometry.height > 0.0);
    assert_eq!(geometry.rotation, PageRotation::None);
    // The canonical origin is the crop box's top-left corner (A4), so the
    // page's own bounds start at zero however the crop box is placed.
    assert_eq!(geometry.bounds().left, 0.0);
    assert_eq!(geometry.bounds().top, 0.0);
}

// Each test above takes the process-wide binding for as long as its document
// is open and gives it back by dropping it — which is the whole benefit of the
// engine borrowing its binding rather than owning it. `cargo test` runs these
// as threads of one process; they serialise on the mutex and are otherwise
// independent, so a failure names one behaviour rather than stopping a run.
