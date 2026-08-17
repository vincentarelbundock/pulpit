//! A mark made at the lectern, all the way through the file and back.
//!
//! §14.3 step 4 routes completed presenter ink through the document engine,
//! and acceptance criteria 1, 2 and 3 are the promises that makes: the gesture
//! creates a native `/Ink` in the open PDF, saving and reopening preserves it,
//! and the same annotation is the one both modes edit.
//!
//! The presenter's half of this is a coordinate conversion, unit-tested in
//! `pulpit_core::annotate::presenter`. What that cannot show is the round trip
//! through a real PDF, which is what this does: draw on a slide, commit, save,
//! reopen the saved file, read the annotation back out, and convert it to the
//! stroke a slide would draw. If those two strokes differ, a mark moves
//! between the talk and the file — and nobody would find out until afterwards.

use std::path::Path;

use pulpit_core::annotate::presenter::{ink_to_stroke, kind_of, stroke_to_draft, SlidePlacement};
use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, AnnotationKind};
use pulpit_core::annotation::{InkColor, InkStroke, StrokeKind};
use pulpit_core::notes::Region;
use pulpit_core::page::PageIndex;
use pulpit_render::document::{DocumentRevision, DocumentTransaction, PdfDocument, SaveOptions};

mod common;

/// The stroke a presenter draws: a wave, so a mark that lands mirrored or
/// transposed is visible in the comparison rather than symmetric under it.
fn drawn(kind: StrokeKind) -> InkStroke {
    InkStroke {
        points: (0..24)
            .map(|step| {
                let along = step as f32 / 23.0;
                (0.1 + along * 0.7, 0.3 + (along * 6.0).sin() * 0.15)
            })
            .collect(),
        width: 0.004,
        color: InkColor::Cyan,
        kind,
        id: None,
    }
}

/// Every mark the file holds on `page`, as the strokes a slide would draw.
fn read_back(document: &mut PdfDocument<'_>, placement: &SlidePlacement) -> Vec<InkStroke> {
    document
        .annotations(placement.page)
        .expect("the page is readable")
        .into_iter()
        .filter(|summary| summary.kind == AnnotationKind::Ink && !summary.geometry_elided)
        .filter_map(|summary| {
            ink_to_stroke(
                summary.id.clone(),
                summary.page,
                &summary.path,
                summary.style.color,
                summary.style.width,
                kind_of(&summary.style),
                placement,
            )
        })
        .collect()
}

fn same_shape(drawn: &InkStroke, read: &InkStroke) {
    assert_eq!(
        drawn.points.len(),
        read.points.len(),
        "the mark came back with a different number of points"
    );
    for (index, (drawn, read)) in drawn.points.iter().zip(&read.points).enumerate() {
        // A thousandth of the slide. A presenter's slide is a metre or two
        // across, so this is well under a millimetre on the wall — and it is
        // *not* loose enough to hide a mirrored axis or a half-page offset,
        // which are the failures that matter.
        assert!(
            (drawn.0 - read.0).abs() < 1e-3 && (drawn.1 - read.1).abs() < 1e-3,
            "point {index} was drawn at {drawn:?} and came back at {read:?}"
        );
    }
    assert!(
        (drawn.width - read.width).abs() < 1e-4,
        "{} vs {}",
        drawn.width,
        read.width
    );
    assert_eq!(drawn.color, read.color);
    assert_eq!(drawn.kind, read.kind);
}

