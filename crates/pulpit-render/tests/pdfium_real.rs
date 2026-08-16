//! Real-PDFium tests. Skipped with a message when no `libpdfium` is
//! installed, so a machine without one still gets a green, honest test run
//! (`scripts/fetch-pdfium.sh` installs a pinned copy).

#![cfg(feature = "pdfium")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use pulpit_core::notes::Region;
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_render::pdf::synth::write_pdf;
use pulpit_render::pdf::{NeverCancel, PdfBackend, PdfError, RenderRequest};

fn workspace_lib() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib")
}

fn shared() -> Option<&'static Mutex<PdfiumBackend>> {
    static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                std::env::set_var("PULPIT_PDFIUM_PATH", workspace_lib());
            }
            match PdfiumBackend::bind() {
                Ok(backend) => Some(Mutex::new(backend)),
                Err(e) => {
                    eprintln!("skipping PDFium tests: {e}");
                    None
                }
            }
        })
        .as_ref()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pulpit-pdfium-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn opens_and_renders_a_real_pdf() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let path = temp_dir("render").join("deck.pdf");
    write_pdf(&path, 5, Some("pulpit:mapping=slides-only")).unwrap();

    let document = backend.open(&path).unwrap();
    let metadata = backend.metadata(document).unwrap();
    assert_eq!(metadata.page_count, 5);
    assert!((metadata.first_page_size.width - 720.0).abs() < 1.0);
    assert_eq!(
        pulpit_core::NotesMapping::from_metadata(&metadata.metadata_text),
        Some(pulpit_core::NotesMapping::SlidesOnly),
        "the metadata contract survives a real PDF round trip"
    );

    let page = backend
        .render(
            &RenderRequest {
                document,
                page: 2,
                region: Region::FULL,
                width: 640,
                height: 360,
                with_annotations: false,
            },
            &NeverCancel,
        )
        .unwrap();
    assert!(page.is_consistent());
    // The fixture draws a red border on white; there must be non-white pixels.
    assert!(
        page.pixels
            .chunks_exact(4)
            .any(|p| p[0] != 255 || p[1] != 255 || p[2] != 255),
        "the page rendered actual content"
    );
    backend.close(document);
}

#[test]
fn a_cropped_region_differs_from_the_full_page() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let path = temp_dir("crop").join("deck.pdf");
    write_pdf(&path, 2, None).unwrap();
    let document = backend.open(&path).unwrap();

    let full = backend
        .render(
            &RenderRequest {
                document,
                page: 0,
                region: Region::FULL,
                width: 200,
                height: 120,
                with_annotations: false,
            },
            &NeverCancel,
        )
        .unwrap();
    let left = backend
        .render(
            &RenderRequest {
                document,
                page: 0,
                region: Region::left_half(),
                width: 200,
                height: 120,
                with_annotations: false,
            },
            &NeverCancel,
        )
        .unwrap();
    assert_ne!(full.pixels, left.pixels, "split-page mappings really crop");
}

#[test]
fn an_already_cancelled_render_returns_promptly() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let path = temp_dir("cancel").join("deck.pdf");
    write_pdf(&path, 2, None).unwrap();
    let document = backend.open(&path).unwrap();

    let cancel = AtomicBool::new(true);
    let result = backend.render(
        &RenderRequest {
            document,
            page: 0,
            region: Region::FULL,
            width: 3840,
            height: 2160,
            with_annotations: false,
        },
        &cancel,
    );
    match result {
        Err(PdfError::Cancelled) => {}
        // A trivially small page can complete inside the first progressive
        // step; that is also acceptable, as long as it does not hang.
        Ok(_) => {}
        Err(e) => panic!("unexpected error: {e}"),
    }
    cancel.store(false, Ordering::Relaxed);
}

#[test]
fn a_malformed_document_is_rejected_without_panicking() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let path = temp_dir("malformed").join("broken.pdf");
    std::fs::write(&path, b"%PDF-1.7\nthis is not a pdf").unwrap();
    assert!(matches!(backend.open(&path), Err(PdfError::Open { .. })));

    let truncated = temp_dir("malformed").join("truncated.pdf");
    write_pdf(&truncated, 3, None).unwrap();
    let bytes = std::fs::read(&truncated).unwrap();
    std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    // Either PDFium recovers it or it refuses; it must not crash the process.
    let _ = backend.open(&truncated);
}

