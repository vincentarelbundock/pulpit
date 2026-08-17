//! End-to-end overlay discovery against the example deck.
//!
//! `examples/beamer.pdf` is written in ordinary beamer — a `\href{run:...}`
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

use pulpit_core::overlay::{
    ContentKind, OverlayContent, OverlayDeclaration, OverlayIndex, OverlaySource,
};
use pulpit_render::pdf::overlays::declarations_from_links;
use pulpit_render::pdf::PdfBackend;

mod common;

fn deck() -> Option<PathBuf> {
    let path = common::workspace_root().join("examples/beamer.pdf");
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("skipping: examples/beamer.pdf has not been built");
        None
    }
}

/// Every overlay the deck declares, grouped as the application groups them.
fn discover() -> Option<OverlayIndex> {
    let path = deck()?;
    let mut backend = common::pdfium("PDFium media-deck tests")?;
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
    let directory = common::workspace_root().join("examples");

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
fn every_overlay_covers_a_contiguous_run_of_pages() {
    let Some(index) = discover() else { return };
    // The stripped example deck has no reveal sequence, so a multi-page
    // overlay is not guaranteed here; when one occurs its pages must still
    // form a single contiguous run that the index agrees with.
    for overlay in index.all() {
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
            let path = common::workspace_root().join("examples").join(&asset.path);
            assert!(path.is_file(), "{} is missing", path.display());
        }
        other => panic!("expected a file beside the document, got {other:?}"),
    }
}