/// The presenter's round trip on one slide layout.
fn round_trip(region: Region, kind: StrokeKind) {
    let Some(mut guard) = common::pdfium("the presenter-ink round trip") else {
        return;
    };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("deck.pdf");
    harness::write_deck(&source, 3).expect("a deck to annotate");

    let stroke = drawn(kind);
    let saved = directory.path().join("deck-with-marks.pdf");

    // Presenting: the pen comes up, and the mark becomes an annotation.
    let placement = {
        let engine = pulpit_render::document::pdfium::PdfiumDocument::open(&mut guard, &source)
            .expect("the deck opens");
        let mut document = PdfDocument::new(Box::new(engine), 31);
        let geometry = document
            .page_geometry(PageIndex(1))
            .expect("the deck has a second page");
        let placement = SlidePlacement::new(PageIndex(1), region, &geometry);
        assert!(placement.is_usable());

        let draft = stroke_to_draft(&stroke, &placement).expect("a drawn stroke commits");
        assert!(
            matches!(draft, AnnotationDraft::Ink(_)),
            "a presenter stroke is native ink, not a stamp (criterion 1)"
        );
        document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::from_annotations([AnnotationCommand::Create(draft)]),
            )
            .expect("the commit succeeds");

        // It is in the open document before anything is saved: this is what
        // document mode would be editing, in the same session (criterion 3).
        let live = read_back(&mut document, &placement);
        assert_eq!(live.len(), 1, "the committed mark is in the open document");
        same_shape(&stroke, &live[0]);

        document
            .save_as(&saved, SaveOptions::verified())
            .expect("the annotated deck saves");
        placement
    };

    // Afterwards: someone opens the saved file. The mark is there, in the same
    // place, with the same style (criterion 2).
    {
        let engine = pulpit_render::document::pdfium::PdfiumDocument::open(&mut guard, &saved)
            .expect("the saved deck reopens");
        let mut document = PdfDocument::new(Box::new(engine), 32);
        let reopened = read_back(&mut document, &placement);
        assert_eq!(reopened.len(), 1, "the saved file carries the mark");
        same_shape(&stroke, &reopened[0]);
    }

    // …and the source was not touched (A6, criterion 11).
    let untouched = pulpit_render::document::pdfium::PdfiumDocument::open(&mut guard, &source)
        .expect("the source still opens");
    let mut source_document = PdfDocument::new(Box::new(untouched), 33);
    assert!(
        read_back(&mut source_document, &placement).is_empty(),
        "the deck the presenter opened was written to"
    );
}

#[test]
fn a_mark_drawn_over_a_whole_slide_survives_the_file() {
    round_trip(Region::FULL, StrokeKind::Ink);
}

/// The unified reader pipeline's promise, end to end: an incremental
/// snapshot of an edited document, opened by the same backend the render
/// worker pool uses, draws the committed mark when `with_annotations` is on
/// — and leaves it out when it is off, which is what keeps the projector
/// clean of the document's own ink.
#[test]
fn a_snapshot_of_an_edited_document_renders_the_mark_through_the_pool_backend() {
    use pulpit_render::pdf::{NeverCancel, PdfBackend, RenderRequest};

    let Some(mut guard) = common::pdfium("the presenter-ink round trip") else {
        return;
    };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("deck.pdf");
    harness::write_deck(&source, 2).expect("a deck to annotate");
    let snapshot = directory.path().join("snapshot.pdf");

    // Commit a mark and snapshot the document the way the reader does:
    // incrementally, unverified — the renderer below is the verification.
    {
        let engine = pulpit_render::document::pdfium::PdfiumDocument::open(&mut guard, &source)
            .expect("the deck opens");
        let mut document = PdfDocument::new(Box::new(engine), 41);
        let geometry = document
            .page_geometry(PageIndex(0))
            .expect("the deck has a first page");
        let placement = SlidePlacement::new(PageIndex(0), Region::FULL, &geometry);
        let draft = stroke_to_draft(&drawn(StrokeKind::Ink), &placement).expect("a draft");
        document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::from_annotations([AnnotationCommand::Create(draft)]),
            )
            .expect("the commit succeeds");
        document
            .save_as(
                &snapshot,
                SaveOptions {
                    incremental: true,
                    verify: false,
                },
            )
            .expect("the snapshot saves");
    }

    let mut render = |path: &Path, with_annotations: bool| -> Vec<u8> {
        let handle = guard
            .open(path)
            .expect("the file opens in the pool backend");
        let request = RenderRequest {
            document: handle,
            page: 0,
            region: Region::FULL,
            width: 153,
            height: 198,
            full_size: None,
            with_annotations,
        };
        let mut pixels = vec![0u8; request.rgba_bytes() as usize];
        guard
            .render_into(&request, &mut pixels, &NeverCancel)
            .expect("the page renders");
        guard.close(handle);
        pixels
    };

    let pristine = render(&source, true);
    let marked = render(&snapshot, true);
    let unmarked = render(&snapshot, false);
    assert_ne!(
        pristine, marked,
        "the snapshot's pixels do not contain the committed mark"
    );
    assert_eq!(
        pristine, unmarked,
        "with annotations off, the snapshot must render like the source"
    );
}

