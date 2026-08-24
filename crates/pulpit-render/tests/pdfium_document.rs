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

use pulpit_core::annotate::{
    AnnotationCommand, AnnotationDraft, AnnotationId, FreeTextDraft, HighlightDraft, InkDraft,
    InkPoint, MarkStyle, NoteDraft, TextSource,
};
use pulpit_core::page::{PageIndex, PagePoint, PageQuad, PageRect, PageRotation};
use pulpit_core::search::{HitSource, Query};
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::{
    AppliedEffect, DocumentCommand, DocumentRevision, DocumentTransaction, PdfDocument,
    SaveOptions, TextSelection,
};
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_render::pdf::synth::write_pdf;

mod common;
mod signing_fixture;

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
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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

    // The keys a note needs in order to be a note in somebody else's reader
    // (§7.4). `/Contents` alone is what pulpit used to write, and a note that
    // carries only its text is one other viewers make very little of: no
    // icon, no author to group it under, no date to sort it by, and — since
    // PDF 2.0 requires an appearance — nothing a strict viewer will draw.
    let raw = std::fs::read(&destination).unwrap();
    let raw = String::from_utf8_lossy(&raw);
    for key in ["/Name", "/T", "/CreationDate", "/M", "/Subj", "/F 4"] {
        assert!(raw.contains(key), "the saved note carries {key}");
    }
    assert!(
        raw.contains("/AP"),
        "the marks carry their own appearance, so every viewer draws the same mark"
    );
}

/// A mark's text may be a paragraph, and a paragraph has line breaks in it.
#[test]
fn a_note_and_a_text_mark_keep_the_lines_they_were_written_on() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
    let backend = &mut *guard;
    let directory = temp_dir("lines");
    let path = source(&directory);
    let destination = directory.join("lines.pdf");
    let written = "first line\nsecond line";

    {
        let mut document = open(backend, &path);
        document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::from_annotations([
                    AnnotationCommand::Create(AnnotationDraft::Note(NoteDraft {
                        page: PageIndex(0),
                        at: PagePoint::new(120.0, 120.0),
                        text: written.into(),
                        style: MarkStyle::note(),
                    })),
                    AnnotationCommand::Create(AnnotationDraft::FreeText(FreeTextDraft {
                        page: PageIndex(0),
                        rect: PageRect::new(200.0, 120.0, 420.0, 200.0),
                        // Brackets and a backslash, which are what a literal
                        // string in an appearance gives meaning to.
                        text: "a (bracketed) line\nand a \\ second".into(),
                        source: TextSource::Plain,
                        style: MarkStyle::default(),
                    })),
                ]),
            )
            .expect("both marks commit");
        document
            .save_as(&destination, SaveOptions::verified())
            .unwrap();
    }

    let reopened = PdfiumDocument::open(backend, &destination).unwrap();
    let document = PdfDocument::new(Box::new(reopened), 8);
    let annotations = document.annotations(PageIndex(0)).unwrap();
    let note = annotations
        .iter()
        .find(|summary| summary.kind == pulpit_core::annotate::AnnotationKind::Note)
        .expect("the note is in the file");
    assert_eq!(
        note.contents.text, written,
        "the line break survived the round trip"
    );
    let text = annotations
        .iter()
        .find(|summary| summary.kind == pulpit_core::annotate::AnnotationKind::FreeText)
        .expect("the text mark is in the file");
    assert_eq!(text.contents.text, "a (bracketed) line\nand a \\ second");
}

