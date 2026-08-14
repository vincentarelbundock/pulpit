//! End-to-end overlay discovery against the example deck.
//!
//! `examples/combined.pdf` is written in ordinary beamer — a `\href{run:...}`
//! around a poster, the convention pdfpc and Impressive already use — so this
//! doubles as the test that pulpit reads a *standard* deck. hyperref
//! turns `run:` into a `/Launch` action, so it also pins the rule that a
//! launch action is read as a media reference and never executed.
//!
//! Skipped with a message when PDFium or the deck is absent, so a checkout
//! without either still gets a green, honest run.

#![cfg(feature = "pdfium")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pulpit_core::overlay::{
    ContentKind, OverlayContent, OverlayDeclaration, OverlayIndex, OverlaySource,
};
use pulpit_render::pdf::overlays::declarations_from_links;
use pulpit_render::pdf::pdfium::PdfiumBackend;
use pulpit_render::pdf::PdfBackend;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn deck() -> Option<PathBuf> {
    let path = workspace_root().join("examples/combined.pdf");
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("skipping: examples/combined.pdf has not been built");
        None
    }
}

/// PDFium binds once per process, so every test shares one backend.
///
/// A panicking test poisons the mutex; recovering from that keeps one real
/// failure from cascading into five that say only `PoisonError`.
fn shared() -> Option<MutexGuard<'static, PdfiumBackend>> {
    static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
    let backend = BACKEND
        .get_or_init(|| {
            if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                std::env::set_var("PULPIT_PDFIUM_PATH", workspace_root().join("lib"));
            }
            match PdfiumBackend::bind() {
                Ok(backend) => Some(Mutex::new(backend)),
                Err(e) => {
                    eprintln!("skipping PDFium tests: {e}");
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

/// Every overlay the deck declares, grouped as the application groups them.
fn discover() -> Option<OverlayIndex> {
    let path = deck()?;
    let mut backend = shared()?;
    let document = backend.open(&path).expect("the example deck should open");
    let pages = backend.page_count(document).expect("page count");

    let mut per_page: BTreeMap<usize, Vec<OverlayDeclaration>> = BTreeMap::new();
    for page in 0..pages {
        let links = backend.links(document, page).unwrap_or_default();
        let (declarations, diagnostics) = declarations_from_links(&links);
        for problem in diagnostics {
            eprintln!("page {}: {problem}", page + 1);
        }
        if !declarations.is_empty() {
            per_page.insert(page, declarations);
        }
    }
    let labels = backend.page_labels(document).unwrap_or_default();
    backend.close(document);
    Some(OverlayIndex::build(&per_page, &labels))
}

fn source_of(overlay: &pulpit_core::PageOverlay) -> &OverlaySource {
    match &overlay.content {
        OverlayContent::AnimatedImage(spec) => &spec.source,
        OverlayContent::Video(spec) => &spec.source,
        OverlayContent::Web(spec) => &spec.bundle,
    }
}

#[test]
fn a_standard_beamer_deck_declares_overlays_at_all() {
    let Some(index) = discover() else { return };
    assert!(
        !index.is_empty(),
        "no overlays were found — hyperref turns `run:` into a /Launch action, \
         so this is what breaks if launch actions stop being read"
    );
}

#[test]
fn the_deck_demonstrates_every_content_kind() {
    let Some(index) = discover() else { return };
    let kinds: Vec<ContentKind> = index
        .all()
        .iter()
        .map(|overlay| overlay.content.kind())
        .collect();
    for expected in [
        ContentKind::AnimatedImage,
        ContentKind::Video,
        ContentKind::Web,
    ] {
        assert!(
            kinds.contains(&expected),
            "the deck should demonstrate {}, found {kinds:?}",
            expected.label()
        );
    }
}

#[test]
fn every_asset_the_deck_names_is_actually_beside_it() {
    // `run:` names files next to the document. A deck whose links point at
    // nothing would still parse, and would then fail silently on stage.
    let Some(index) = discover() else { return };
    let directory = workspace_root().join("examples");

    let mut checked = 0;
    for overlay in index.all() {
        let OverlaySource::External(asset) = source_of(overlay) else {
            continue;
        };
        let path = directory.join(&asset.path);
        assert!(
            path.is_file(),
            "overlay {} names {}, which does not exist",
            overlay.id,
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected several external assets, saw {checked}"
    );
}

#[test]
fn playback_intent_survives_the_latex_to_pdf_round_trip() {
    // The query string is the part LaTeX is most likely to mangle: `&` is an
    // alignment tab unless the deck fixes its catcode.
    let Some(index) = discover() else { return };
    let playback = |overlay: &pulpit_core::PageOverlay| match &overlay.content {
        OverlayContent::AnimatedImage(spec) => Some(spec.playback.clone()),
        OverlayContent::Video(spec) => Some(spec.playback.clone()),
        OverlayContent::Web(_) => None,
    };
    let all: Vec<_> = index.all().iter().filter_map(playback).collect();
    assert!(
        all.iter().any(|p| p.autoplay),
        "`?autostart` did not survive; the `&` was probably eaten by TeX"
    );
    assert!(all.iter().any(|p| p.repeat), "`?loop` did not survive");
    assert!(all.iter().any(|p| p.mute), "`?mute` did not survive");
}

#[test]
fn a_reveal_sequence_collapses_into_one_overlay_across_its_pages() {
    let Some(index) = discover() else { return };
    let spanning: Vec<_> = index
        .all()
        .iter()
        .filter(|overlay| overlay.pages.len() > 1)
        .collect();
    assert!(
        !spanning.is_empty(),
        "the incremental-reveal frame should produce one overlay covering several pages"
    );
    for overlay in spanning {
        for window in overlay.pages.windows(2) {
            assert_eq!(
                window[1],
                window[0] + 1,
                "an overlay's pages must be a contiguous run: {:?}",
                overlay.pages
            );
        }
        for page in &overlay.pages {
            assert!(
                index
                    .on_page(*page)
                    .iter()
                    .any(|other| other.id == overlay.id),
                "overlay {} is missing from page {page}",
                overlay.id
            );
        }
    }
}

#[test]
fn the_html_overlay_names_the_file_and_serves_its_directory() {
    let Some(index) = discover() else { return };
    let web = index
        .all()
        .iter()
        .find_map(|overlay| match &overlay.content {
            OverlayContent::Web(spec) => Some(spec.clone()),
            _ => None,
        })
        .expect("the deck should declare a web overlay");

    assert_eq!(web.entrypoint.0, "bouncing-balls.html");
    match &web.bundle {
        OverlaySource::External(asset) => {
            assert!(asset.path.ends_with("bouncing-balls.html"));
            let path = workspace_root().join("examples").join(&asset.path);
            assert!(path.is_file(), "{} is missing", path.display());
        }
        other => panic!("expected a file beside the document, got {other:?}"),
    }
}
