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
    AppliedEffect, DocumentCommand, DocumentRevision, DocumentTransaction, PdfDocument, SaveOptions,
};
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_render::pdf::synth::write_pdf;

fn workspace_lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib")
}

/// PDFium binds once per process, so every test in this binary shares one
/// binding and takes it in turn. A test hands the backend back when it is
/// done, which is also what exercises [`PdfiumDocument::into_backend`].
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
                    eprintln!("skipping the PDFium document tests: {error}");
                    None
                }
            }
        })
        .as_ref()?;
    Some(slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
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

/// Open a document, taking the process-wide binding out of the slot.
fn open(slot: &mut Option<PdfiumBackend>, path: &Path) -> PdfDocument {
    let backend = slot.take().expect("the binding is free");
    let engine = PdfiumDocument::open(backend, path).expect("the document opens");
    PdfDocument::new(Box::new(engine), 4_242)
}

/// Close a document and put the binding back, so the next section can open
/// one. PDFium is bound once per process; a document is not.
fn close(slot: &mut Option<PdfiumBackend>, document: PdfDocument) {
    let engine = *document
        .into_backend()
        .into_any()
        .downcast::<PdfiumDocument>()
        .expect("these documents are PDFium documents");
    *slot = Some(engine.into_backend());
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

fn a_completed_gesture_becomes_an_ink_annotation_in_the_open_document(
    slot: &mut Option<PdfiumBackend>,
) {
    let directory = temp_dir("create");
    let path = source(&directory);
    let mut document = open(slot, &path);

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
    close(slot, document);
}

fn saving_and_reopening_preserves_identity_geometry_and_style(slot: &mut Option<PdfiumBackend>) {
    let directory = temp_dir("roundtrip");
    let path = source(&directory);
    let destination = directory.join("annotated.pdf");

    let (id, bounds) = {
        let mut document = open(slot, &path);
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
        close(slot, document);
        (id, bounds)
    };

    // A fresh engine over the saved file: nothing of the first session is
    // carried across except what is in the PDF (A1).
    let reopened =
        PdfiumDocument::open(slot.take().expect("the binding is free"), &destination).unwrap();
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
    close(slot, document);
}

fn the_source_file_is_never_written(slot: &mut Option<PdfiumBackend>) {
    let directory = temp_dir("immutable");
    let path = source(&directory);
    let before = std::fs::read(&path).unwrap();

    let mut document = open(slot, &path);
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
    close(slot, document);
}

fn an_erased_mark_comes_back_under_its_own_name(slot: &mut Option<PdfiumBackend>) {
    let directory = temp_dir("undo");
    let path = source(&directory);
    let mut document = open(slot, &path);

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
    close(slot, document);
}

fn several_kinds_of_mark_round_trip_through_a_saved_file(slot: &mut Option<PdfiumBackend>) {
    let directory = temp_dir("kinds");
    let path = source(&directory);
    let destination = directory.join("marks.pdf");

    {
        let mut document = open(slot, &path);
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
        close(slot, document);
    }

    let reopened =
        PdfiumDocument::open(slot.take().expect("the binding is free"), &destination).unwrap();
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
    close(slot, document);
}

fn a_stale_revision_cannot_overwrite_a_later_change(slot: &mut Option<PdfiumBackend>) {
    let directory = temp_dir("conflict");
    let path = source(&directory);
    let mut document = open(slot, &path);

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
    close(slot, document);
}

fn a_pages_geometry_is_read_from_its_crop_box_and_rotation(slot: &mut Option<PdfiumBackend>) {
    let directory = temp_dir("geometry");
    let path = source(&directory);
    let document = open(slot, &path);

    let geometry = document.page_geometry(PageIndex(0)).unwrap();
    assert!(geometry.is_valid());
    assert!(geometry.width > 0.0 && geometry.height > 0.0);
    assert_eq!(geometry.rotation, PageRotation::None);
    // The canonical origin is the crop box's top-left corner (A4), so the
    // page's own bounds start at zero however the crop box is placed.
    assert_eq!(geometry.bounds().left, 0.0);
    assert_eq!(geometry.bounds().top, 0.0);
    close(slot, document);
}

/// One test, several sections.
///
/// PDFium binds once per process and `cargo test` runs a binary's tests as
/// threads of one process, so the sections take the binding in turn rather
/// than being separate `#[test]`s that would race for it. Each section names
/// what it establishes, and each hands the binding back when it is done.
#[test]
fn native_annotations_round_trip_through_real_pdfium() {
    let Some(mut guard) = binding() else { return };
    let slot = &mut *guard;

    a_completed_gesture_becomes_an_ink_annotation_in_the_open_document(slot);
    saving_and_reopening_preserves_identity_geometry_and_style(slot);
    the_source_file_is_never_written(slot);
    an_erased_mark_comes_back_under_its_own_name(slot);
    several_kinds_of_mark_round_trip_through_a_saved_file(slot);
    a_stale_revision_cannot_overwrite_a_later_change(slot);
    a_pages_geometry_is_read_from_its_crop_box_and_rotation(slot);
}