/// The whole embedded-notes path, through a real PDF name tree: a `*.pdfpc`
/// attachment written into the catalog, found by name, read back as bytes, and
/// parsed into notes keyed by page.
#[test]
fn embedded_pdfpc_notes_are_read_from_a_real_pdf() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();

    let payload = r#"{"pdfpcFormat":2,"pages":[
        {"idx":1,"note":"Open by naming the question."},
        {"idx":3,"note":"Give the point estimate first."}
    ]}"#;
    let dir = temp_dir("attachment");
    let path = dir.join("talk.pdf");
    pulpit_render::pdf::synth::write_pdf_with_attachment(
        &path,
        3,
        None,
        Some(("talk.pdfpc", payload)),
    )
    .unwrap();

    let document = backend.open(&path).expect("open the fixture");

    let names = backend.attachment_names(document).expect("read the names");
    assert_eq!(
        names,
        vec!["talk.pdfpc".to_string()],
        "the name tree carries exactly the attachment that was written"
    );

    let bytes = backend
        .attachment(document, "talk.pdfpc")
        .expect("read the attachment");
    let text = String::from_utf8(bytes).expect("the payload is text");
    let notes = pulpit_core::pdfpc::TextNotes::parse(&text).expect("a pdfpc payload");

    assert_eq!(notes.len(), 2);
    assert_eq!(notes.for_page(0), Some("Open by naming the question."));
    assert_eq!(notes.for_page(2), Some("Give the point estimate first."));
    assert_eq!(notes.for_page(1), None, "page two carries no note");

    backend.close(document);
    std::fs::remove_file(path).ok();
}

/// How long PDFium actually takes on a real deck, at the sizes the two
/// windows ask for.
///
/// Ignored because it is a measurement, not an assertion: it needs a deck
/// that is not a fixture, and there is no threshold that would be honest on
/// every machine. It exists because "the renders are slow" is the conclusion
/// every other measurement in this project has eventually pointed at, and the
/// only way to tell a slow rasteriser from a queue that is merely deep is to
/// time the rasteriser with nothing else running.
///
///     cargo test -p pulpit-render --test pdfium_real -- --ignored --nocapture
///
/// `PULPIT_BENCH_DECK` chooses the deck; it defaults to the stress deck in
/// `examples/`.
#[test]
#[ignore]
fn how_long_a_page_takes_to_rasterise() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let deck = std::env::var("PULPIT_BENCH_DECK").unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/stress-test-730.pdf")
            .to_string_lossy()
            .into_owned()
    });
    let path = PathBuf::from(&deck);
    if !path.exists() {
        eprintln!("skipping: no deck at {deck}");
        return;
    }
    let document = backend.open(&path).unwrap();
    let metadata = backend.metadata(document).unwrap();
    let sample: Vec<usize> = {
        let count = metadata.page_count.max(1);
        // Spread across the whole deck. Sampling only the front is how the
        // first run of this concluded that rasterising was cheap: a deck can
        // get heavier as it goes, and a presenter is rarely on page one.
        (0..12).map(|i| i * count / 12).collect()
    };
    let pages = sample.len();
    let aspect = metadata.first_page_size.width / metadata.first_page_size.height.max(1.0);
    println!("\n{deck}: {} pages", metadata.page_count);

    // The widths that actually get asked for: a warming thumbnail, a
    // presenter panel on a HiDPI display, and an audience frame.
    for width in [240u32, 480, 1024, 2048, 3840] {
        let height = ((width as f32 / aspect).max(1.0)) as u32;
        let mut total = 0.0f64;
        let mut worst = 0.0f64;
        for &page in &sample {
            let request = RenderRequest {
                document,
                page,
                region: Region::FULL,
                width,
                height,
                with_annotations: false,
            };
            let start = std::time::Instant::now();
            let rendered = backend.render(&request, &NeverCancel).unwrap();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            std::hint::black_box(rendered);
            total += ms;
            if ms > worst {
                worst = ms;
            }
        }
        println!(
            "  {width:>5}×{height:<5} {:>8.1} ms/page mean, {:>8.1} ms worst  ({pages} pages)",
            total / pages as f64,
            worst
        );
    }
}

/// Render one page of a document to RGBA at a fixed size, for comparing what
/// a file looked like before and after it was stamped.
fn frame(backend: &PdfiumBackend, document: pulpit_render::pdf::BackendDocumentId) -> Vec<u8> {
    backend
        .render(
            &RenderRequest {
                document,
                page: 0,
                region: Region::FULL,
                width: 400,
                height: 300,
                with_annotations: false,
            },
            &NeverCancel,
        )
        .unwrap()
        .pixels
}

fn red_stroke() -> pulpit_core::annotation::InkStroke {
    pulpit_core::annotation::InkStroke {
        // A diagonal across the middle of the page, well clear of its edges.
        points: vec![(0.2, 0.2), (0.5, 0.5), (0.8, 0.8)],
        width: 0.02,
        color: pulpit_core::annotation::InkColor::Red,
        kind: pulpit_core::annotation::StrokeKind::Ink,
    }
}

