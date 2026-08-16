//! The reader's pure decisions: the viewport model, the zoom ladder and the
//! page-run arithmetic that continuous scroll needs.
//!
//! No Iced here. Everything below is the answer to "given a document, a cell
//! this size and a scroll position, what is on screen and where" — which is
//! the one substantial piece of new view code §8.7 calls out, and which is
//! worth being able to test without a window.

use pulpit_core::page::{PageGeometry, PageIndex};
use serde::{Deserialize, Serialize};

/// How the page is fitted into its cell.
///
/// A different viewport model from the presenter's fit-to-cell: a slide is
/// shown whole because that is how the audience sees it, and a page is read,
/// which means the width usually matters more than the height.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Zoom {
    /// The page.s width fills the cell. The reading default: a column of text
    /// set to the width of the window is what a page is for.
    #[default]
    FitWidth,
    /// The whole page is visible, however much space that leaves beside it.
    FitPage,
    /// A scale the user set, as a multiple of one PDF point per layout point.
    Fixed(f32),
}

/// The scales the zoom control steps through.
///
/// Geometric rather than linear, because a fixed step is a large change at the
/// bottom of the range and an imperceptible one at the top; each step here is
/// roughly a quarter more than the last near 100%, which is about the smallest
/// change worth a press.
pub const ZOOM_STEPS: [f32; 13] = [
    0.25, 0.33, 0.50, 0.67, 0.75, 1.00, 1.25, 1.50, 2.00, 3.00, 4.00, 6.00, 8.00,
];

pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 16.0;

impl Zoom {
    /// The scale this zoom resolves to for a page in a cell.
    ///
    /// `cell` is the space available in layout points, `page` the page's
    /// canonical size in PDF points.
    pub fn scale(self, page: &PageGeometry, cell: (f32, f32)) -> f32 {
        let (width, height) = cell;
        if width <= 0.0 || height <= 0.0 || !page.is_valid() {
            return 1.0;
        }
        let scale = match self {
            Zoom::FitWidth => width / page.width,
            Zoom::FitPage => (width / page.width).min(height / page.height),
            Zoom::Fixed(value) => value,
        };
        if scale.is_finite() {
            scale.clamp(MIN_ZOOM, MAX_ZOOM)
        } else {
            1.0
        }
    }

    /// The next scale up the ladder from `current`.
    pub fn zoomed_in(current: f32) -> Zoom {
        Zoom::Fixed(
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|step| *step > current + 1e-4)
                .unwrap_or(MAX_ZOOM),
        )
    }

    /// …and down.
    pub fn zoomed_out(current: f32) -> Zoom {
        Zoom::Fixed(
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|step| *step < current - 1e-4)
                .unwrap_or(MIN_ZOOM),
        )
    }

    /// "Fit width", "Fit page" or "150%".
    pub fn label(self, resolved: f32) -> String {
        match self {
            Zoom::FitWidth => "Fit width".to_string(),
            Zoom::FitPage => "Fit page".to_string(),
            Zoom::Fixed(_) => format!("{}%", (resolved * 100.0).round() as i32),
        }
    }

    /// Is this a fit rather than a scale the reader set?
    #[allow(dead_code)] // read by the zoom control once it offers a menu
    pub fn is_fit(self) -> bool {
        !matches!(self, Zoom::Fixed(_))
    }
}

/// The gap between pages in a continuous scroll, in layout points.
///
/// Wide enough that two pages read as two sheets rather than one long one,
/// which is what tells a reader they have crossed a page boundary when the
/// text runs straight on.
pub const PAGE_GAP: f32 = 12.0;

/// One page's place in the scrolled column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedPage {
    pub page: PageIndex,
    /// Distance from the top of the whole column to the top of this page, in
    /// layout points.
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl PlacedPage {
    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

/// Where every page sits in the scrolled column, and how tall the column is.
///
/// Computed from the pages' own sizes rather than from the first page's:
/// a document that mixes portrait body pages with a landscape appendix is
/// common, and a column laid out from page one alone puts the appendix in the
/// wrong place and takes the scroll position with it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Column {
    pub pages: Vec<PlacedPage>,
    pub height: f32,
    pub width: f32,
}

