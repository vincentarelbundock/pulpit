//! DjVu against a real djvulibre (`SPEC-reader-formats.md` §55, §56, §63.2).
//!
//! Class B cannot run in CI without the library installed, so this follows the
//! PDFium precedent: every test skips **with a message** when djvulibre is
//! absent, because a green run that quietly skipped the meaningful tests is
//! the failure mode that matters. `PULPIT_REQUIRE_DJVU=1` turns the skip into
//! a failure, which is what a machine that is supposed to have the library
//! should set.
//!
//! The fixtures in `tests/djvu_fixture/` are built by djvulibre's own tools
//! and are a few hundred bytes each:
//!
//! * `book.djvu` — three bundled pages, 120×80, 80×120 and 100×100 at 300dpi,
//!   each a solid colour.
//! * `rotated.djvu` — the same bundle with page 1 turned a quarter turn, which
//!   is the case `get_pageinfo` reports differently from what `page_render`
//!   produces.
//! * `halves.djvu` — one 100×100 page, red over blue, for the two orientation
//!   flags that a solid colour cannot tell apart.

use std::path::{Path, PathBuf};

use pulpit_core::notes::Region;
use pulpit_render::djvu::DjvuBackend;
use pulpit_render::pdf::{BackendDocumentId, NeverCancel, PdfBackend, RenderRequest};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/djvu_fixture")
        .join(name)
}

