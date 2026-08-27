//! What other PDF software makes of pulpit's output (§13.4).
//!
//! Every other test in this workspace asks PDFium whether PDFium wrote what
//! PDFium was asked to write. That is worth checking and it is not the claim
//! the specification makes. The claim is that a mark pulpit writes is a *PDF
//! annotation* — visible, and editable, in ordinary PDF software — and only
//! ordinary PDF software can establish it.
//!
//! So these run MuPDF and Poppler over saved output and check what §13.4
//! lists: that the annotation objects are in `/Annots`; that a `/Highlight`'s
//! quads land on the intended text and are reported by another viewer's own
//! text tooling; that marks are visible *without* appearance regeneration;
//! and that a save and reopen does not shift geometry on a rotated page.
//!
//! Skipped with a message when PDFium is missing, and each check skips
//! individually when the tool it needs is not installed — a green run on a
//! bare machine has established nothing, and says so.

#![cfg(feature = "pdfium")]

use std::path::{Path, PathBuf};

use crate::testkit::Engines;
use pulpit_core::annotate::{
    AnnotationCommand, AnnotationDraft, HighlightDraft, InkDraft, InkPoint, MarkStyle,
};
use pulpit_core::page::PageIndex;
use pulpit_render::document::pdfium::PdfiumDocument;
use pulpit_render::document::{
    DocumentRevision, DocumentTransaction, PdfDocument, SaveOptions, TextSelection,
};
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_render::pdf::synth::write_pdf;

mod common;
mod signing_fixture;
mod testkit;

fn temp_dir(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("pulpit-cross-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn source(directory: &Path) -> PathBuf {
    let path = directory.join("source.pdf");
    write_pdf(&path, 2, None).unwrap();
    path
}

fn ink(page: usize) -> DocumentTransaction {
    DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
        InkDraft {
            page: PageIndex(page),
            points: vec![
                InkPoint::new(80.0, 60.0),
                InkPoint::new(300.0, 120.0),
                InkPoint::new(500.0, 70.0),
            ],
            style: MarkStyle::default(),
        },
    ))])
}

/// Save a document with one ink stroke on page zero, and hand back the path.
fn annotated(backend: &mut PdfiumBackend, directory: &Path) -> PathBuf {
    let path = source(directory);
    let destination = directory.join("annotated.pdf");
    let engine = PdfiumDocument::open(backend, &path).expect("the document opens");
    let mut document = PdfDocument::new(Box::new(engine), 21);
    document
        .apply(DocumentRevision::INITIAL, ink(0))
        .expect("the stroke commits");
    document
        .save_as(&destination, SaveOptions::verified())
        .expect("the save succeeds");
    destination
}

#[test]
fn another_reader_finds_the_annotation_in_the_pages_annots_array() {
    let Some(mut guard) = common::pdfium("the cross-viewer tests") else {
        return;
    };
    let engines = Engines::detect();
    if !engines.can_read_objects("the /Annots check") {
        return;
    }
    let directory = temp_dir("annots");
    let destination = annotated(&mut guard, &directory);

    let objects = engines
        .objects(&destination)
        .expect("MuPDF parses pulpit's output");
    assert!(
        objects.iter().any(|line| line.contains("/Annots")),
        "no /Annots array in the saved file"
    );
    assert!(
        objects
            .iter()
            .any(|line| line.contains("/Subtype") && line.contains("/Ink")),
        "the mark is not an /Ink annotation as another reader sees it"
    );
    // A3, from outside pulpit entirely: the identity is in the file.
    assert!(
        objects.iter().any(|line| line.contains("/NM")),
        "the annotation carries no /NM, so nothing can find it again"
    );
    // The geometry other software edits by, not only the appearance pulpit
    // generated (§7.1).
    assert!(
        objects.iter().any(|line| line.contains("/InkList")),
        "the stroke has no /InkList, so another editor cannot edit it"
    );
}