#[test]
fn an_exported_copy_carries_the_marks_and_leaves_the_source_alone() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let dir = temp_dir("export");
    let source = dir.join("deck.pdf");
    let destination = dir.join("deck-annotated.pdf");
    let _ = std::fs::remove_file(&destination);
    write_pdf(&source, 3, None).unwrap();
    let before = std::fs::read(&source).unwrap();

    let document = backend.open(&source).unwrap();
    let plain = frame(&backend, document);

    backend
        .export_annotated(
            &source,
            &destination,
            &[pulpit_render::pdf::PageStamp {
                page: 0,
                region: Region::FULL,
                strokes: vec![red_stroke()],
                images: Vec::new(),
            }],
        )
        .unwrap();

    assert_eq!(
        std::fs::read(&source).unwrap(),
        before,
        "an export never writes to the document it copied"
    );
    // The open document is the one the audience is rendered from; stamping a
    // copy must not have reached into it.
    assert_eq!(
        frame(&backend, document),
        plain,
        "the document held open for rendering is untouched"
    );

    let exported = backend.open(&destination).unwrap();
    assert_eq!(
        backend.page_count(exported).unwrap(),
        3,
        "every page survives"
    );
    let stamped = frame(&backend, exported);
    assert_ne!(stamped, plain, "the marks reached the page");

    // The stroke runs through the middle of the page, so the centre pixel is
    // the ink's colour rather than the slide's.
    let centre = (150 * 400 + 200) * 4;
    assert!(
        stamped[centre] > 180 && stamped[centre + 1] < 120 && stamped[centre + 2] < 120,
        "the centre of the page is red ink, not slide: {:?}",
        &stamped[centre..centre + 4]
    );
    // A corner the stroke never reaches is exactly as it was.
    let corner = (10 * 400 + 10) * 4;
    assert_eq!(stamped[corner..corner + 4], plain[corner..corner + 4]);

    backend.close(document);
    backend.close(exported);
}

#[test]
fn a_page_the_document_does_not_have_is_skipped_rather_than_failing_the_save() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let dir = temp_dir("export-range");
    let source = dir.join("deck.pdf");
    let destination = dir.join("out.pdf");
    let _ = std::fs::remove_file(&destination);
    write_pdf(&source, 2, None).unwrap();

    backend
        .export_annotated(
            &source,
            &destination,
            &[pulpit_render::pdf::PageStamp {
                page: 99,
                region: Region::FULL,
                strokes: vec![red_stroke()],
                images: Vec::new(),
            }],
        )
        .unwrap();

    let exported = backend.open(&destination).unwrap();
    assert_eq!(backend.page_count(exported).unwrap(), 2);
    backend.close(exported);
}

#[test]
fn a_malformed_picture_is_refused_before_a_file_is_written() {
    let Some(backend) = shared() else { return };
    let backend = backend.lock().unwrap();
    let dir = temp_dir("export-invalid");
    let source = dir.join("deck.pdf");
    let destination = dir.join("never.pdf");
    let _ = std::fs::remove_file(&destination);
    write_pdf(&source, 1, None).unwrap();

    let result = backend.export_annotated(
        &source,
        &destination,
        &[pulpit_render::pdf::PageStamp {
            page: 0,
            region: Region::FULL,
            strokes: Vec::new(),
            images: vec![pulpit_render::pdf::StampImage {
                x: 0.1,
                y: 0.1,
                width: 0.5,
                pixel_width: 4,
                pixel_height: 4,
                // Four pixels short of the size it claims.
                rgba: vec![0; 60],
            }],
        }],
    );
    assert!(matches!(result, Err(PdfError::Invalid(_))));
    assert!(
        !destination.exists(),
        "a refused export leaves no half-written file behind"
    );
}

#[test]
fn a_rasterised_annotation_lands_where_it_was_placed() {
    let Some(backend) = shared() else { return };
    let mut backend = backend.lock().unwrap();
    let dir = temp_dir("export-image");
    let source = dir.join("deck.pdf");
    let destination = dir.join("out.pdf");
    let _ = std::fs::remove_file(&destination);
    write_pdf(&source, 1, None).unwrap();

    // A solid opaque blue square, filling the left half of the page from a
    // quarter of the way down.
    let pixels = 32;
    let mut rgba = Vec::with_capacity(pixels * pixels * 4);
    for _ in 0..pixels * pixels {
        rgba.extend_from_slice(&[0, 0, 255, 255]);
    }
    backend
        .export_annotated(
            &source,
            &destination,
            &[pulpit_render::pdf::PageStamp {
                page: 0,
                region: Region::FULL,
                strokes: Vec::new(),
                images: vec![pulpit_render::pdf::StampImage {
                    x: 0.25,
                    y: 0.25,
                    width: 0.5,
                    pixel_width: pixels as u32,
                    pixel_height: pixels as u32,
                    rgba,
                }],
            }],
        )
        .unwrap();

    let exported = backend.open(&destination).unwrap();
    let stamped = frame(&backend, exported);
    // The picture spans x 0.25..0.75 of the width; its height follows its own
    // square aspect, so it covers y 0.25 down by the same number of points.
    let inside = (150 * 400 + 200) * 4;
    assert!(
        stamped[inside + 2] > 180 && stamped[inside] < 100,
        "the middle of the picture is blue: {:?}",
        &stamped[inside..inside + 4]
    );
    let outside = (10 * 400 + 380) * 4;
    assert!(
        stamped[outside + 2] < 200 || stamped[outside] > 100,
        "the top-right corner is untouched slide"
    );
    backend.close(exported);
}