impl Column {
    /// Lay out `pages` at `scale` into a cell `cell_width` wide.
    pub fn lay_out(pages: &[PageGeometry], scale: f32, cell_width: f32) -> Column {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let mut placed = Vec::with_capacity(pages.len());
        let mut top = 0.0f32;
        let mut widest = 0.0f32;
        for (index, page) in pages.iter().enumerate() {
            let width = page.width * scale;
            let height = page.height * scale;
            placed.push(PlacedPage {
                page: PageIndex(index),
                top,
                width,
                height,
            });
            widest = widest.max(width);
            top += height + PAGE_GAP;
        }
        Column {
            height: (top - PAGE_GAP).max(0.0),
            width: widest.max(cell_width.max(0.0)),
            pages: placed,
        }
    }

    /// The pages that intersect the window `[offset, offset + viewport)`.
    ///
    /// This is what keeps a thousand-page document to a handful of rendered
    /// pages: everything off screen is not drawn, and nothing off screen is
    /// asked for.
    pub fn visible(&self, offset: f32, viewport: f32) -> Vec<PlacedPage> {
        let top = offset.max(0.0);
        let bottom = top + viewport.max(0.0);
        self.pages
            .iter()
            .copied()
            .filter(|placed| placed.bottom() >= top && placed.top <= bottom)
            .collect()
    }

    /// The page a reader would say they are on: the one covering the most of
    /// the visible window.
    ///
    /// Not "the first one visible": at most zoom levels two pages are on
    /// screen at once, and a counter that flipped the instant a sliver of the
    /// next page appeared would be wrong for most of the time it was showing.
    pub fn current(&self, offset: f32, viewport: f32) -> Option<PageIndex> {
        let top = offset.max(0.0);
        let bottom = top + viewport.max(0.0);
        self.pages
            .iter()
            .map(|placed| {
                let overlap = placed.bottom().min(bottom) - placed.top.max(top);
                (placed.page, overlap)
            })
            .filter(|(_, overlap)| *overlap > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(page, _)| page)
            // A viewport of no height sits between pages rather than on one;
            // the page whose top is nearest above is still the answer.
            .or_else(|| {
                self.pages
                    .iter()
                    .rev()
                    .find(|placed| placed.top <= top)
                    .map(|placed| placed.page)
            })
    }

    /// The scroll offset that puts `page` at the top of the window.
    pub fn offset_of(&self, page: PageIndex) -> Option<f32> {
        self.pages
            .iter()
            .find(|placed| placed.page == page)
            .map(|placed| placed.top)
    }

    /// Keep `offset` inside the column, given a window `viewport` tall.
    pub fn clamp_offset(&self, offset: f32, viewport: f32) -> f32 {
        if !offset.is_finite() {
            return 0.0;
        }
        // A document shorter than the window does not scroll at all, rather
        // than scrolling to a negative offset.
        let furthest = (self.height - viewport).max(0.0);
        offset.clamp(0.0, furthest)
    }
}

/// Which document controls the reader currently offers.
///
/// Application state rather than layout configuration: whether the eraser is
/// armed is not a property of where the toolbar was placed.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderControls {
    pub zoom: Zoom,
    /// Where the column is scrolled to, in layout points.
    pub offset: f32,
    /// The page the counter shows.
    pub page: PageIndex,
    /// The armed tool, or `None` when a press reaches the document's own links
    /// and form fields instead of laying down a mark.
    pub tool: Option<pulpit_core::annotation::AnnotationTool>,
    /// Whether the outline rail shows bookmarks or thumbnails.
    pub outline: OutlineView,
}

impl Default for ReaderControls {
    fn default() -> Self {
        Self {
            zoom: Zoom::default(),
            offset: 0.0,
            page: PageIndex(0),
            tool: None,
            outline: OutlineView::default(),
        }
    }
}

/// What the outline rail is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutlineView {
    /// The document's bookmark tree. The default where there is one: a title
    /// says more than a picture of a page of text.
    #[default]
    Bookmarks,
    /// Page thumbnails, which is the only answer for a document with no
    /// bookmarks and the better one for a document of figures.
    Thumbnails,
}

