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
//! * `text.djvu` — `book.djvu` with a hidden text layer on its first two
//!   pages and none on its third, added with `djvused set-txt` (§59.2).
//! * `rotated-text.djvu` — `rotated.djvu` with one word on its turned first
//!   page, which is the case `get_pagetext` reports differently from what
//!   `get_pageinfo` and `page_render` do.

use std::path::{Path, PathBuf};

use pulpit_core::notes::Region;
use pulpit_render::djvu::DjvuBackend;
use pulpit_render::document::DocumentBackend;
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

/// A book with no text layer at all answers no matches, and does not fail.
///
/// This is the one case §59.2 and §48.2 warn about, from the other side: while
/// this backend could not read a text layer, an empty list would have told a
/// presenter their term was absent from a book that may well contain it, so it
/// refused instead. Now that it reads one, an empty list means what it says —
/// these pages carry no hidden text, which `djvused print-txt` agrees with.
#[test]
fn a_book_with_no_text_layer_answers_no_matches() {
    let Some((_turn, backend, document)) = opened("book.djvu") else {
        return;
    };
    let query = pulpit_core::search::Query::new("anything", false, false);
    let hits = backend.find_text(document, &query, 0..3).unwrap();
    assert!(hits.is_empty(), "{hits:?}");
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

/// §59.2: the hidden text layer is what makes a scanned book searchable, and
/// the matcher is `pulpit_core::search`'s — the same one the PDF path runs.
///
/// `text.djvu` carries "hello world" and "Hello" on its first page, "world" on
/// its second, and nothing at all on its third.
#[test]
fn a_text_layer_is_searched_page_by_page() {
    let Some((_turn, backend, document)) = opened("text.djvu") else {
        return;
    };
    let query = pulpit_core::search::Query::new("world", false, false);
    let hits = backend.find_text(document, &query, 0..3).unwrap();
    let pages: Vec<_> = hits.iter().map(|hit| hit.page.0).collect();
    assert_eq!(pages, vec![0, 1], "one on each page that says it: {hits:?}");
    for hit in &hits {
        assert_eq!(
            hit.source,
            pulpit_core::search::HitSource::PageText,
            "a DjVu hit is on the page itself"
        );
        assert!(!hit.quads.is_empty(), "a hit on a page has geometry");
        assert!(
            hit.context.contains("world"),
            "the results list shows the document's own words: {:?}",
            hit.context
        );
    }
}

/// A page with no text layer is not a failure.
///
/// A plate, a map, a page nobody ran OCR over: it has nothing to find, which
/// is a different fact from a document that cannot be searched, and it must
/// not stop the scan of the pages around it.
#[test]
fn a_page_with_no_text_finds_nothing_and_fails_nothing() {
    let Some((_turn, backend, document)) = opened("text.djvu") else {
        return;
    };
    let query = pulpit_core::search::Query::new("world", false, false);
    let hits = backend.find_text(document, &query, 2..3).unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

/// Case folding and multi-word matching are the matcher's, not this backend's.
///
/// "hello" and "world" are two zones with a gap between them, so a query
/// spanning both is also the check that words are joined by a separator: with
/// no space between them the page would read "helloworld" and this would find
/// nothing.
#[test]
fn a_match_spanning_two_words_is_highlighted_word_by_word() {
    let Some((_turn, backend, document)) = opened("text.djvu") else {
        return;
    };
    let query = pulpit_core::search::Query::new("HELLO WORLD", false, false);
    let hits = backend.find_text(document, &query, 0..1).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(
        hits[0].quads.len(),
        2,
        "one box per word, not one box from the first to the last: {:?}",
        hits[0].quads
    );

    let sensitive = pulpit_core::search::Query::new("HELLO WORLD", true, false);
    assert!(
        backend
            .find_text(document, &sensitive, 0..1)
            .unwrap()
            .is_empty(),
        "case sensitivity is the matcher's, and it is shared"
    );
}

/// The mirror image of `a_rotated_page_is_measured_once_and_not_turned_twice`,
/// and the second half of §56.6's finding.
///
/// `get_pageinfo` reports a rotated page's *turned* size; `get_pagetext`
/// reports its text in the *stored*, unturned image space, and says nothing
/// about it. Measured on djvulibre 3.5.30: this fixture's first page is stored
/// 120×80 and turned a quarter turn, `pageinfo` answers 80×120, and the text
/// s-expression still says `(page 0 0 120 80)`. So the rotation the renderer
/// applies for free must be applied to these coordinates by hand.
///
/// The word sits at (10,50)–(55,70) in the stored image, which is its upper
/// left. djvulibre turns counter-clockwise, so on the rendered page it belongs
/// near the *bottom* left — and it must land inside a page that is 19.2×28.8
/// points, which the unturned reading would not even fit inside.
#[test]
fn a_word_on_a_rotated_page_is_turned_the_way_the_renderer_turns_it() {
    let Some((_turn, backend, document)) = opened("rotated-text.djvu") else {
        return;
    };
    let size = backend.page_size(document, 0).unwrap();
    let query = pulpit_core::search::Query::new("corner", false, false);
    let hits = backend.find_text(document, &query, 0..1).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    let bounds = hits[0].quads[0].bounds();

    assert!(
        bounds.right <= size.width + 0.01 && bounds.bottom <= size.height + 0.01,
        "the box is off a {}×{} page: {bounds:?} — which is what reading the \
         text layer in the page's turned space produces",
        size.width,
        size.height
    );
    // 300dpi: the stored x range 10–55 becomes the vertical range 15.6–26.4pt
    // measured down the page, and the stored y range 50–70 becomes 2.4–7.2pt
    // across it.
    for (got, want) in [
        (bounds.left, 2.4),
        (bounds.top, 15.6),
        (bounds.right, 7.2),
        (bounds.bottom, 26.4),
    ] {
        assert!(
            (got - want).abs() < 0.05,
            "{bounds:?} is not the quarter turn the renderer makes"
        );
    }
}

/// An empty query is not a scan, and the reader's view answers the same
/// search the presenter's does — the same layer, through the same backend.
#[test]
fn the_reader_searches_the_same_text_the_presenter_does() {
    let Some(_turn) = backend().map(|(turn, _)| turn) else {
        return;
    };
    let document =
        pulpit_render::djvu::DjvuDocument::open(&fixture("text.djvu")).expect("djvulibre is here");
    let empty = pulpit_core::search::Query::new("", false, false);
    let chunk = document.find_text(&empty, 0..3).unwrap();
    assert!(chunk.hits.is_empty() && !chunk.truncated);

    let query = pulpit_core::search::Query::new("world", false, false);
    let chunk = document.find_text(&query, 0..3).unwrap();
    let pages: Vec<_> = chunk.hits.iter().map(|hit| hit.page.0).collect();
    assert_eq!(pages, vec![0, 1], "{:?}", chunk.hits);
    assert_eq!((chunk.from_page, chunk.to_page), (0, 3));
}
