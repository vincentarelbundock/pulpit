//! Deterministic presenter-notes mapping.
//!
//! A mapping is chosen by the user, supplied by a recognised
//! document-metadata contract, or read off page geometry in the one case where
//! the file states it outright — beamer's doubled `show notes on second
//! screen` page. It is always visible in the presenter UI, and no source below
//! the user ever overrides a mapping the user chose for that document.
//!
//! Nothing else is inferred. `Alternating` and `TwoRanges` produce pages
//! indistinguishable in shape from plain slides, so detecting them would mean
//! guessing at content rather than reading the file.

use crate::document::PageSize;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A fractional region of a PDF page, origin top-left, in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Region {
    pub const FULL: Region = Region {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn left_half() -> Self {
        Self::new(0.0, 0.0, 0.5, 1.0)
    }

    pub fn right_half() -> Self {
        Self::new(0.5, 0.0, 0.5, 1.0)
    }

    pub fn is_full(&self) -> bool {
        (self.x - 0.0).abs() < f32::EPSILON
            && (self.y - 0.0).abs() < f32::EPSILON
            && (self.width - 1.0).abs() < f32::EPSILON
            && (self.height - 1.0).abs() < f32::EPSILON
    }

    pub fn is_valid(&self) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && self.x >= 0.0
            && self.y >= 0.0
            && self.x + self.width <= 1.0 + 1e-4
            && self.y + self.height <= 1.0 + 1e-4
    }

    /// The overlap of two regions, if any.
    pub fn intersect(&self, other: &Region) -> Option<Region> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then(|| Region::new(x, y, right - x, bottom - y))
    }
}

/// How audience pages and notes pages are paired inside one PDF.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PairedRule {
    /// Pages alternate. `notes_first = false` means slide, notes, slide, ...
    Alternating { notes_first: bool },
    /// The document is two equal halves: slides then notes, or the reverse.
    TwoRanges { notes_first: bool },
}

/// The mapping from a logical slide index to PDF page regions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum NotesMapping {
    /// PDF page `N` is audience slide `N`; there are no notes.
    #[default]
    SlidesOnly,
    /// Configured regions of each PDF page carry the slide and the notes.
    SplitPage { slide: Region, notes: Region },
    /// A configured rule maps audience pages and notes pages.
    PairedPages(PairedRule),
}

/// Where to find one renderable image: a PDF page plus the region to crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSource {
    pub pdf_page: usize,
    pub region: Region,
}

impl PageSource {
    pub fn full(pdf_page: usize) -> Self {
        Self {
            pdf_page,
            region: Region::FULL,
        }
    }
}

impl NotesMapping {
    /// Number of logical slides produced from `pdf_pages` physical pages.
    pub fn slide_count(&self, pdf_pages: usize) -> usize {
        match self {
            NotesMapping::SlidesOnly | NotesMapping::SplitPage { .. } => pdf_pages,
            NotesMapping::PairedPages(PairedRule::Alternating { .. }) => pdf_pages.div_ceil(2),
            NotesMapping::PairedPages(PairedRule::TwoRanges { .. }) => pdf_pages.div_ceil(2),
        }
    }

    /// The audience image for logical slide `slide` (0-based).
    pub fn audience_source(&self, slide: usize, pdf_pages: usize) -> Option<PageSource> {
        if pdf_pages == 0 || slide >= self.slide_count(pdf_pages) {
            return None;
        }
        let source = match self {
            NotesMapping::SlidesOnly => PageSource::full(slide),
            NotesMapping::SplitPage { slide: region, .. } => PageSource {
                pdf_page: slide,
                region: *region,
            },
            NotesMapping::PairedPages(rule) => {
                PageSource::full(self.paired_page(*rule, slide, pdf_pages, false)?)
            }
        };
        (source.pdf_page < pdf_pages).then_some(source)
    }

    /// The notes image for logical slide `slide`, if the mapping has notes.
    pub fn notes_source(&self, slide: usize, pdf_pages: usize) -> Option<PageSource> {
        if pdf_pages == 0 || slide >= self.slide_count(pdf_pages) {
            return None;
        }
        let source = match self {
            NotesMapping::SlidesOnly => return None,
            NotesMapping::SplitPage { notes, .. } => PageSource {
                pdf_page: slide,
                region: *notes,
            },
            NotesMapping::PairedPages(rule) => {
                PageSource::full(self.paired_page(*rule, slide, pdf_pages, true)?)
            }
        };
        (source.pdf_page < pdf_pages).then_some(source)
    }