impl OutlineView {
    pub fn label(self) -> &'static str {
        match self {
            OutlineView::Bookmarks => "Bookmarks",
            OutlineView::Thumbnails => "Pages",
        }
    }

    pub fn other(self) -> OutlineView {
        match self {
            OutlineView::Bookmarks => OutlineView::Thumbnails,
            OutlineView::Thumbnails => OutlineView::Bookmarks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(count: usize) -> Vec<PageGeometry> {
        vec![PageGeometry::upright(612.0, 792.0); count]
    }

    #[test]
    fn fit_width_fills_the_cell_and_fit_page_shows_all_of_it() {
        let page = PageGeometry::upright(600.0, 800.0);
        let cell = (300.0, 300.0);
        assert!((Zoom::FitWidth.scale(&page, cell) - 0.5).abs() < 1e-6);
        // The height is the binding constraint for a portrait page in a square
        // cell, and fit-page has to honour it.
        assert!((Zoom::FitPage.scale(&page, cell) - 0.375).abs() < 1e-6);
        assert_eq!(Zoom::Fixed(2.0).scale(&page, cell), 2.0);
    }

    #[test]
    fn a_cell_with_no_room_and_a_page_with_no_size_still_produce_a_scale() {
        let page = PageGeometry::upright(600.0, 800.0);
        assert_eq!(Zoom::FitWidth.scale(&page, (0.0, 100.0)), 1.0);
        assert_eq!(
            Zoom::FitWidth.scale(&PageGeometry::upright(0.0, 0.0), (10.0, 10.0)),
            1.0
        );
        assert_eq!(Zoom::Fixed(f32::NAN).scale(&page, (10.0, 10.0)), 1.0);
    }

    #[test]
    fn scales_are_bounded_however_they_were_arrived_at() {
        let page = PageGeometry::upright(600.0, 800.0);
        assert_eq!(Zoom::Fixed(9_000.0).scale(&page, (10.0, 10.0)), MAX_ZOOM);
        assert_eq!(Zoom::Fixed(0.0001).scale(&page, (10.0, 10.0)), MIN_ZOOM);
    }

    #[test]
    fn the_zoom_ladder_steps_and_stops() {
        assert_eq!(Zoom::zoomed_in(1.0), Zoom::Fixed(1.25));
        assert_eq!(Zoom::zoomed_out(1.0), Zoom::Fixed(0.75));
        // A scale between two rungs steps to the next rung, not by a fixed
        // amount from where it was.
        assert_eq!(Zoom::zoomed_in(0.9), Zoom::Fixed(1.0));
        assert_eq!(Zoom::zoomed_out(0.9), Zoom::Fixed(0.75));
        assert_eq!(Zoom::zoomed_in(MAX_ZOOM), Zoom::Fixed(MAX_ZOOM));
        assert_eq!(Zoom::zoomed_out(0.0), Zoom::Fixed(MIN_ZOOM));
    }

    #[test]
    fn a_zoom_says_what_it_is_in_the_readers_own_words() {
        assert_eq!(Zoom::FitWidth.label(0.83), "Fit width");
        assert_eq!(Zoom::FitPage.label(0.4), "Fit page");
        assert_eq!(Zoom::Fixed(1.5).label(1.5), "150%");
        assert!(Zoom::FitWidth.is_fit());
        assert!(!Zoom::Fixed(1.0).is_fit());
    }

    #[test]
    fn pages_stack_with_a_gap_between_them() {
        let column = Column::lay_out(&letter(3), 1.0, 800.0);
        assert_eq!(column.pages.len(), 3);
        assert_eq!(column.pages[0].top, 0.0);
        assert_eq!(column.pages[1].top, 792.0 + PAGE_GAP);
        // The gap is *between* pages, so it is not on the end of the column.
        assert_eq!(column.height, 792.0 * 3.0 + PAGE_GAP * 2.0);
        // A cell wider than the page keeps its own width, so the mount around
        // the sheet is the cell's and the page is centred in it.
        assert_eq!(column.width, 800.0);
    }

    #[test]
    fn a_document_of_mixed_page_sizes_is_laid_out_from_each_pages_own() {
        let pages = vec![
            PageGeometry::upright(612.0, 792.0),
            PageGeometry::upright(792.0, 612.0),
            PageGeometry::upright(612.0, 792.0),
        ];
        let column = Column::lay_out(&pages, 1.0, 400.0);
        assert_eq!(column.pages[1].height, 612.0);
        assert_eq!(column.pages[2].top, 792.0 + 612.0 + PAGE_GAP * 2.0);
        assert_eq!(column.width, 792.0, "the widest page sets the column");
    }

    #[test]
    fn an_empty_document_lays_out_to_nothing_rather_than_to_a_negative_column() {
        let column = Column::lay_out(&[], 1.0, 500.0);
        assert_eq!(column.height, 0.0);
        assert!(column.pages.is_empty());
        assert_eq!(column.current(0.0, 100.0), None);
        assert_eq!(column.clamp_offset(50.0, 100.0), 0.0);
    }

    #[test]
    fn only_the_pages_in_the_window_are_visible() {
        let column = Column::lay_out(&letter(20), 1.0, 800.0);
        let visible = column.visible(0.0, 900.0);
        assert_eq!(visible.len(), 2, "a page and the top of the next");
        assert_eq!(visible[0].page, PageIndex(0));

        let far = column.visible(column.pages[10].top, 100.0);
        assert_eq!(far.len(), 1);
        assert_eq!(far[0].page, PageIndex(10));
    }

    #[test]
    fn the_page_counter_names_the_page_you_are_mostly_looking_at() {
        let column = Column::lay_out(&letter(5), 1.0, 800.0);
        // A sliver of page two at the bottom does not make it the page you are
        // on.
        assert_eq!(column.current(0.0, 800.0), Some(PageIndex(0)));
        // Past the midpoint of the crossing, it does.
        let crossing = column.pages[1].top - 100.0;
        assert_eq!(column.current(crossing, 800.0), Some(PageIndex(1)));
    }

    #[test]
    fn scrolling_to_a_page_puts_its_top_at_the_top() {
        let column = Column::lay_out(&letter(5), 0.5, 400.0);
        let offset = column.offset_of(PageIndex(3)).unwrap();
        assert_eq!(offset, column.pages[3].top);
        assert_eq!(column.current(offset, 400.0), Some(PageIndex(3)));
        assert_eq!(column.offset_of(PageIndex(9)), None);
    }

    #[test]
    fn the_scroll_offset_stays_inside_the_document() {
        let column = Column::lay_out(&letter(3), 1.0, 800.0);
        assert_eq!(column.clamp_offset(-500.0, 600.0), 0.0);
        assert_eq!(column.clamp_offset(f32::NAN, 600.0), 0.0);
        assert_eq!(column.clamp_offset(1e9, 600.0), column.height - 600.0);
        // A document shorter than the window does not scroll at all.
        let short = Column::lay_out(&letter(1), 0.1, 800.0);
        assert_eq!(short.clamp_offset(400.0, 2_000.0), 0.0);
    }

    #[test]
    fn a_nonsense_scale_lays_out_at_one_rather_than_collapsing_the_column() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let column = Column::lay_out(&letter(2), scale, 500.0);
            assert!(column.height > 0.0, "scale {scale} collapsed the column");
        }
    }

    #[test]
    fn the_outline_rail_has_two_views_and_toggles_between_them() {
        assert_eq!(OutlineView::default(), OutlineView::Bookmarks);
        assert_eq!(OutlineView::Bookmarks.other(), OutlineView::Thumbnails);
        assert_eq!(OutlineView::Thumbnails.other(), OutlineView::Bookmarks);
        assert_eq!(OutlineView::Thumbnails.label(), "Pages");
    }

    #[test]
    fn a_reader_opens_fitted_to_the_width_at_the_first_page_with_nothing_armed() {
        let controls = ReaderControls::default();
        assert_eq!(controls.zoom, Zoom::FitWidth);
        assert_eq!(controls.page, PageIndex(0));
        assert_eq!(controls.offset, 0.0);
        assert!(
            controls.tool.is_none(),
            "a document opens for reading, not for drawing on"
        );
    }
}