#[test]
fn another_renderer_draws_the_mark_without_regenerating_its_appearance() {
    // §7.1: the appearance is authoritative, and a viewer that does not
    // synthesise missing ones must still show the mark.
    let Some(mut guard) = common::pdfium("the cross-viewer tests") else {
        return;
    };
    let engines = Engines::detect();
    if !engines.can_render("the visibility check") {
        return;
    }
    let directory = temp_dir("visible");
    let path = source(&directory);
    let clean = directory.join("clean.pdf");
    {
        let engine = PdfiumDocument::open(&mut guard, &path).unwrap();
        let mut document = PdfDocument::new(Box::new(engine), 1);
        document
            .save_as(&clean, SaveOptions::verified())
            .expect("a copy with no marks");
    }
    let marked = annotated(&mut guard, &directory);

    let before = engines
        .render(&clean, 0, 72)
        .expect("another renderer draws the page");
    let after = engines
        .render(&marked, 0, 72)
        .expect("another renderer draws the annotated page");

    let added = after.dark_total(200).saturating_sub(before.dark_total(200));
    assert!(
        added > 50,
        "another renderer drew {added} more dark pixels; the mark is not visible outside pulpit"
    );
}

#[test]
fn a_highlights_quads_land_on_the_text_another_reader_extracts() {
    // §13.4 and criterion 4: the marked region has to be the text, as some
    // other implementation understands both.
    let Some(mut guard) = common::pdfium("the cross-viewer tests") else {
        return;
    };
    let engines = Engines::detect();
    if !engines.can_read_objects("the /QuadPoints check") {
        return;
    }
    let directory = temp_dir("quads");
    let path = source(&directory);
    let destination = directory.join("highlighted.pdf");

    let selected = {
        let engine = PdfiumDocument::open(&mut guard, &path).unwrap();
        let mut document = PdfDocument::new(Box::new(engine), 5);
        let geometry = document.page_geometry(PageIndex(0)).unwrap();
        let at = geometry.from_user_space(320.0, 200.0);
        let word = document
            .select_text(PageIndex(0), TextSelection::Word { at })
            .expect("the page has text");
        assert!(!word.is_empty(), "the fixture's text was not found");

        document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::from_annotations([AnnotationCommand::Create(
                    AnnotationDraft::Highlight(HighlightDraft {
                        kind: pulpit_core::annotation::MarkupKind::Highlight,
                        page: PageIndex(0),
                        quads: word.quads.clone(),
                        text: word.text.clone(),
                        style: MarkStyle::highlighter(),
                    }),
                )]),
            )
            .expect("the highlight commits");
        document
            .save_as(&destination, SaveOptions::verified())
            .unwrap();
        word.text
    };

    let objects = engines.objects(&destination).expect("MuPDF parses it");
    assert!(
        objects
            .iter()
            .any(|line| line.contains("/Subtype") && line.contains("/Highlight")),
        "the mark is not a /Highlight to another reader"
    );
    assert!(
        objects.iter().any(|line| line.contains("/QuadPoints")),
        "the highlight carries no /QuadPoints, so its region is only its appearance"
    );

    // The selected text is recoverable from /Contents, and it is text this
    // document actually contains — checked against another implementation's
    // extraction rather than against PDFium's own.
    if let Some(extracted) = engines.text(&destination) {
        let wanted = selected.trim();
        if !wanted.is_empty() {
            assert!(
                extracted.contains(wanted),
                "pulpit highlighted {wanted:?}, which another reader does not find in the page"
            );
        }
    }
}