    fn paired_page(
        &self,
        rule: PairedRule,
        slide: usize,
        pdf_pages: usize,
        want_notes: bool,
    ) -> Option<usize> {
        match rule {
            PairedRule::Alternating { notes_first } => {
                let base = slide * 2;
                let offset = usize::from(want_notes != notes_first);
                Some(base + offset)
            }
            PairedRule::TwoRanges { notes_first } => {
                let half = pdf_pages.div_ceil(2);
                let first_block = if want_notes == notes_first { 0 } else { half };
                Some(first_block + slide)
            }
        }
    }

    pub fn has_notes(&self) -> bool {
        !matches!(self, NotesMapping::SlidesOnly)
    }

    pub fn is_valid(&self) -> bool {
        match self {
            NotesMapping::SlidesOnly | NotesMapping::PairedPages(_) => true,
            NotesMapping::SplitPage { slide, notes } => slide.is_valid() && notes.is_valid(),
        }
    }

    /// The same split with the two halves exchanged.
    ///
    /// The presenter's one-press correction when a deck used beamer's `left`
    /// rather than its default `right`. Other mappings have no halves to
    /// exchange and are returned unchanged.
    pub fn swapped(&self) -> NotesMapping {
        match self {
            NotesMapping::SplitPage { slide, notes } => NotesMapping::SplitPage {
                slide: *notes,
                notes: *slide,
            },
            other => other.clone(),
        }
    }

    /// Parse the Typst/Mosaic metadata contract.
    ///
    /// The contract lives in any PDF metadata string (Keywords or Subject) as
    /// a single directive:
    ///
    /// ```text
    /// pulpit:mapping=slides-only
    /// pulpit:mapping=split;slide=0,0,0.5,1;notes=0.5,0,0.5,1
    /// pulpit:mapping=alternating;notes-first=false
    /// pulpit:mapping=two-ranges;notes-first=true
    /// ```
    ///
    /// Unknown or malformed directives return `None` rather than a guess.
    pub fn from_metadata(metadata: &str) -> Option<NotesMapping> {
        let directive = metadata
            .split_whitespace()
            .find(|token| token.starts_with("pulpit:mapping="))?;
        let body = directive.trim_start_matches("pulpit:mapping=");
        let mut parts = body.split(';');
        let kind = parts.next()?;
        let mut fields: Vec<(&str, &str)> = Vec::new();
        for part in parts {
            let (k, v) = part.split_once('=')?;
            fields.push((k, v));
        }
        let field = |name: &str| fields.iter().find(|(k, _)| *k == name).map(|(_, v)| *v);
        let notes_first = match field("notes-first") {
            None => false,
            Some("true") => true,
            Some("false") => false,
            Some(_) => return None,
        };
        match kind {
            "slides-only" => Some(NotesMapping::SlidesOnly),
            "split" => {
                let slide = parse_region(field("slide")?)?;
                let notes = parse_region(field("notes")?)?;
                let mapping = NotesMapping::SplitPage { slide, notes };
                mapping.is_valid().then_some(mapping)
            }
            "alternating" => Some(NotesMapping::PairedPages(PairedRule::Alternating {
                notes_first,
            })),
            "two-ranges" => Some(NotesMapping::PairedPages(PairedRule::TwoRanges {
                notes_first,
            })),
            _ => None,
        }
    }
}

/// A page at least this wide, relative to its height, is a doubled page
/// rather than a slide.
///
/// beamer's `show notes on second screen` doubles the width: a 4:3 deck
/// becomes 8:3 (2.67) and a 16:9 deck becomes 32:9 (3.56). The widest ratio
/// anyone authors a plain slide at is 2:1, so the gap either side of this
/// threshold is large. Nothing is inferred from a ratio below it.
const DOUBLED_PAGE_RATIO: f32 = 2.2;

/// How far two pages' ratios may differ and still count as the same shape.
const RATIO_TOLERANCE: f32 = 0.02;