/// A note written by another reader has no `/NM`, and editing it has to find
/// it again on the next event-loop turn — which an identity derived from a
/// PDFium handle could not (rule 2).
#[test]
fn a_note_from_another_reader_can_be_rewritten_in_place() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
    let backend = &mut *guard;
    let directory = temp_dir("foreign");
    let path = source(&directory);
    let foreign = directory.join("foreign.pdf");
    let rewritten = directory.join("rewritten.pdf");

    // Stand in for the other reader: a note pulpit made, stripped of the one
    // key pulpit uses to recognise its own work.
    {
        let mut document = open(backend, &path);
        document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::from_annotations([AnnotationCommand::Create(
                    AnnotationDraft::Note(NoteDraft {
                        page: PageIndex(0),
                        at: PagePoint::new(200.0, 120.0),
                        text: "somebody else wrote this".into(),
                        style: MarkStyle::note(),
                    }),
                )]),
            )
            .unwrap();
        document.save_as(&foreign, SaveOptions::verified()).unwrap();
    }
    let stripped = String::from_utf8_lossy(&std::fs::read(&foreign).unwrap())
        .replace("/NM", "/XX")
        .into_bytes();
    std::fs::write(&foreign, stripped).unwrap();

    let mut document = open(backend, &foreign);
    let annotations = document.annotations(PageIndex(0)).unwrap();
    let note = annotations
        .iter()
        .find(|summary| summary.kind == pulpit_core::annotate::AnnotationKind::Note)
        .expect("the other reader's note is there");
    assert_eq!(note.contents.text, "somebody else wrote this");
    assert!(
        note.editable(),
        "a note is editable whoever wrote it — the hand opens it by name, not by author"
    );

    // The identity survives the turn it was read in, which is the whole
    // point: the edit is applied through a fresh call, and a session identity
    // built from a PDFium handle would already be meaningless by now.
    let id = note.id.clone();
    document
        .apply(
            document.revision(),
            DocumentTransaction::from_annotations([AnnotationCommand::Replace {
                id: id.clone(),
                replacement: AnnotationDraft::Note(NoteDraft {
                    page: PageIndex(0),
                    at: PagePoint::new(200.0, 120.0),
                    text: "and pulpit rewrote it".into(),
                    style: MarkStyle::note(),
                }),
            }]),
        )
        .expect("another reader's note is editable in place");
    document
        .save_as(&rewritten, SaveOptions::verified())
        .unwrap();
    drop(document);

    let reopened = PdfiumDocument::open(backend, &rewritten).unwrap();
    let document = PdfDocument::new(Box::new(reopened), 8);
    let annotations = document.annotations(PageIndex(0)).unwrap();
    let note = annotations
        .iter()
        .find(|summary| summary.kind == pulpit_core::annotate::AnnotationKind::Note)
        .expect("the note is still there");
    assert_eq!(note.contents.text, "and pulpit rewrote it");
    // The edit gave it a durable name, so the next session finds it by
    // identity rather than by where it happens to sit.
    assert_eq!(note.id, id, "the rewrite kept the mark's identity (A3)");
}

#[test]
fn a_stale_revision_cannot_overwrite_a_later_change() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
    let backend = &mut *guard;
    let directory = temp_dir("frame");
    let path = source(&directory);
    let mut document = open(backend, &path);

    let (width, height) = (306u32, 396u32);
    let before = document
        .render_page(
            PageIndex(0),
            pulpit_core::notes::Region::FULL,
            width,
            height,
            None,
        )
        .expect("the page renders");
    assert_eq!(before.len(), width as usize * height as usize * 4);

    document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .expect("the stroke commits");

    let after = document
        .render_page(
            PageIndex(0),
            pulpit_core::notes::Region::FULL,
            width,
            height,
            None,
        )
        .expect("the page renders again");
    assert_ne!(
        before, after,
        "the frame drawn after the commit does not contain it — either the \
         annotation was not written or the renderer is not drawing /Annots"
    );

    // …and it is the *annotation* that changed the picture, not the render
    // being non-deterministic.
    let again = document
        .render_page(
            PageIndex(0),
            pulpit_core::notes::Region::FULL,
            width,
            height,
            None,
        )
        .unwrap();
    assert_eq!(after, again, "two renders of one revision differ");
}

