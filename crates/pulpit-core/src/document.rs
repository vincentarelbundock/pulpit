use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Identity of a *loaded* document instance.
///
/// A reload of the same path produces a new `DocumentId`: the identifier names
/// a concrete set of pages currently held by the renderer, not a file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DocumentId(pub u64);

impl DocumentId {
    pub const NONE: DocumentId = DocumentId(0);

    pub fn next(self) -> DocumentId {
        DocumentId(self.0 + 1)
    }
}

impl std::fmt::Display for DocumentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "doc#{}", self.0)
    }
}

/// Size of a PDF page in PDF points (1/72 inch).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    pub fn aspect_ratio(&self) -> f32 {
        if self.height <= 0.0 {
            1.0
        } else {
            self.width / self.height
        }
    }
}

/// Where a link annotation points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// Another physical PDF page of the same document (zero-based).
    Page {
        page: usize,
        /// A `/FitR` destination view (beamer `\framezoom`): show only this
        /// normalised region of the destination page. Other view kinds
        /// (`/XYZ`, `/FitH`, …) carry no zoom and fall back to plain page
        /// navigation.
        zoom: Option<crate::notes::Region>,
    },
    /// An external URI, opened with the desktop's default handler.
    Uri(String),
}

/// One clickable link annotation on a PDF page.
///
/// The rectangle is normalised to the page with a top-left origin, the same
/// convention as [`crate::notes::Region`], so hit-testing needs no knowledge
/// of PDF point coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageLink {
    /// Normalised bounds on the page: x, y, width, height in `0.0..=1.0`.
    pub rect: crate::notes::Region,
    pub target: LinkTarget,
}

impl PageLink {
    /// Does a normalised page point fall inside this link?
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.rect.x
            && x <= self.rect.x + self.rect.width
            && y >= self.rect.y
            && y <= self.rect.y + self.rect.height
    }
}

/// Which link a page point is over, if any.
///
/// The *last* match wins: PDF producers draw later annotations over earlier
/// ones, so when two overlap the one a click would reach is the later one.
pub fn link_at(links: &[PageLink], x: f32, y: f32) -> Option<usize> {
    links.iter().rposition(|link| link.contains(x, y))
}

/// Move keyboard focus through a page's links.
///
/// Focus order is the order the producer wrote the annotations, which for
/// beamer and Typst is reading order. Stepping past either end wraps, and a
/// page with no links never takes focus at all — pressing the key on such a
/// page must do nothing rather than trap the presenter in an empty cycle.
pub fn step_link_focus(current: Option<usize>, count: usize, forward: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => 0,
        (None, false) => count - 1,
        (Some(index), true) => (index + 1) % count,
        (Some(index), false) => (index + count - 1) % count,
    })
}

/// How many per-page sizes are worth carrying for one document.
///
/// A page size is eight bytes, so a 50 000-page PDF would put 400 kB of
/// geometry into a single IPC message that exists only to answer "what shape
/// is this page". Decks that large are scanned archives, not talks, and their
/// pages are overwhelmingly uniform. So the first [`MAX_TRACKED_PAGE_SIZES`]
/// pages are measured exactly and the rest fall back to the first page, with
/// [`DocumentInfo::page_sizes_sampled`] saying out loud that this happened.
pub const MAX_TRACKED_PAGE_SIZES: usize = 4096;

/// Everything the domain needs to know about the open document.
///
/// Deliberately holds no backend handle so the state survives a reload.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentInfo {
    pub id: DocumentId,
    pub path: PathBuf,
    /// Number of physical PDF pages.
    pub pdf_pages: usize,
    /// Size of the first page; used for aspect fit before any render lands.
    pub first_page_size: Option<PageSize>,
    /// Measured size of pages `0..page_sizes.len()`. A deck whose pages differ
    /// — a 4:3 appendix glued onto a 16:9 talk — is common enough that the
    /// first page cannot be trusted to describe the rest.
    pub page_sizes: Vec<PageSize>,
    /// True when [`page_sizes`](Self::page_sizes) covers only a prefix of the
    /// document because it exceeded [`MAX_TRACKED_PAGE_SIZES`]. Pages beyond
    /// the prefix answer with the first page's size.
    pub page_sizes_sampled: bool,
    /// Speaker notes the document carries in an embedded `*.pdfpc` file.
    ///
    /// A property of the document, not of the presentation: it survives a
    /// change of mapping, and it is dropped when the document is.
    pub text_notes: Option<crate::pdfpc::TextNotes>,
}

impl DocumentInfo {
    pub fn new(id: DocumentId, path: impl AsRef<Path>, pdf_pages: usize) -> Self {
        Self {
            id,
            path: path.as_ref().to_path_buf(),
            pdf_pages,
            first_page_size: None,
            page_sizes: Vec::new(),
            page_sizes_sampled: false,
            text_notes: None,
        }
    }

    pub fn with_text_notes(mut self, notes: Option<crate::pdfpc::TextNotes>) -> Self {
        self.text_notes = notes;
        self
    }

    pub fn with_first_page_size(mut self, size: PageSize) -> Self {
        self.first_page_size = Some(size);
        self
    }

    /// Record measured page sizes. The first of them is also the first page
    /// size, so the two views of the geometry can never disagree.
    pub fn with_page_sizes(mut self, sizes: Vec<PageSize>, sampled: bool) -> Self {
        if let Some(first) = sizes.first().copied() {
            self.first_page_size = Some(first);
        }
        self.page_sizes = sizes;
        self.page_sizes_sampled = sampled;
        self
    }

    /// Size of one page, falling back to the first page for any page that was
    /// never measured. Aspect fit must always have an answer: an unmeasured
    /// page is a reason to guess the common case, not to draw nothing.
    pub fn page_size(&self, page: usize) -> Option<PageSize> {
        self.page_sizes
            .get(page)
            .copied()
            .or(self.first_page_size)
            .or_else(|| self.page_sizes.first().copied())
    }