/// Recognise beamer's `show notes on second screen` from page geometry alone.
///
/// This is the one mapping a PDF genuinely announces: the doubled page is a
/// fact about the file, not a guess about the author. Which *half* carries the
/// notes is not in the file, so beamer's default — slide left, notes right —
/// is assumed, and the presenter can swap it. The vertical forms (`top`,
/// `bottom`) have no default to lean on and are deliberately not detected.
///
/// `sizes` may be a sample of the document rather than all of it; every page
/// in it must share one shape, so a deck that is doubled only in places is
/// left alone.
pub fn detect_split(sizes: &[PageSize]) -> Option<NotesMapping> {
    let first = sizes.first()?.aspect_ratio();
    if first < DOUBLED_PAGE_RATIO {
        return None;
    }
    let uniform = sizes
        .iter()
        .all(|size| (size.aspect_ratio() - first).abs() <= RATIO_TOLERANCE);
    uniform.then(|| NotesMapping::SplitPage {
        slide: Region::left_half(),
        notes: Region::right_half(),
    })
}

fn parse_region(value: &str) -> Option<Region> {
    let mut it = value.split(',').map(|n| n.trim().parse::<f32>());
    let x = it.next()?.ok()?;
    let y = it.next()?.ok()?;
    let w = it.next()?.ok()?;
    let h = it.next()?.ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Region::new(x, y, w, h))
}

impl fmt::Display for NotesMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotesMapping::SlidesOnly => write!(f, "Slides only"),
            NotesMapping::SplitPage { slide, notes } => write!(
                f,
                "Split page (slide {:.2},{:.2},{:.2},{:.2} / notes {:.2},{:.2},{:.2},{:.2})",
                slide.x,
                slide.y,
                slide.width,
                slide.height,
                notes.x,
                notes.y,
                notes.width,
                notes.height
            ),
            NotesMapping::PairedPages(PairedRule::Alternating { notes_first }) => {
                write!(f, "Paired pages, alternating ({})", order(*notes_first))
            }
            NotesMapping::PairedPages(PairedRule::TwoRanges { notes_first }) => {
                write!(f, "Paired pages, two ranges ({})", order(*notes_first))
            }
        }
    }
}

fn order(notes_first: bool) -> &'static str {
    if notes_first {
        "notes first"
    } else {
        "slides first"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_intersect_where_they_overlap() {
        let a = Region::new(0.0, 0.0, 0.5, 1.0);
        let b = Region::new(0.25, 0.25, 0.5, 0.5);
        assert_eq!(a.intersect(&b), Some(Region::new(0.25, 0.25, 0.25, 0.5)));
        assert_eq!(
            Region::FULL.intersect(&b),
            Some(b),
            "the full page constrains nothing"
        );
        let disjoint = Region::new(0.6, 0.0, 0.4, 1.0);
        assert_eq!(a.intersect(&disjoint), None);
    }

    #[test]
    fn slides_only_is_identity() {
        let m = NotesMapping::SlidesOnly;
        assert_eq!(m.slide_count(10), 10);
        assert_eq!(m.audience_source(3, 10), Some(PageSource::full(3)));
        assert_eq!(m.notes_source(3, 10), None);
        assert_eq!(m.audience_source(10, 10), None);
    }

    #[test]
    fn split_page_crops_the_same_page() {
        let m = NotesMapping::SplitPage {
            slide: Region::left_half(),
            notes: Region::right_half(),
        };
        assert_eq!(m.slide_count(4), 4);
        assert_eq!(
            m.audience_source(2, 4),
            Some(PageSource {
                pdf_page: 2,
                region: Region::left_half()
            })
        );
        assert_eq!(
            m.notes_source(2, 4),
            Some(PageSource {
                pdf_page: 2,
                region: Region::right_half()
            })
        );
    }

    #[test]
    fn alternating_pages() {
        let m = NotesMapping::PairedPages(PairedRule::Alternating { notes_first: false });
        assert_eq!(m.slide_count(8), 4);
        assert_eq!(m.audience_source(0, 8), Some(PageSource::full(0)));
        assert_eq!(m.notes_source(0, 8), Some(PageSource::full(1)));
        assert_eq!(m.audience_source(3, 8), Some(PageSource::full(6)));

        let m = NotesMapping::PairedPages(PairedRule::Alternating { notes_first: true });
        assert_eq!(m.audience_source(0, 8), Some(PageSource::full(1)));
        assert_eq!(m.notes_source(0, 8), Some(PageSource::full(0)));
    }

    #[test]
    fn alternating_with_odd_trailing_page_has_no_notes() {
        let m = NotesMapping::PairedPages(PairedRule::Alternating { notes_first: false });
        assert_eq!(m.slide_count(7), 4);
        assert_eq!(m.audience_source(3, 7), Some(PageSource::full(6)));
        assert_eq!(m.notes_source(3, 7), None, "page 7 does not exist");
    }

    #[test]
    fn two_ranges() {
        let m = NotesMapping::PairedPages(PairedRule::TwoRanges { notes_first: false });
        assert_eq!(m.slide_count(10), 5);
        assert_eq!(m.audience_source(1, 10), Some(PageSource::full(1)));
        assert_eq!(m.notes_source(1, 10), Some(PageSource::full(6)));
    }

    #[test]
    fn metadata_contract_round_trip() {
        assert_eq!(
            NotesMapping::from_metadata("built-with-mosaic pulpit:mapping=slides-only"),
            Some(NotesMapping::SlidesOnly)
        );
        assert_eq!(
            NotesMapping::from_metadata("pulpit:mapping=split;slide=0,0,0.5,1;notes=0.5,0,0.5,1"),
            Some(NotesMapping::SplitPage {
                slide: Region::left_half(),
                notes: Region::right_half()
            })
        );
        assert_eq!(
            NotesMapping::from_metadata("pulpit:mapping=alternating;notes-first=true"),
            Some(NotesMapping::PairedPages(PairedRule::Alternating {
                notes_first: true
            }))
        );
    }

    #[test]
    fn malformed_metadata_is_rejected_not_guessed() {
        assert_eq!(NotesMapping::from_metadata("pulpit:mapping=split"), None);
        assert_eq!(NotesMapping::from_metadata("pulpit:mapping=whatever"), None);
        assert_eq!(NotesMapping::from_metadata("notes on the right"), None);
        assert_eq!(
            NotesMapping::from_metadata("pulpit:mapping=split;slide=0,0,2,1;notes=0.5,0,0.5,1"),
            None,
            "out-of-range regions are invalid"
        );
    }
}