/// §7.6: a visible signature is a picture in a `/Stamp`, with the bytes
/// embedded and no path to a source file left in the annotation.
#[test]
fn a_picture_stamp_is_embedded_and_visible_to_another_renderer() {
    let Some(mut guard) = common::pdfium("the cross-viewer tests") else {
        return;
    };
    let engines = Engines::detect();
    if !engines.can_render("the picture stamp check") {
        return;
    }
    let directory = temp_dir("stamp");
    let path = source(&directory);
    let clean = directory.join("clean.pdf");
    {
        let engine = PdfiumDocument::open(&mut guard, &path).unwrap();
        let mut document = PdfDocument::new(Box::new(engine), 2);
        document.save_as(&clean, SaveOptions::verified()).unwrap();
    }

    // A solid black square: a picture whose presence is unmistakable when the
    // question is whether another renderer drew it at all.
    let (pixel_width, pixel_height) = (32u32, 32u32);
    let rgba = vec![0u8; pixel_width as usize * pixel_height as usize * 4]
        .chunks(4)
        .flat_map(|_| [0u8, 0, 0, 255])
        .collect::<Vec<u8>>();

    let destination = directory.join("stamped.pdf");
    {
        let engine = PdfiumDocument::open(&mut guard, &path).unwrap();
        let mut document = PdfDocument::new(Box::new(engine), 3);
        let transaction = DocumentTransaction::from_annotations([AnnotationCommand::Create(
            AnnotationDraft::Stamp(pulpit_core::annotate::StampDraft {
                page: PageIndex(0),
                rect: pulpit_core::page::PageRect::new(60.0, 60.0, 180.0, 180.0),
                mark: pulpit_core::annotate::StampMark::Image {
                    pixel_width,
                    pixel_height,
                    rgba,
                },
                style: MarkStyle::default(),
                source: None,
            }),
        )]);
        document
            .apply(DocumentRevision::INITIAL, transaction)
            .expect("the picture stamp commits");
        document
            .save_as(&destination, SaveOptions::verified())
            .unwrap();
    }

    if let Some(objects) = engines.objects(&destination) {
        assert!(
            objects
                .iter()
                .any(|line| line.contains("/Subtype") && line.contains("/Stamp")),
            "the mark is not a /Stamp to another reader"
        );
        // The picture is *in* the file. A stamp naming a path on disk would
        // break when the file moved and would read whatever the document
        // pointed at (§12).
        assert!(
            objects
                .iter()
                .any(|line| line.contains("/Subtype") && line.contains("/Image")),
            "no image XObject: the picture was not embedded"
        );
        assert!(
            !objects
                .iter()
                .any(|line| line.contains("/F ") && line.contains("/FileSpec")),
            "the stamp refers to a file outside the document"
        );
    }

    let before = engines.render(&clean, 0, 72).expect("a rendered page");
    let after = engines
        .render(&destination, 0, 72)
        .expect("a rendered stamped page");
    let added = after.dark_total(200).saturating_sub(before.dark_total(200));
    assert!(
        added > 100,
        "another renderer drew {added} more dark pixels; the picture is not visible"
    );
}

/// §7.4: a generated mark shows *somewhere else* as a picture and reopens
/// *here* as its source.
///
/// The markup is stood in for rather than compiled — this crate has no Typst —
/// but everything the specification asks of the annotation is the same either
/// way: a standard subtype with a generated appearance, a plain fallback in
/// `/Contents`, and the source in pulpit's namespaced entry.
#[test]
fn a_generated_mark_carries_its_source_and_shows_its_appearance() {
    let Some(mut guard) = common::pdfium("the cross-viewer tests") else {
        return;
    };
    let engines = Engines::detect();
    if !engines.can_read_objects("the generated-mark check") {
        return;
    }
    let directory = temp_dir("generated");
    let path = source(&directory);
    let destination = directory.join("generated.pdf");
    let markup = "$ integral_0^1 x^2 dif x $";

    let (pixel_width, pixel_height) = (24u32, 12u32);
    let rgba = (0..pixel_width * pixel_height)
        .flat_map(|_| [0u8, 0, 0, 255])
        .collect::<Vec<u8>>();

    {
        let engine = PdfiumDocument::open(&mut guard, &path).unwrap();
        let mut document = PdfDocument::new(Box::new(engine), 4);
        let transaction = DocumentTransaction::from_annotations([AnnotationCommand::Create(
            AnnotationDraft::Stamp(pulpit_core::annotate::StampDraft {
                page: PageIndex(0),
                rect: pulpit_core::page::PageRect::new(72.0, 260.0, 216.0, 332.0),
                mark: pulpit_core::annotate::StampMark::Image {
                    pixel_width,
                    pixel_height,
                    rgba,
                },
                style: MarkStyle::default(),
                source: Some(markup.to_string()),
            }),
        )]);
        document
            .apply(DocumentRevision::INITIAL, transaction)
            .expect("the generated mark commits");
        document
            .save_as(&destination, SaveOptions::verified())
            .unwrap();
    }

    // Reopened, pulpit finds the source and offers it for editing.
    let reopened = PdfiumDocument::open(&mut guard, &destination).unwrap();
    let document = PdfDocument::new(Box::new(reopened), 6);
    let annotations = document.annotations(PageIndex(0)).unwrap();
    assert_eq!(annotations.len(), 1);
    assert_eq!(
        annotations[0].contents.pulpit_source.as_deref(),
        Some(markup),
        "the markup did not survive, so the mark cannot be reopened for editing"
    );
    // …and the plain fallback is there for anything that reads /Contents.
    assert_eq!(annotations[0].contents.text, markup);

    // Another reader sees a standard subtype with a picture in it, and is not
    // asked to understand Typst.
    let objects = engines.objects(&destination).expect("MuPDF parses it");
    assert!(
        objects
            .iter()
            .any(|line| line.contains("/Subtype") && line.contains("/Stamp")),
        "a generated mark must be an ordinary annotation to other software"
    );
    assert!(
        objects
            .iter()
            .any(|line| line.contains("/Subtype") && line.contains("/Image")),
        "the generated appearance is not in the file"
    );
}