/// §9.4: a partial repaint is a *crop* of the full render, not a second way of
/// drawing the page. So the pixels it returns have to be the pixels the full
/// render puts in that rectangle — which is the whole basis on which a patch
/// may be pasted into a frame.
#[test]
fn a_partial_render_is_exactly_the_crop_of_the_full_one() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
    let backend = &mut *guard;
    let directory = temp_dir("crop");
    let path = source(&directory);
    let mut document = open(backend, &path);

    // A mark to make the page's content non-uniform, so an accidentally blank
    // or misaligned crop cannot pass.
    document
        .apply(DocumentRevision::INITIAL, DocumentTransaction::one(ink(0)))
        .expect("the stroke commits");

    let (width, height) = (400u32, 520u32);
    let full = document
        .render_page(
            PageIndex(0),
            pulpit_core::notes::Region::FULL,
            width,
            height,
            None,
        )
        .expect("the page renders");

    // A quarter of the page, on exact pixel boundaries so there is a single
    // right answer to compare against.
    let region = pulpit_core::notes::Region::new(0.25, 0.5, 0.5, 0.25);
    let (crop_width, crop_height) = (width / 2, height / 4);
    let cropped = document
        .render_page(PageIndex(0), region, crop_width, crop_height, None)
        .expect("the crop renders");
    assert_eq!(
        cropped.len(),
        crop_width as usize * crop_height as usize * 4
    );

    let x0 = (width / 4) as usize;
    let y0 = (height / 2) as usize;
    for row in 0..crop_height as usize {
        let from = row * crop_width as usize * 4;
        let to = ((y0 + row) * width as usize + x0) * 4;
        assert_eq!(
            &cropped[from..from + crop_width as usize * 4],
            &full[to..to + crop_width as usize * 4],
            "row {row} of the crop is not the same row of the full render — \
             the partial repaint is drawing the page at a different offset \
             or a different scale"
        );
    }
}

#[test]
fn a_pages_geometry_is_read_from_its_crop_box_and_rotation() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
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

/// Search against a real text layer: the hits land on the right pages, carry
/// geometry over the text they matched, and a query for something that is not
/// there is an empty answer rather than a failure.
#[test]
fn finding_text_reports_hits_with_the_geometry_to_draw_them() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
    let backend = &mut *guard;
    let directory = temp_dir("search");
    let path = source(&directory);
    let document = open(backend, &path);

    // The fixture writes the page's own number as its only run of text, so
    // "2" is on page two and nowhere else.
    let query = Query::new("2", false, false);
    let chunk = document
        .find_text(&query, 0..3)
        .expect("the document has a text layer");
    assert_eq!(chunk.from_page, 0);
    assert_eq!(chunk.to_page, 3);
    assert_eq!(
        chunk.hits.len(),
        1,
        "expected one hit, got {:?}",
        chunk.hits
    );

    let hit = &chunk.hits[0];
    assert_eq!(hit.page, PageIndex(1));
    assert_eq!(hit.source, HitSource::PageText);
    assert!(!hit.quads.is_empty(), "a hit with no quads marks nothing");
    assert!(
        hit.quads.iter().all(|quad| !quad.is_degenerate()),
        "a quad with no area marks nothing"
    );
    // The text is drawn at x = 300 in user space on a 720-point page.
    let bounds = hit.quads[0].bounds();
    assert!(
        bounds.left > 250.0 && bounds.left < 400.0,
        "the quad is not over the text: {bounds:?}"
    );
    let marked: String = hit
        .context
        .chars()
        .skip(hit.highlight.offset)
        .take(hit.highlight.len)
        .collect();
    assert_eq!(marked, "2");

    // Regex takes the worker's bounded full-page path, but preserves the same
    // geometry contract as literal PDFium search.
    let expression = Query::regex("[23]", false, false);
    let regex = document
        .find_text(&expression, 0..3)
        .expect("a valid regular expression searches the text layer");
    assert_eq!(
        regex.hits.iter().map(|hit| hit.page).collect::<Vec<_>>(),
        vec![PageIndex(1), PageIndex(2)]
    );
    assert!(regex.hits.iter().all(|hit| !hit.quads.is_empty()));

    // Nothing to find is an empty chunk, not an error and not "unsupported".
    let nothing = document
        .find_text(&Query::new("zzzz", false, false), 0..3)
        .expect("a query with no matches is not a failure");
    assert!(nothing.hits.is_empty());
    assert!(!nothing.truncated);

    // A range that walks off the end of the document stops there.
    let clamped = document
        .find_text(&query, 2..99)
        .expect("clamped, not refused");
    assert_eq!(clamped.to_page, 3);
    assert!(clamped.hits.is_empty());
}

