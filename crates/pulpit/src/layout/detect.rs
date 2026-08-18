//! What a file looks like it is for, read off its first page.
//!
//! A deck and a report are the same file format, so the only honest signal at
//! open is the shape of the page. Slide software emits a small, fixed set of
//! page sizes; paper emits a different, equally fixed set. This reads the
//! first page and says which set it fell in, so a talk opens into the
//! presenter screen and a paper opens into the Reader without either being
//! asked for.
//!
//! It is a guess, and it is treated as one: a layout the user chose for this
//! file wins over anything here, and switching mode by hand is always one
//! press away. Pure, so every case below is an ordinary unit test.

use crate::layout::PrimaryViewer;

/// How close two dimensions must be, in points, to be the same page size.
///
/// Three points is a fortieth of an inch. Generators round millimetres to
/// points differently — 210mm is 595.276pt, written as 595 or 596 depending on
/// the tool — and no two distinct sizes in the tables below are within three
/// points of each other, so the tolerance separates rounding from meaning.
const SIZE_TOLERANCE_PT: f32 = 3.0;

/// How close two aspect ratios must be to count as the same shape.
///
/// A percent, which is wide enough for a generator that rounded its page to
/// whole points (a 960x540 slide rounded to 959x540 is off by a thousandth)
/// and narrow enough that 4:3 (1.3333) and the nearest thing to it in the
/// table, 3:2 (1.5), stay far apart.
const RATIO_TOLERANCE: f32 = 0.01;

/// Paper, in points, portrait. A page this size is a document whatever its
/// ratio works out to, which is the point of checking size before shape:
/// US Letter landscape is 1.294:1, close enough to 4:3 that a shape-only test
/// would open every landscape handout as a presentation.
const PAPER_SIZES_PT: [(f32, f32); 8] = [
    (595.0, 842.0),  // A4
    (842.0, 1191.0), // A3
    (420.0, 595.0),  // A5
    (499.0, 709.0),  // B5 (ISO)
    (516.0, 729.0),  // B5 (JIS)
    (612.0, 792.0),  // US Letter
    (612.0, 1008.0), // US Legal
    (792.0, 1224.0), // US Tabloid
];

/// The shapes slide software produces, widest dimension over the other.
///
/// Beamer's two presets are 4:3 (128x96mm) and 16:9 (160x90mm); PowerPoint
/// and Keynote add 16:10 and their older 4:3; 5:4 and 3:2 are what the long
/// tail of templates and older projectors use.
const SLIDE_RATIOS: [f32; 5] = [4.0 / 3.0, 16.0 / 9.0, 16.0 / 10.0, 5.0 / 4.0, 3.0 / 2.0];

/// Which layout a document of this page shape should open into.
///
/// `width` and `height` are the first page's displayed size in points, after
/// rotation — [`pulpit_core::page::PageGeometry`]'s own `width` and `height`,
/// which is what a reader would measure with a ruler.
///
/// The order is what makes it work:
///
/// 1. A known paper size, in either orientation, is a document. Size beats
///    shape here because several papers are within a hair of a slide ratio.
/// 2. A known slide ratio is a presentation.
/// 3. Twice a known slide ratio is a presentation too: that is a beamer deck
///    built with `\setbeameroption{show notes on second screen}`, which puts
///    the slide and its notes side by side on one double-wide page.
/// 4. Anything else is a document. The Reader shows a deck perfectly well,
///    whereas the presenter screen shows a report as a slide that does not
///    fit, so the fallback goes the way that degrades gracefully.
pub fn viewer_for_page(width: f32, height: f32) -> PrimaryViewer {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return PrimaryViewer::Document;
    }
    if is_paper(width, height) {
        return PrimaryViewer::Document;
    }
    // Portrait pages are not slides in any of the shapes below, and the ratio
    // table is written landscape, so measure the long side over the short one
    // only for a page that is actually wider than it is tall.
    if height > width {
        return PrimaryViewer::Document;
    }
    let ratio = width / height;
    let slide = SLIDE_RATIOS
        .iter()
        .any(|known| close_ratio(ratio, *known) || close_ratio(ratio, known * 2.0));
    if slide {
        PrimaryViewer::Slide
    } else {
        PrimaryViewer::Document
    }
}

/// Which layout a whole document should open into.
///
/// The first page decides. A deck's pages are all one size, and a report whose
/// first page is a cover is still a report; averaging over a document that
/// mixes sizes would answer for a page that is not in it. `None` — a document
/// that measured no pages — is a document, on the same "degrades gracefully"
/// grounds as the fallback above.
pub fn viewer_for_document(pages: &[pulpit_core::page::PageGeometry]) -> PrimaryViewer {
    match pages.first() {
        Some(first) => viewer_for_page(first.width, first.height),
        None => PrimaryViewer::Document,
    }
}

/// Is this page a sheet of paper, in either orientation?
fn is_paper(width: f32, height: f32) -> bool {
    let short = width.min(height);
    let long = width.max(height);
    PAPER_SIZES_PT.iter().any(|(paper_short, paper_long)| {
        (short - paper_short).abs() <= SIZE_TOLERANCE_PT
            && (long - paper_long).abs() <= SIZE_TOLERANCE_PT
    })
}