#[test]
fn a_highlighter_mark_survives_the_file_as_a_highlighter_mark() {
    // The tool is carried by `/CA` rather than by a private key, so this is
    // also the check that a translucent mark does not come back opaque.
    round_trip(Region::FULL, StrokeKind::Highlight);
}

#[test]
fn a_mark_on_half_a_split_page_deck_lands_on_that_half() {
    // The case where slide space and page space genuinely differ. If the
    // region were ever dropped, this is the test that fails while the
    // whole-page one keeps passing.
    round_trip(Region::left_half(), StrokeKind::Ink);
    round_trip(Region::new(0.5, 0.0, 0.5, 1.0), StrokeKind::Ink);
}

#[test]
fn a_mark_made_in_document_mode_is_a_mark_the_slide_can_draw() {
    // The other direction of criterion 3, and the reason the conversion is
    // one function used both ways: an annotation nobody drew on a slide still
    // has to *be* drawable on one.
    let Some(mut guard) = common::pdfium("the presenter-ink round trip") else {
        return;
    };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let source = directory.path().join("paper.pdf");
    harness::write_deck(&source, 2).expect("a document");

    let engine = pulpit_render::document::pdfium::PdfiumDocument::open(&mut guard, &source)
        .expect("the document opens");
    let mut document = PdfDocument::new(Box::new(engine), 41);
    let geometry = document.page_geometry(PageIndex(0)).expect("a page");
    let placement = SlidePlacement::new(PageIndex(0), Region::FULL, &geometry);

    // Committed in page points, the way document mode does it — nothing here
    // went through the presenter's conversion on the way in.
    use pulpit_core::annotate::{InkDraft, InkPoint, MarkStyle};
    let points: Vec<InkPoint> = (0..8)
        .map(|step| InkPoint::new(100.0 + step as f32 * 20.0, 200.0 + (step % 3) as f32 * 15.0))
        .collect();
    document
        .apply(
            DocumentRevision::INITIAL,
            DocumentTransaction::from_annotations([AnnotationCommand::Create(
                AnnotationDraft::Ink(InkDraft {
                    page: PageIndex(0),
                    points: points.clone(),
                    style: MarkStyle::default(),
                }),
            )]),
        )
        .expect("document mode commits");

    let on_the_slide = read_back(&mut document, &placement);
    assert_eq!(on_the_slide.len(), 1, "the slide can draw it");
    let stroke = &on_the_slide[0];
    assert_eq!(stroke.points.len(), points.len());
    assert!(
        stroke.id.is_some(),
        "a mark the slide draws names the annotation it shows"
    );
    // Every point is on the slide, because the whole page is the slide.
    for point in &stroke.points {
        assert!(
            placement.contains(*point),
            "{point:?} is off a slide that is the whole page"
        );
    }
    // …and it is at the right end of the page: 200pt down a 792pt page is a
    // quarter of the way, not three quarters. An inverted y would pass every
    // "is it on the slide" check and fail this one.
    let expected = points[0].at.y / geometry.height;
    assert!(
        (stroke.points[0].1 - expected).abs() < 1e-3,
        "the first point is {} down the slide, expected {expected}",
        stroke.points[0].1
    );
}

/// A deck to annotate, shared by the tests above.
mod harness {
    use super::*;
    /// A plain letter-sized deck of `pages` pages with a word on each.
    pub fn write_deck(path: &Path, pages: usize) -> std::io::Result<()> {
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            Vec::new(), // the page tree, once the kids are numbered
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ];
        let mut kids = Vec::new();
        for page in 0..pages {
            let content = format!("BT /F1 36 Tf 72 700 Td (Slide {}) Tj ET\n", page + 1);
            let content_number = objects.len() + 2;
            kids.push(format!("{} 0 R", objects.len() + 1));
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 3 0 R >> >> /Contents {content_number} 0 R >>"
                )
                .into_bytes(),
            );
            let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
            stream.extend_from_slice(content.as_bytes());
            stream.extend_from_slice(b"endstream");
            objects.push(stream);
        }
        objects[1] = format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        )
        .into_bytes();

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
}