#[test]
fn saving_and_reopening_does_not_shift_geometry_on_a_rotated_page() {
    // §13.4's rotation check, done where it can be seen: the mark is drawn on
    // a rotated page, saved, and another renderer is asked where the ink is.
    let Some(mut guard) = common::pdfium("the cross-viewer tests") else {
        return;
    };
    let engines = Engines::detect();
    if !engines.can_render("the rotation check") {
        return;
    }
    let directory = temp_dir("rotated");
    let path = directory.join("rotated.pdf");
    if write_rotated_pdf(&path, 90).is_err() {
        eprintln!("skipping: cannot write a rotated fixture");
        return;
    }

    // A copy with no marks, so the comparison below is about what the *mark*
    // added rather than about where the page's own content happens to be.
    let clean = directory.join("rotated-clean.pdf");
    {
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the rotated page opens");
        let mut document = PdfDocument::new(Box::new(engine), 8);
        document.save_as(&clean, SaveOptions::verified()).unwrap();
    }

    let destination = directory.join("rotated-annotated.pdf");
    let bounds = {
        let engine = PdfiumDocument::open(&mut guard, &path).expect("the rotated page opens");
        let mut document = PdfDocument::new(Box::new(engine), 9);
        let geometry = document.page_geometry(PageIndex(0)).unwrap();
        assert_eq!(
            geometry.rotation,
            pulpit_core::page::PageRotation::Clockwise90,
            "the fixture is not rotated, so this proves nothing"
        );
        // A stroke in the top-left quarter of the page *as displayed*.
        let transaction = DocumentTransaction::from_annotations([AnnotationCommand::Create(
            AnnotationDraft::Ink(InkDraft {
                page: PageIndex(0),
                points: vec![
                    InkPoint::new(geometry.width * 0.10, geometry.height * 0.10),
                    InkPoint::new(geometry.width * 0.35, geometry.height * 0.30),
                ],
                style: MarkStyle {
                    width: 8.0,
                    ..MarkStyle::default()
                },
            }),
        )]);
        document
            .apply(DocumentRevision::INITIAL, transaction)
            .expect("the stroke commits on a rotated page");
        let bounds = document.annotations(PageIndex(0)).unwrap()[0].bounds;
        document
            .save_as(&destination, SaveOptions::verified())
            .unwrap();
        bounds
    };
    // Canonical space is the displayed page, so a mark in its top-left
    // quarter has small coordinates whatever /Rotate says.
    assert!(
        bounds.left < 200.0 && bounds.top < 200.0,
        "the mark is not where it was drawn in canonical space: {bounds:?}"
    );

    // …and another renderer, which applies the rotation itself, finds the ink
    // in the same quarter of what it draws.
    let before = engines
        .render(&clean, 0, 72)
        .expect("another renderer draws the rotated page");
    let after = engines
        .render(&destination, 0, 72)
        .expect("another renderer draws the annotated rotated page");

    let added_inside = after
        .dark_in(0.0, 0.0, 0.5, 0.5, 200)
        .saturating_sub(before.dark_in(0.0, 0.0, 0.5, 0.5, 200));
    let added_outside = after
        .dark_outside(0.0, 0.0, 0.5, 0.5, 200)
        .saturating_sub(before.dark_outside(0.0, 0.0, 0.5, 0.5, 200));
    assert!(
        added_inside > 0,
        "the mark is not visible on the rotated page at all"
    );
    assert!(
        added_inside > added_outside,
        "the mark moved when the page was rotated: it added {added_inside} dark pixels \
         to the quarter it was drawn in and {added_outside} elsewhere"
    );
}