fn close_ratio(ratio: f32, known: f32) -> bool {
    (ratio - known).abs() <= known * RATIO_TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Millimetres to points, which is how slide geometry is usually written.
    fn mm(value: f32) -> f32 {
        value * 72.0 / 25.4
    }

    #[test]
    fn the_shapes_slide_software_actually_emits_open_as_a_presentation() {
        for (name, width, height) in [
            ("beamer 4:3", mm(128.0), mm(96.0)),
            ("beamer 16:9", mm(160.0), mm(90.0)),
            ("powerpoint 16:9", 960.0, 540.0),
            ("powerpoint 4:3", 720.0, 540.0),
            ("keynote 4:3", 1024.0, 768.0),
            ("keynote 16:9", 1920.0, 1080.0),
            ("16:10", 1440.0, 900.0),
            ("5:4 projector", 1280.0, 1024.0),
            ("3:2", 1080.0, 720.0),
        ] {
            assert_eq!(
                viewer_for_page(width, height),
                PrimaryViewer::Slide,
                "{name} should present"
            );
        }
    }

    /// A beamer deck built with `show notes on second screen` is one page per
    /// slide, twice as wide, with the notes in the right half. It is still a
    /// deck, and the presenter screen is still where it belongs.
    #[test]
    fn a_double_wide_deck_with_notes_beside_the_slide_is_still_a_deck() {
        for (name, width, height) in [
            ("beamer 4:3 with notes", mm(256.0), mm(96.0)),
            ("beamer 16:9 with notes", mm(320.0), mm(90.0)),
            ("powerpoint 16:9 with notes", 1920.0, 540.0),
        ] {
            assert_eq!(
                viewer_for_page(width, height),
                PrimaryViewer::Slide,
                "{name} should present"
            );
        }
    }

    /// The case size-before-shape exists for: Letter landscape is 1.294:1,
    /// within 3% of 4:3, and a ratio-only test opens every handout as a talk.
    #[test]
    fn paper_is_a_document_in_either_orientation_however_its_ratio_reads() {
        for (name, width, height) in [
            ("A4 portrait", 595.0, 842.0),
            ("A4 landscape", 842.0, 595.0),
            ("Letter portrait", 612.0, 792.0),
            ("Letter landscape", 792.0, 612.0),
            ("Legal", 612.0, 1008.0),
            ("A3 landscape", 1191.0, 842.0),
            ("A5", 420.0, 595.0),
            ("Tabloid", 792.0, 1224.0),
        ] {
            assert_eq!(
                viewer_for_page(width, height),
                PrimaryViewer::Document,
                "{name} should read"
            );
        }
    }

    /// A generator that rounded 210mm to 596pt rather than 595 wrote the same
    /// sheet of paper, and a slide rounded to whole points is the same slide.
    #[test]
    fn a_point_of_rounding_does_not_change_what_a_page_is() {
        assert_eq!(viewer_for_page(596.0, 843.0), PrimaryViewer::Document);
        assert_eq!(viewer_for_page(594.0, 841.0), PrimaryViewer::Document);
        assert_eq!(viewer_for_page(959.0, 540.0), PrimaryViewer::Slide);
    }

    #[test]
    fn a_portrait_page_is_never_a_slide() {
        // 3:4 is 4:3 stood on end. Slides are not published portrait, and a
        // portrait page is the one shape the Reader is unambiguously for.
        assert_eq!(viewer_for_page(540.0, 720.0), PrimaryViewer::Document);
        assert_eq!(viewer_for_page(300.0, 900.0), PrimaryViewer::Document);
    }

    #[test]
    fn an_unrecognised_shape_falls_back_to_the_reader() {
        // A square, and a banner: neither is a slide format, and the Reader
        // shows both without pretending they are.
        assert_eq!(viewer_for_page(600.0, 600.0), PrimaryViewer::Document);
        assert_eq!(viewer_for_page(2000.0, 300.0), PrimaryViewer::Document);
    }

    /// A page that measured to nothing is not a reason to guess, and it is
    /// certainly not a reason to divide by zero.
    #[test]
    fn a_degenerate_page_is_a_document_rather_than_a_panic() {
        assert_eq!(viewer_for_page(0.0, 0.0), PrimaryViewer::Document);
        assert_eq!(viewer_for_page(-100.0, 200.0), PrimaryViewer::Document);
        assert_eq!(viewer_for_page(f32::NAN, 500.0), PrimaryViewer::Document);
        assert_eq!(
            viewer_for_page(f32::INFINITY, 500.0),
            PrimaryViewer::Document
        );
    }

    #[test]
    fn the_first_page_decides_and_an_unmeasured_document_reads() {
        use pulpit_core::page::{PageGeometry, PageRotation};

        let page = |width: f32, height: f32| PageGeometry {
            width,
            height,
            crop_left: 0.0,
            crop_bottom: 0.0,
            crop_width: width,
            crop_height: height,
            rotation: PageRotation::None,
            user_unit: 1.0,
        };

        assert_eq!(viewer_for_document(&[]), PrimaryViewer::Document);
        // A deck whose later pages are a different size is still a deck.
        assert_eq!(
            viewer_for_document(&[page(960.0, 540.0), page(595.0, 842.0)]),
            PrimaryViewer::Slide
        );
        // …and a report with a wide foldout in it is still a report.
        assert_eq!(
            viewer_for_document(&[page(595.0, 842.0), page(960.0, 540.0)]),
            PrimaryViewer::Document
        );
    }
}