    /// Aspect ratio of one page, with the same fallback as
    /// [`page_size`](Self::page_size).
    pub fn aspect_ratio(&self, page: usize) -> Option<f32> {
        self.page_size(page).map(|size| size.aspect_ratio())
    }

    /// Do the measured pages disagree about their shape?
    ///
    /// Compared by aspect ratio rather than by points: a deck that mixes
    /// letter and A4 at the same ratio letterboxes identically, and warning
    /// about it would be noise. Sampling can only hide a difference, never
    /// invent one, so a `true` answer is always real.
    pub fn has_mixed_page_sizes(&self) -> bool {
        let mut sizes = self.page_sizes.iter();
        let Some(first) = sizes.next() else {
            return false;
        };
        let reference = first.aspect_ratio();
        sizes.any(|size| (size.aspect_ratio() - reference).abs() > ASPECT_TOLERANCE)
    }
}

/// Aspect-ratio difference below which two pages are the same shape. One part
/// in a thousand is far finer than any layout consequence and far coarser than
/// the rounding in a PDF's `/MediaBox`.
const ASPECT_TOLERANCE: f32 = 1e-3;

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: f32, height: f32) -> PageSize {
        PageSize { width, height }
    }

    fn document(sizes: Vec<PageSize>) -> DocumentInfo {
        let pages = sizes.len();
        DocumentInfo::new(DocumentId(1), "/decks/talk.pdf", pages).with_page_sizes(sizes, false)
    }

    #[test]
    fn page_sizes_set_the_first_page_size_too() {
        let info = document(vec![size(960.0, 540.0), size(720.0, 540.0)]);
        assert_eq!(info.first_page_size, Some(size(960.0, 540.0)));
    }

    #[test]
    fn an_unmeasured_page_falls_back_to_the_first_page() {
        let info = document(vec![size(960.0, 540.0), size(720.0, 540.0)]);
        assert_eq!(info.page_size(1), Some(size(720.0, 540.0)));
        assert_eq!(
            info.page_size(9),
            Some(size(960.0, 540.0)),
            "a page beyond the sample answers with the first page"
        );
        assert_eq!(info.aspect_ratio(1), Some(720.0 / 540.0));
    }

    #[test]
    fn a_document_without_any_measurement_has_no_page_size() {
        let info = DocumentInfo::new(DocumentId(1), "/decks/talk.pdf", 10);
        assert_eq!(info.page_size(0), None);
        assert_eq!(info.aspect_ratio(0), None);
        assert!(!info.has_mixed_page_sizes());
    }

    #[test]
    fn only_the_first_page_size_still_answers_for_every_page() {
        let info = DocumentInfo::new(DocumentId(1), "/decks/talk.pdf", 10)
            .with_first_page_size(size(960.0, 540.0));
        assert_eq!(info.page_size(7), Some(size(960.0, 540.0)));
    }

    #[test]
    fn mixed_page_sizes_are_detected_by_shape_not_by_points() {
        assert!(!document(vec![size(960.0, 540.0), size(960.0, 540.0)]).has_mixed_page_sizes());
        assert!(
            !document(vec![size(960.0, 540.0), size(1920.0, 1080.0)]).has_mixed_page_sizes(),
            "the same shape at a different scale letterboxes identically"
        );
        assert!(document(vec![size(960.0, 540.0), size(720.0, 540.0)]).has_mixed_page_sizes());
    }

    #[test]
    fn a_single_page_deck_is_never_mixed() {
        assert!(!document(vec![size(960.0, 540.0)]).has_mixed_page_sizes());
    }

    fn link(x: f32, y: f32, width: f32, height: f32) -> PageLink {
        PageLink {
            rect: crate::notes::Region::new(x, y, width, height),
            target: LinkTarget::Uri("https://example.com".into()),
        }
    }

    #[test]
    fn a_point_finds_the_link_it_is_inside() {
        let links = [link(0.1, 0.1, 0.2, 0.2), link(0.5, 0.5, 0.2, 0.2)];
        assert_eq!(link_at(&links, 0.15, 0.15), Some(0));
        assert_eq!(link_at(&links, 0.55, 0.55), Some(1));
        assert_eq!(link_at(&links, 0.9, 0.9), None);
    }

    #[test]
    fn overlapping_links_resolve_to_the_one_drawn_last() {
        // Whichever a click would reach is the one the highlight must show,
        // or the affordance would point at the wrong rectangle.
        let links = [link(0.1, 0.1, 0.5, 0.5), link(0.2, 0.2, 0.1, 0.1)];
        assert_eq!(link_at(&links, 0.25, 0.25), Some(1));
    }

    #[test]
    fn keyboard_focus_starts_at_either_end_depending_on_direction() {
        assert_eq!(step_link_focus(None, 3, true), Some(0));
        assert_eq!(step_link_focus(None, 3, false), Some(2));
    }

    #[test]
    fn keyboard_focus_wraps_at_both_ends() {
        assert_eq!(step_link_focus(Some(2), 3, true), Some(0));
        assert_eq!(step_link_focus(Some(0), 3, false), Some(2));
        assert_eq!(step_link_focus(Some(0), 3, true), Some(1));
        assert_eq!(step_link_focus(Some(1), 3, false), Some(0));
    }

    #[test]
    fn a_page_without_links_never_takes_focus() {
        // Otherwise the key would appear broken *and* leave the presenter in
        // a focus cycle with nothing in it.
        assert_eq!(step_link_focus(None, 0, true), None);
        assert_eq!(step_link_focus(Some(0), 0, true), None);
    }
}