/// Only one djvulibre context may exist per process, so only one test may
/// hold a backend at a time.
///
/// This is not a testing workaround; it is the invariant under test. Two
/// contexts driven from two threads make `open` return null for good files
/// about one run in seven, which is why `DjvuBackend::bind` refuses the second
/// one — and why the tests take a turn each rather than racing.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Held for the length of a test, so the next one binds only after this one's
/// backend has been dropped and its context released.
struct Turn {
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn take_a_turn() -> Turn {
    Turn {
        // A test that failed while holding this poisoned it; the next test
        // still needs its turn, and the failure is already reported.
        _guard: ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    }
}

/// A bound backend, or `None` with a visible skip (§63.2).
fn backend() -> Option<(Turn, DjvuBackend)> {
    let turn = take_a_turn();
    match DjvuBackend::bind() {
        Ok(backend) => Some((turn, backend)),
        Err(error) => {
            if std::env::var_os("PULPIT_REQUIRE_DJVU").is_some() {
                panic!("PULPIT_REQUIRE_DJVU is set but djvulibre would not bind: {error}");
            }
            eprintln!(
                "skipping: no djvulibre on this machine, so the DjVu backend was not exercised. \
                 Set PULPIT_REQUIRE_DJVU=1 to make this a failure."
            );
            None
        }
    }
}

fn opened(name: &str) -> Option<(Turn, DjvuBackend, BackendDocumentId)> {
    let (turn, mut backend) = backend()?;
    let document = backend
        .open(&fixture(name))
        .unwrap_or_else(|e| panic!("cannot open {name}: {e}"));
    Some((turn, backend, document))
}

fn request(document: BackendDocumentId, page: usize, width: u32, height: u32) -> RenderRequest {
    RenderRequest {
        document,
        page,
        region: Region::FULL,
        width,
        height,
        with_annotations: false,
        full_size: None,
    }
}

/// The pixel at `(x, y)` of a rendered frame.
fn pixel(frame: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let start = ((y * width + x) * 4) as usize;
    frame[start..start + 4].try_into().expect("four channels")
}

/// IW44 is a lossy wavelet codec, so a solid fill comes back close rather than
/// exact. Anything within this of the original is the colour that was encoded;
/// anything outside it is a different colour, a channel swap or an empty
/// frame — which is what these tests are actually looking for.
fn assert_close(actual: [u8; 4], expected: [u8; 3], what: &str) {
    for (channel, (got, want)) in actual.iter().zip(expected.iter()).enumerate() {
        let difference = (*got as i32 - *want as i32).abs();
        assert!(
            difference <= 24,
            "{what}: channel {channel} is {got}, expected about {want} \
             (whole pixel {actual:?}, expected about {expected:?})"
        );
    }
    assert_eq!(
        actual[3], 0xff,
        "{what}: a page is paper, never transparent"
    );
}

/// §56.3: page count and sizes are read without rendering anything.
#[test]
fn a_bundle_reports_every_page_at_its_true_size() {
    let Some((_turn, backend, document)) = opened("book.djvu") else {
        return;
    };
    let metadata = backend.metadata(document).unwrap();
    assert_eq!(metadata.page_count, 3);
    assert!(!metadata.page_sizes_sampled, "three pages are all measured");

    // 300dpi, so 120 pixels is 28.8pt. Points rather than pixels is what
    // makes a mixed-resolution scan report its pages at their true relative
    // sizes.
    let expected = [(28.8, 19.2), (19.2, 28.8), (24.0, 24.0)];
    for (page, (width, height)) in expected.iter().enumerate() {
        let size = backend.page_size(document, page).unwrap();
        assert!(
            (size.width - width).abs() < 0.01 && (size.height - height).abs() < 0.01,
            "page {page} is {}×{}, expected {width}×{height}",
            size.width,
            size.height
        );
        assert_eq!(
            metadata.page_sizes[page], size,
            "metadata agrees with the query"
        );
    }
}

/// A page with a stored rotation must be measured exactly once.
///
/// `ddjvuapi.h` documents rotation as applied by `page_render`,
/// `page_get_width` and `page_get_height`, and says nothing about
/// `get_pageinfo` — which reads as though the one call that answers *without*
/// decoding were the exception, and invites a backend to turn the dimensions
/// itself. It is not the exception. This fixture's first page is the same
/// 120×80 page as `book.djvu`'s, turned a quarter turn, and it must come back
/// portrait — not turned back to landscape by a second rotation.
#[test]
fn a_rotated_page_is_measured_once_and_not_turned_twice() {
    let Some((_turn, backend, document)) = opened("rotated.djvu") else {
        return;
    };
    let size = backend.page_size(document, 0).unwrap();
    assert!(
        size.height > size.width,
        "a page turned a quarter turn is portrait, but it reported {}×{} — \
         which is what applying the stored rotation a second time produces",
        size.width,
        size.height
    );
    assert!((size.width - 19.2).abs() < 0.01 && (size.height - 28.8).abs() < 0.01);

    // The page beside it is natively portrait and carries no rotation at all,
    // so it pins the measurement rather than the turning.
    let untouched = backend.page_size(document, 1).unwrap();
    assert!((untouched.width - 19.2).abs() < 0.01 && (untouched.height - 28.8).abs() < 0.01);
    assert_eq!(backend.metadata(document).unwrap().page_count, 3);

    let frame = backend
        .render(&request(document, 0, 40, 60), &NeverCancel)
        .unwrap();
    assert!(frame.is_consistent());
}

#[test]
fn a_full_page_render_carries_the_pages_colour_and_is_opaque() {
    let Some((_turn, backend, document)) = opened("book.djvu") else {
        return;
    };
    let colours = [[255, 0, 0], [0, 128, 255], [255, 255, 0]];
    for (page, colour) in colours.iter().enumerate() {
        let frame = backend
            .render(&request(document, page, 32, 32), &NeverCancel)
            .unwrap();
        assert!(frame.is_consistent(), "page {page} frame is well-formed");
        assert_close(
            pixel(&frame.pixels, 32, 16, 16),
            *colour,
            &format!("page {page} centre"),
        );
    }
}

/// Both of djvulibre's orientation flags at once. Its defaults are
/// PostScript-like — rows from the bottom, `y` from the bottom — and each has
/// to be flipped separately. Getting the row order wrong turns the page upside
/// down; getting the y direction wrong crops from the wrong end. A solid
/// colour cannot tell either apart, which is what this fixture is for.
#[test]
fn the_top_of_the_page_renders_at_the_top_and_crops_from_the_top() {
    let Some((_turn, backend, document)) = opened("halves.djvu") else {
        return;
    };
    let frame = backend
        .render(&request(document, 0, 40, 40), &NeverCancel)
        .unwrap();
    assert_close(
        pixel(&frame.pixels, 40, 20, 4),
        [220, 20, 20],
        "top of the full page",
    );
    assert_close(
        pixel(&frame.pixels, 40, 20, 35),
        [20, 20, 220],
        "bottom of the full page",
    );

    // The top half alone, which must be entirely red.
    let top = RenderRequest {
        region: Region::new(0.0, 0.0, 1.0, 0.5),
        ..request(document, 0, 40, 20)
    };
    let cropped = backend.render(&top, &NeverCancel).unwrap();
    assert!(cropped.is_consistent());
    assert_close(
        pixel(&cropped.pixels, 40, 20, 2),
        [220, 20, 20],
        "top crop, near its top",
    );
    assert_close(
        pixel(&cropped.pixels, 40, 20, 16),
        [220, 20, 20],
        "top crop, near its bottom — blue here means the crop came from the wrong end",
    );

    // And the bottom half is entirely blue, which rules out a crop that
    // simply ignored the region.
    let bottom = RenderRequest {
        region: Region::new(0.0, 0.5, 1.0, 0.5),
        ..request(document, 0, 40, 20)
    };
    let cropped = backend.render(&bottom, &NeverCancel).unwrap();
    assert_close(
        pixel(&cropped.pixels, 40, 20, 2),
        [20, 20, 220],
        "bottom crop, near its top",
    );
}

/// §56.4: djvulibre rasterises into the caller's buffer, and the override must
/// agree with the allocating path pixel for pixel.
#[test]
fn rendering_into_a_caller_buffer_matches_rendering_into_a_new_one() {
    let Some((_turn, backend, document)) = opened("halves.djvu") else {
        return;
    };
    let request = request(document, 0, 24, 24);
    let allocated = backend.render(&request, &NeverCancel).unwrap();
    let mut target = vec![0u8; 24 * 24 * 4];
    backend
        .render_into(&request, &mut target, &NeverCancel)
        .unwrap();
    assert_eq!(allocated.pixels, target);

    // A buffer too small is refused rather than written past.
    let mut small = vec![0u8; 24 * 24 * 4 - 1];
    assert!(backend
        .render_into(&request, &mut small, &NeverCancel)
        .is_err());
}

#[test]
fn a_page_past_the_end_is_refused_by_number() {
    let Some((_turn, backend, document)) = opened("book.djvu") else {
        return;
    };
    let error = backend.page_size(document, 7).unwrap_err().to_string();
    assert!(error.contains('7') && error.contains('3'), "{error}");
    assert!(backend
        .render(&request(document, 7, 16, 16), &NeverCancel)
        .is_err());
}

/// §59.2 and §48.2: "this cannot be searched" and "there are no matches" are
/// different facts about a document, and answering with an empty list would
/// tell a presenter their search term is absent from a book that may well
/// contain it.
#[test]
fn search_is_unsupported_rather_than_empty() {
    let Some((_turn, backend, document)) = opened("book.djvu") else {
        return;
    };
    let query = pulpit_core::search::Query::new("anything", false, false);
    let error = backend.find_text(document, &query, 0..3).unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("djvu"),
        "the refusal names the format: {error}"
    );
}

/// One worker holds several documents, and closing one must not disturb
/// another — the reload path holds two at once by construction.
#[test]
fn closing_one_document_leaves_the_others_standing() {
    let Some((_turn, mut backend)) = backend() else {
        return;
    };
    let book = backend.open(&fixture("book.djvu")).unwrap();
    let halves = backend.open(&fixture("halves.djvu")).unwrap();
    assert_ne!(book, halves);
    assert_eq!(backend.metadata(book).unwrap().page_count, 3);
    assert_eq!(backend.metadata(halves).unwrap().page_count, 1);

    backend.close(halves);
    assert!(
        backend.metadata(halves).is_err(),
        "a closed document is gone"
    );
    assert_eq!(
        backend.metadata(book).unwrap().page_count,
        3,
        "and the one still open is untouched"
    );
}

/// §61.2: a file that is not a DjVu at all fails to *open*, and never
/// silently produces a blank document.
#[test]
fn something_that_is_not_a_djvu_fails_to_open() {
    let Some((_turn, mut backend)) = backend() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let impostor = directory.path().join("not-really.djvu");
    std::fs::write(&impostor, b"%PDF-1.7\nthis is not a DjVu at all\n").unwrap();
    assert!(backend.open(&impostor).is_err());
}