/// A one-page PDF whose page carries `/Rotate degrees` and some text.
///
/// Written here rather than taken from `synth`, which writes unrotated pages:
/// the rotation is the whole point of the case, and a fixture that quietly
/// lost it would make the test pass for the wrong reason.
fn write_rotated_pdf(path: &Path, degrees: i32) -> std::io::Result<()> {
    let content = b"BT /F1 96 Tf 120 500 Td (Rotated) Tj ET\n\
                    1 0 0 RG 6 w 40 40 520 700 re S\n";
    let mut pdf = crate::testkit::builder::Pdf::new();
    pdf.add("<< /Type /Catalog /Pages 2 0 R >>");
    pdf.add("<< /Type /Pages /Kids [4 0 R] /Count 1 >>");
    pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    pdf.add(format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate {degrees} \
         /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>"
    ));
    pdf.add_stream("", content);
    std::fs::write(path, pdf.build())
}

/// §25.5, checked where it matters: a signature drawn into a field's own box
/// on a page that is not page 0 has to be *visible*, in an engine that is not
/// PDFium, on that page and nowhere else.
///
/// PDFium agreeing with PDFium about an `/AP /N` stream would establish
/// nothing here; MuPDF or Poppler drawing ink inside the sender's box is the
/// claim the flow makes.
#[test]
fn another_renderer_draws_the_signature_inside_the_senders_box() {
    let engines = Engines::detect();
    if !engines.can_render("the visible signature appearance") {
        return;
    }
    let Some(credential) = signing_fixture::load_test_credential() else {
        signing_fixture::skip_message();
        return;
    };

    let directory = temp_dir("signature-appearance");
    let unsigned = directory.join("contract.pdf");
    let signed = directory.join("signed.pdf");
    std::fs::write(
        &unsigned,
        signing_fixture::build_unsigned_pdf_multipage(
            3,
            &[signing_fixture::FixtureField {
                name: "Recipient",
                page: 2,
                rect: [100.0, 100.0, 340.0, 250.0],
            }],
        ),
    )
    .expect("write the unsigned contract");

    let mut request = pulpit_render::sign::SignRequest {
        signing_time: signing_fixture::SIGNING_TIME_UNIX,
        field: pulpit_render::sign::SignTarget::ExistingField("Recipient".to_string()),
        id2: [7u8; 16],
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
        .expect("signing into the field's own box succeeds");

    let before = engines
        .render(&unsigned, 2, 72)
        .expect("another renderer draws the unsigned last page");
    let after = engines
        .render(&signed, 2, 72)
        .expect("another renderer draws the signed last page");
    // The comparison cannot be "more dark pixels than before": an empty
    // signature widget is exactly the thing viewers synthesise an appearance
    // for, and MuPDF draws an empty box in the same rect. What distinguishes
    // ours is *where* the ink is — the strokes cross the middle of the box,
    // which a synthesised border never touches.
    //
    // The rect is [100 100 340 250] on a 612x792 page; this is its middle
    // fifth-to-fourth-fifth, in normalized coordinates with y downward.
    let inner = (0.2418f32, 0.7222f32, 0.4771f32, 0.8359f32);
    let before_inner = before.ink(inner.0, inner.1, inner.2, inner.3, 200);
    let after_inner = after.ink(inner.0, inner.1, inner.2, inner.3, 200);
    assert!(
        after_inner > before_inner + 0.01,
        "another renderer drew {after_inner} ink inside the field's box against \
         {before_inner} before signing; the appearance is not visible outside pulpit"
    );

    let first_before = engines
        .render(&unsigned, 0, 72)
        .expect("another renderer draws the unsigned first page");
    let first_after = engines
        .render(&signed, 0, 72)
        .expect("another renderer draws the signed first page");
    assert_eq!(
        first_before.dark_total(200),
        first_after.dark_total(200),
        "the appearance must not touch a page it was not placed on"
    );
}

/// §25.5 on a quarter-turned page, checked where a reader would notice: the
/// mark has to land in the box *as displayed*, and the ink inside it has to
/// run the way it was drawn rather than sideways.
///
/// PDFium agreeing with itself would establish nothing; MuPDF and Poppler
/// both apply `/Rotate` themselves, so what they draw is what a reader of the
/// signed file sees. Without the appearance stream's counter-rotating
/// `/Matrix` this test fails on the second assertion: the mark is in the
/// right box and lying on its side.
#[test]
fn another_renderer_draws_a_rotated_page_signature_upright() {
    let engines = Engines::detect();
    if !engines.can_render("the rotated-page signature appearance") {
        return;
    }
    let Some(credential) = signing_fixture::load_test_credential() else {
        signing_fixture::skip_message();
        return;
    };

    let directory = temp_dir("signature-rotated");
    let unsigned = directory.join("rotated.pdf");
    let signed = directory.join("signed.pdf");
    std::fs::write(
        &unsigned,
        signing_fixture::build_unsigned_pdf_pages(
            &[signing_fixture::FixturePage::rotated(90)],
            &[],
        ),
    )
    .expect("write the unsigned rotated page");

    // A 612x792 sheet turned a quarter clockwise displays 792x612. This user
    // rect is 70 wide by 200 tall on the sheet, which is the 200x70 box a
    // reader sees; `PageGeometry::rect_from_user_space` maps its corners to
    // the displayed (296, 271) and (496, 341).
    let mut request = pulpit_render::sign::SignRequest {
        signing_time: signing_fixture::SIGNING_TIME_UNIX,
        field: pulpit_render::sign::SignTarget::NewInvisibleField { name: None },
        id2: [9u8; 16],
        ..Default::default()
    };
    request.appearance = Some(pulpit_render::sign::SignAppearance {
        placement: pulpit_render::sign::AppearancePlacement::Rect {
            page_index: 0,
            rect: [271.0, 296.0, 341.0, 496.0],
        },
        content: pulpit_render::sign::AppearanceContent::Ink {
            strokes: vec![vec![(0.05, 0.5), (0.95, 0.5)]],
            stroke_width: 6.0,
        },
        page_rotation: pulpit_render::sign::AppearanceRotation::Cw90,
    });
    pulpit_render::sign::sign_document_file(&unsigned, &signed, &credential, &request)
        .expect("signing the rotated page succeeds");

    let after = engines
        .render(&signed, 0, 72)
        .expect("another renderer draws the signed rotated page");
    assert_eq!(
        (after.width, after.height),
        (792, 612),
        "the renderer is not applying /Rotate, so this proves nothing"
    );

    // The box as displayed, in normalized coordinates with y downward.
    let (left, top, right, bottom) = (296.0 / 792.0, 271.0 / 612.0, 496.0 / 792.0, 341.0 / 612.0);
    assert!(
        after.dark_in(left, top, right, bottom, 200) > 0,
        "the signature is not visible on the rotated page at all"
    );
    assert_eq!(
        after.dark_outside(left, top, right, bottom, 200),
        0,
        "the mark must land inside the box the rect names, and nowhere else"
    );

    // The stroke was drawn straight across the box. Across means across: the
    // middle third of the box's height holds nearly all of it, and the middle
    // third of its width holds only the part that crosses there.
    let third_height = (bottom - top) / 3.0;
    let third_width = (right - left) / 3.0;
    let along = after.dark_in(left, top + third_height, right, bottom - third_height, 200);
    let across = after.dark_in(left + third_width, top, right - third_width, bottom, 200);
    assert!(
        along > 2 * across,
        "the ink is not lying the way it was drawn: {along} dark pixels along the box's \
         middle band against {across} across it, which is what a signature turned on its \
         side looks like"
    );
}