/// The user-visible bug this pins: a file signed with a *visible* signature
/// opens in pulpit's own reader with the appearance missing, while every other
/// viewer draws it.
///
/// A signature's visible appearance is a `/Widget` annotation's `/AP` `/N`, and
/// `FPDF_RenderPageBitmap` draws no widget at all — not even with `FPDF_ANNOT`
/// set, which document mode does set. The only pass that puts a widget on the
/// page is `FPDF_FFLDraw`, over the form-fill environment. So this asserts the
/// outcome that the whole two-pass arrangement exists for, on the two paths a
/// reader's pixels can come from: the document engine, and the render pool the
/// reader submits its page jobs to.
///
/// Measured with each half suppressed in turn: with the `FPDF_FFLDraw` pass
/// removed the count below goes to zero even though `FPDF_ANNOT` is set, and
/// with `FPDF_ANNOT` cleared it stays at its full value. Losing the form-fill
/// environment — a library whose `FPDFDOC_InitFormFillEnvironment` refuses the
/// interface version, say — therefore takes a signed document's signature off
/// the page while every other viewer still draws it.
#[test]
fn a_signed_visible_signature_is_in_the_picture() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
    let Some(credential) = signing_fixture::load_test_credential() else {
        signing_fixture::skip_message();
        return;
    };
    let directory = temp_dir("signature-appearance");
    let unsigned = directory.join("unsigned.pdf");
    let signed = directory.join("signed.pdf");

    // The reported shape: one page, one `/Sig` field whose widget box is in
    // the bottom-right corner of the sheet.
    let rect = [376.0f32, 36.0, 576.0, 106.0];
    std::fs::write(
        &unsigned,
        signing_fixture::build_unsigned_pdf_multipage(
            1,
            &[signing_fixture::FixtureField {
                name: "Recipient",
                page: 0,
                rect: [
                    rect[0] as f64,
                    rect[1] as f64,
                    rect[2] as f64,
                    rect[3] as f64,
                ],
            }],
        ),
    )
    .expect("write the unsigned document");

    let mut request = pulpit_render::sign::SignRequest {
        signing_time: signing_fixture::SIGNING_TIME_UNIX,
        field: pulpit_render::sign::SignTarget::ExistingField("Recipient".to_string()),
        id2: [3u8; 16],
        ..Default::default()
    };
    request.appearance = Some(pulpit_render::sign::SignAppearance {
        page_rotation: pulpit_render::sign::AppearanceRotation::None,
        placement: pulpit_render::sign::AppearancePlacement::FieldRect,
        content: pulpit_render::sign::AppearanceContent::Ink {
            strokes: vec![
                vec![(0.05, 0.1), (0.5, 0.9), (0.95, 0.1)],
                vec![(0.05, 0.5), (0.95, 0.5)],
            ],
            stroke_width: 4.0,
        },
    });
    pulpit_render::sign::sign_document_file(&unsigned, &signed, &credential, &request)
        .expect("signing with a visible appearance succeeds");

    // One pixel per point, so the widget's `/Rect` is its pixel rectangle with
    // y measured from the top.
    let (width, height) = (612u32, 792u32);
    let frame = |backend: &mut PdfiumBackend, path: &Path| -> Vec<u8> {
        let document = open(backend, path);
        document
            .render_page(
                PageIndex(0),
                pulpit_core::notes::Region::FULL,
                width,
                height,
                None,
            )
            .expect("the page renders")
    };
    let before = frame(&mut guard, &unsigned);
    let after = frame(&mut guard, &signed);

    let dark = |pixels: &[u8]| -> usize {
        let mut count = 0;
        for y in (height - rect[3] as u32)..(height - rect[1] as u32) {
            for x in (rect[0] as u32)..(rect[2] as u32) {
                let at = ((y as usize) * width as usize + x as usize) * 4;
                if pixels[at] < 200 && pixels[at + 1] < 200 && pixels[at + 2] < 200 {
                    count += 1;
                }
            }
        }
        count
    };
    let before_dark = dark(&before);
    let after_dark = dark(&after);
    assert!(
        after_dark > before_dark + 200,
        "the signed page has {after_dark} dark pixels inside the signature's box \
         against {before_dark} before signing — pulpit is not drawing the \
         appearance every other viewer shows"
    );

    // The reader's own path: its page jobs are rendered by the pool, from the
    // presentation's document, `with_annotations`.
    let pool_dark = {
        use pulpit_render::pdf::PdfBackend;
        let backend = &mut *guard;
        let opened = backend
            .open(&signed)
            .expect("the pool opens the signed file");
        let request = pulpit_render::pdf::RenderRequest {
            document: opened,
            page: 0,
            region: pulpit_core::notes::Region::FULL,
            width,
            height,
            full_size: None,
            with_annotations: true,
        };
        let frame = backend
            .render(&request, &pulpit_render::pdf::NeverCancel)
            .expect("the pool renders the signed page");
        let counted = dark(&frame.pixels);
        backend.close(opened);
        counted
    };
    assert_eq!(
        pool_dark, after_dark,
        "the render pool and the document engine disagree about the signature's \
         box; the reader's frames come from the pool"
    );
}