#[cfg(test)]
mod split_detection_tests {
    use super::*;

    fn page(width: f32, height: f32) -> PageSize {
        PageSize { width, height }
    }

    fn pages(width: f32, height: f32, count: usize) -> Vec<PageSize> {
        vec![page(width, height); count]
    }

    #[test]
    fn a_doubled_four_by_three_deck_is_a_split() {
        // beamer 4:3 with notes on the second screen: 256pt by 96pt.
        let detected = detect_split(&pages(256.0, 96.0, 8));
        assert_eq!(
            detected,
            Some(NotesMapping::SplitPage {
                slide: Region::left_half(),
                notes: Region::right_half(),
            }),
            "8:3 is a doubled 4:3 page"
        );
    }

    #[test]
    fn a_doubled_sixteen_by_nine_deck_is_a_split() {
        assert!(
            detect_split(&pages(728.0, 204.75, 4)).is_some(),
            "32:9 is a doubled 16:9 page"
        );
    }

    #[test]
    fn ordinary_slide_shapes_are_left_alone() {
        for (width, height, what) in [
            (720.0, 405.0, "16:9"),
            (720.0, 540.0, "4:3"),
            (720.0, 450.0, "16:10"),
            (720.0, 360.0, "2:1, the widest anyone authors"),
        ] {
            assert_eq!(
                detect_split(&pages(width, height, 6)),
                None,
                "{what} is a slide, not a doubled page"
            );
        }
    }

    #[test]
    fn a_deck_of_mixed_shapes_is_left_alone() {
        let mixed = vec![page(256.0, 96.0), page(256.0, 96.0), page(720.0, 405.0)];
        assert_eq!(
            detect_split(&mixed),
            None,
            "one odd page means the doubling is not the document's shape"
        );
    }

    #[test]
    fn an_empty_document_is_left_alone() {
        assert_eq!(detect_split(&[]), None);
    }

    #[test]
    fn rounding_within_tolerance_still_counts_as_one_shape() {
        let nearly = vec![page(256.0, 96.0), page(256.4, 96.0), page(255.7, 96.0)];
        assert!(
            detect_split(&nearly).is_some(),
            "producers round page boxes; a hair of drift is the same shape"
        );
    }

    #[test]
    fn swapping_a_split_exchanges_the_halves() {
        let detected = detect_split(&pages(256.0, 96.0, 2)).expect("doubled page");
        assert_eq!(
            detected.swapped(),
            NotesMapping::SplitPage {
                slide: Region::right_half(),
                notes: Region::left_half(),
            },
            "the beamer `left` correction"
        );
        assert_eq!(
            NotesMapping::SlidesOnly.swapped(),
            NotesMapping::SlidesOnly,
            "a mapping with no halves is unchanged"
        );
    }
}