/// A text mark's box is measured from its words, and the appearance is clipped
/// to that box — so the measurement has to be right or the mark is cut off.
///
/// `pulpit_core::annotate::text_box` decides the size from Helvetica's own
/// advance widths, which is arithmetic and unit-tested there. What that cannot
/// show is whether the glyphs PDFium actually paints fit inside it. This
/// renders the same words twice, once in the box the measurement asks for and
/// once in one three times its size, and counts the ink: the text is drawn from
/// the box's top-left either way, so any difference is the fitted box clipping
/// what the roomy one had room for.
#[test]
fn a_text_mark_fits_the_box_measured_for_it() {
    let Some(mut guard) = common::pdfium("the PDFium document tests") else {
        return;
    };
    let directory = temp_dir("text-box-fit");
    // Ascenders, descenders and the widest letters in the face, so a box that
    // is short in any direction shows up as missing ink.
    const WORDS: &str = "Wg mmmm illustrative jjjj";
    const FONT_SIZE: f32 = 12.0;

    let (fitted, height) = pulpit_core::annotate::text_box::fit(WORDS, FONT_SIZE);
    let mut mark = |name: &str, width: f32, height: f32| -> PathBuf {
        let source = directory.join(format!("source-{name}.pdf"));
        write_pdf(&source, 1, None).unwrap();
        let written = directory.join(format!("{name}.pdf"));
        let backend = &mut *guard;
        let mut document = open(backend, &source);
        document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::from_annotations([AnnotationCommand::Create(
                    AnnotationDraft::FreeText(FreeTextDraft {
                        page: PageIndex(0),
                        rect: PageRect::new(100.0, 100.0, 100.0 + width, 100.0 + height),
                        text: WORDS.into(),
                        source: TextSource::Plain,
                        style: MarkStyle::default(),
                    }),
                )]),
            )
            .expect("the mark commits");
        document
            .save_as(
                &written,
                SaveOptions {
                    incremental: false,
                    verify: false,
                },
            )
            .expect("the file saves");
        written
    };
    let tight = mark("fitted", fitted, height);
    let roomy = mark("roomy", fitted * 3.0, height * 3.0);

    let mut ink = |path: &Path| -> usize {
        use pulpit_render::pdf::PdfBackend;
        let backend = &mut *guard;
        let opened = backend.open(path).expect("the file opens");
        let request = pulpit_render::pdf::RenderRequest {
            document: opened,
            page: 0,
            region: pulpit_core::notes::Region::FULL,
            // Twice the page's own points, so a clipped stem is several pixels
            // rather than one.
            width: 1224,
            height: 1584,
            full_size: None,
            with_annotations: true,
        };
        let frame = backend
            .render(&request, &pulpit_render::pdf::NeverCancel)
            .expect("the page renders");
        let counted = frame.pixels.chunks(4).filter(|p| p[0] < 200).count();
        backend.close(opened);
        counted
    };

    let (tight, roomy) = (ink(&tight), ink(&roomy));
    assert!(tight > 0, "the mark drew nothing at all");
    assert_eq!(
        tight, roomy,
        "the measured box clips the text: {tight} pixels of ink in it against \
         {roomy} in a box three times the size"
    );
}
