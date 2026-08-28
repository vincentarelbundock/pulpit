//! The reader's pure decisions: the viewport model, the zoom ladder and the
//! page-run arithmetic that continuous scroll needs.
//!
//! No Iced here. Everything below is the answer to "given a document, a cell
//! this size and a scroll position, what is on screen and where" — which is
//! the one substantial piece of new view code §8.7 calls out, and which is
//! worth being able to test without a window.

use pulpit_core::notes::Region;
use pulpit_core::page::{PageGeometry, PageIndex, PageRotation};
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
    /// The page.s height fills the cell, however wide that leaves it.
    ///
    /// The fit for reading a page at a time rather than a column of text:
    /// one whole page in the window, and the next press of the wheel is the
    /// next page rather than the rest of this one.
    FitHeight,
    /// A scale the user set, as a multiple of one PDF point per layout point.
    Fixed(f32),
}

/// The scales the zoom control steps through.
///
/// Geometric rather than linear, because a fixed step is a large change at the
/// bottom of the range and an imperceptible one at the top; each step here is
/// roughly a quarter more than the last near 100%, which is about the smallest
/// change worth a press.
pub const ZOOM_STEPS: [f32; 15] = [
    0.10, 0.15, 0.25, 0.33, 0.50, 0.67, 0.75, 1.00, 1.25, 1.50, 2.00, 3.00, 4.00, 6.00, 8.00,
];

/// The smallest scale the reader can reach.
///
/// Ten per cent: a page smaller than that is a stamp with no readable text on
/// it, so the rungs below are range the control spends presses on without
/// showing the reader anything.
pub const MIN_ZOOM: f32 = 0.10;
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
            Zoom::FitHeight => height / page.height,
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
    ///
    /// The band shows the resolved percentage rather than these names — the
    /// icons say which fit is on — so this is what a zoom *menu* would put
    /// beside each choice.
    #[allow(dead_code)]
    pub fn label(self, resolved: f32) -> String {
        match self {
            Zoom::FitWidth => "Fit width".to_string(),
            Zoom::FitPage => "Fit page".to_string(),
            Zoom::FitHeight => "Fit height".to_string(),
            Zoom::Fixed(_) => format!("{}%", (resolved * 100.0).round() as i32),
        }
    }

    /// Is this a fit rather than a scale the reader set?
    #[allow(dead_code)] // read by the zoom control once it offers a menu
    pub fn is_fit(self) -> bool {
        !matches!(self, Zoom::Fixed(_))
    }
}

/// The marquee crop: a rectangle drawn on the page, and what it was taken to
/// mean.
///
/// A zoom control and never a document edit: nothing here sets the dirty flag,
/// enters the undo stack or reaches `/CropBox`. It changes what is on screen
/// and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CropState {
    /// No marquee, no crop: presses reach the page as usual.
    #[default]
    Off,
    /// Armed and waiting for a drag. Nothing is drawn yet.
    Armed,
    /// A rectangle has been drawn and the reader is choosing what it means.
    Choosing(Region),
    /// A crop is in force: every page is read through this window.
    Cropped(Region),
}

impl CropState {
    /// The window every page is read through, which is the whole page unless
    /// a crop is in force. Deliberately not the rectangle being *chosen*: a
    /// rectangle is still a proposal until the reader says what it means.
    pub fn window(self) -> Region {
        match self {
            CropState::Cropped(region) => region,
            _ => Region::FULL,
        }
    }

    /// Does the pointer belong to the marquee rather than to the page?
    pub fn takes_the_pointer(self) -> bool {
        matches!(self, CropState::Armed | CropState::Choosing(_))
    }

    /// Is the control lit — armed, mid-choice, or holding a crop?
    pub fn is_on(self) -> bool {
        !matches!(self, CropState::Off)
    }

    /// What the button's tooltip says. A latch that clears something should
    /// say so before it is pressed, not after.
    pub fn label(self) -> &'static str {
        match self {
            CropState::Cropped(_) => "Clear crop",
            _ => "Crop",
        }
    }
}

/// What a drawn rectangle was taken to mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropChoice {
    /// Fill the window with it, once. A zoom and nothing more.
    Zoom,
    /// Read every page through it until the crop is cleared.
    Pages,
}

/// The smallest crop worth honouring, as a fraction of the page.
///
/// A rectangle thinner than this is a slip of the hand rather than a reading
/// decision, and a crop made of one would scale the page past anything the
/// zoom ladder can reach.
pub const MIN_CROP_FRACTION: f32 = 0.02;

/// Is this region a rectangle a page can actually be read through?
pub fn is_usable_crop(region: &Region) -> bool {
    region.x.is_finite()
        && region.y.is_finite()
        && region.width >= MIN_CROP_FRACTION
        && region.height >= MIN_CROP_FRACTION
        && region.x >= -f32::EPSILON
        && region.y >= -f32::EPSILON
        && region.x + region.width <= 1.0 + 1e-3
        && region.y + region.height <= 1.0 + 1e-3
}

/// The rectangle two corners describe, in fractions of the page, whichever
/// way round it was drawn and however far outside the sheet the hand went.
pub fn crop_between(start: (f32, f32), end: (f32, f32), page: &PageGeometry) -> Region {
    let fraction = |value: f32, extent: f32| {
        if extent > 0.0 {
            (value / extent).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let (x0, x1) = (fraction(start.0, page.width), fraction(end.0, page.width));
    let (y0, y1) = (fraction(start.1, page.height), fraction(end.1, page.height));
    Region::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
}

/// The page as it is read through `window`.
///
/// Fractions rather than points is what makes one crop right for a document
/// of mixed page sizes: the same window trims a letter page and the A4
/// appendix behind it in proportion, where a rectangle in points would be
/// wrong on one of them.
pub fn cropped(page: &PageGeometry, window: Region) -> PageGeometry {
    if window == Region::FULL || !is_usable_crop(&window) {
        return *page;
    }
    PageGeometry {
        width: page.width * window.width,
        height: page.height * window.height,
        ..*page
    }
}

/// The page as the reader has turned it: the same sheet with its displayed
/// axes swapped when the view rotation calls for it.
///
/// A view transform in the same family as [`cropped`]: it changes what is on
/// screen and nothing else. The geometry's own `rotation` and crop fields are
/// left untouched — they describe the document, this describes the reader's
/// window onto it — so a consumer that converts to user space must not be
/// handed this; only the layout, which reads width and height, ever sees it.
pub fn view_rotated(page: &PageGeometry, rotation: PageRotation) -> PageGeometry {
    if !rotation.swaps_axes() {
        return *page;
    }
    PageGeometry {
        width: page.height,
        height: page.width,
        ..*page
    }
}

/// Where a fraction-of-the-page window sits once the page is turned.
///
/// What lets the marquee crop and the view rotation coexist: the crop is kept
/// in the sheet's own upright fractions, and this is the one place it is
/// turned to match what the reader is looking at.
pub fn rotated_region(region: Region, rotation: PageRotation) -> Region {
    match rotation {
        PageRotation::None => region,
        PageRotation::Clockwise90 => Region::new(
            1.0 - region.y - region.height,
            region.x,
            region.height,
            region.width,
        ),
        PageRotation::Clockwise180 => Region::new(
            1.0 - region.x - region.width,
            1.0 - region.y - region.height,
            region.width,
            region.height,
        ),
        PageRotation::Clockwise270 => Region::new(
            region.y,
            1.0 - region.x - region.width,
            region.height,
            region.width,
        ),
    }
}

/// The gap between pages in a continuous scroll, in layout points.
///
/// Wide enough that two pages read as two sheets rather than one long one,
/// which is what tells a reader they have crossed a page boundary when the
/// text runs straight on.
pub const PAGE_GAP: f32 = 12.0;

/// How many pages stand side by side in the column.
///
/// A reading decision rather than a zoom: a scanned book is two facing pages
/// whatever scale it is read at, and a report is one however wide the window
/// is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageSpread {
    /// One page across the column: the reading default.
    #[default]
    Single,
    /// Two pages side by side, paired from the first: 1–2, 3–4, and so on.
    ///
    /// Paired from the first rather than offset by a cover page, because
    /// pulpit is shown documents that were exported, not bound, and a rule a
    /// reader can predict beats one that is right for half of them.
    Double,
}

impl PageSpread {
    pub fn label(self) -> &'static str {
        match self {
            PageSpread::Single => "Single page",
            PageSpread::Double => "Two pages",
        }
    }

    pub fn other(self) -> PageSpread {
        match self {
            PageSpread::Single => PageSpread::Double,
            PageSpread::Double => PageSpread::Single,
        }
    }

    /// How many pages stand in one row.
    fn across(self) -> usize {
        match self {
            PageSpread::Single => 1,
            PageSpread::Double => 2,
        }
    }
}

/// One page's place in the scrolled column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedPage {
    pub page: PageIndex,
    /// Distance from the top of the whole column to the top of this page, in
    /// layout points.
    pub top: f32,
    /// …and from its left edge. Pages are centred in the column, so this is
    /// not zero even in a single-page spread, and in a two-page one it is
    /// what tells the two sheets of a row apart.
    pub left: f32,
    pub width: f32,
    pub height: f32,
    /// The bottom of the *row* this page stands in, which in a single-page
    /// spread is its own.
    ///
    /// Carried because two facing pages need not be the same height, so the
    /// pages' own bottoms are not in order down the column and cannot be
    /// binary-searched; the rows' are.
    pub row_bottom: f32,
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
    pub fn lay_out(
        pages: &[PageGeometry],
        scale: f32,
        cell_width: f32,
        spread: PageSpread,
    ) -> Column {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let across = spread.across();
        let mut placed = Vec::with_capacity(pages.len());
        let mut rows: Vec<(usize, usize, f32)> = Vec::new();
        let mut top = 0.0f32;
        let mut widest = 0.0f32;
        for (row, chunk) in pages.chunks(across).enumerate() {
            let first = placed.len();
            // A row is as tall as its tallest page and as wide as its pages
            // plus the gaps between them: a landscape page beside a portrait
            // one must not have the next row start over its foot.
            let height = chunk
                .iter()
                .map(|page| page.height * scale)
                .fold(0.0f32, f32::max);
            let mut row_width = 0.0f32;
            for (offset, page) in chunk.iter().enumerate() {
                let width = page.width * scale;
                placed.push(PlacedPage {
                    page: PageIndex(row * across + offset),
                    top,
                    // Filled in below, once the column's own width is known:
                    // a page's place across it is measured from the column,
                    // and the column is as wide as its widest row.
                    left: row_width,
                    width,
                    height: page.height * scale,
                    row_bottom: top + height,
                });
                row_width += width + PAGE_GAP;
            }
            let row_width = (row_width - PAGE_GAP).max(0.0);
            rows.push((first, placed.len(), row_width));
            widest = widest.max(row_width);
            top += height + PAGE_GAP;
        }
        let width = widest.max(cell_width.max(0.0));
        for (first, end, row_width) in rows {
            // Rows are centred in the column, so a document that mixes page
            // sizes reads down a centre line rather than flush left.
            let margin = ((width - row_width) / 2.0).max(0.0);
            for page in &mut placed[first..end] {
                page.left += margin;
            }
        }
        Column {
            height: (top - PAGE_GAP).max(0.0),
            width,
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
        // Pages stack without overlapping, so their bottoms are as ordered as
        // their tops and the window is one contiguous run: binary-search its
        // start, walk to its end. This runs on every tick and every scroll,
        // and a linear filter here made each of those cost the whole
        // document.
        let first = self.pages.partition_point(|placed| placed.row_bottom < top);
        self.pages[first..]
            .iter()
            .copied()
            .take_while(|placed| placed.top <= bottom)
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
        self.visible(offset, viewport)
            .into_iter()
            .map(|placed| {
                let overlap = placed.bottom().min(bottom) - placed.top.max(top);
                (placed.page, overlap)
            })
            .filter(|(_, overlap)| *overlap > 0.0)
            // Ties go to the earlier page, which is what a two-page spread is
            // made of: both halves show equally, and the counter says the
            // left one.
            .fold(None, |best: Option<(PageIndex, f32)>, next| match best {
                Some(best) if best.1 >= next.1 => Some(best),
                _ => Some(next),
            })
            .map(|(page, _)| page)
            // A viewport of no height sits between pages rather than on one;
            // the page whose top is nearest above is still the answer.
            .or_else(|| {
                let above = self.pages.partition_point(|placed| placed.top <= top);
                above.checked_sub(1).map(|index| self.pages[index].page)
            })
    }

    /// The scroll offset that puts `page` at the top of the window.
    pub fn offset_of(&self, page: PageIndex) -> Option<f32> {
        self.pages
            .iter()
            .find(|placed| placed.page == page)
            .map(|placed| placed.top)
    }

    /// Where `page`'s left edge sits in the column, in layout points from the
    /// column's own left edge.
    ///
    /// Pages are centred in the column, so a document whose pages are narrower
    /// than its widest one is not flush left — and a pan that measured from the
    /// page's own origin would drift by half that difference at every page
    /// boundary.
    pub fn left_of(&self, page: PageIndex) -> Option<f32> {
        self.pages
            .iter()
            .find(|placed| placed.page == page)
            .map(|placed| placed.left)
    }

    /// Keep a horizontal offset inside the column, given a window
    /// `cell_width` wide.
    ///
    /// The column is never narrower than the cell, so a page that fits across
    /// the window does not scroll sideways at all: the whole range exists only
    /// once a zoom has made the page wider than the space it is read in.
    pub fn clamp_offset_x(&self, offset: f32, cell_width: f32) -> f32 {
        if !offset.is_finite() {
            return 0.0;
        }
        let furthest = (self.width - cell_width.max(0.0)).max(0.0);
        offset.clamp(0.0, furthest)
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
    /// …and how far across it, for a zoom that makes the page wider than the
    /// window. Zero whenever the page fits across it.
    pub offset_x: f32,
    /// The page the counter shows.
    pub page: PageIndex,
    /// The armed tool, or `None` when a press reaches the document's own links
    /// and form fields instead of laying down a mark.
    pub tool: Option<pulpit_core::annotation::AnnotationTool>,
    /// Which tool's options popover is open in the toolbar, if any.
    pub tool_options: Option<pulpit_core::annotation::AnnotationTool>,
    /// Which tool's colour wheel is open, if any. The fixed swatches cover
    /// the colours a reader reaches for without thinking; the wheel is for
    /// the one they have in mind.
    pub tool_wheel: Option<pulpit_core::annotation::AnnotationTool>,
    /// Whether the compact navigation overflow menu is open.
    pub navigation_overflow: bool,
    /// Whether the compact annotation overflow menu is open.
    pub tool_overflow: bool,
    /// The colour each colour-bearing tool lays down, and the pen's width.
    /// Mirrors of the interaction's styles, held here so the toolbar can be
    /// drawn from the controls alone.
    pub ink_color: pulpit_core::annotation::InkColor,
    /// Stroke width in page points.
    pub ink_width: f32,
    pub highlight_color: pulpit_core::annotation::InkColor,
    /// What the select tool's band does with what it encloses. A mirror of
    /// the palette's own, here for the same reason the colours are: the
    /// toolbar is drawn from the controls alone.
    pub select_kind: pulpit_core::annotation::SelectKind,
    /// Which of its three marks the highlighter lays down, mirrored here for
    /// the same reason.
    pub markup_kind: pulpit_core::annotation::MarkupKind,
    /// Which shape the shape tool draws, and which mark the stamp puts down.
    /// Mirrored here for the same reason the colours are: the toolbar is
    /// drawn from the controls alone.
    pub shape_kind: pulpit_core::annotation::ShapeKind,
    pub stamp_mark: pulpit_core::annotation::StampChoice,
    pub text_color: pulpit_core::annotation::InkColor,
    /// The size placed text and notes are written at, in page points. The
    /// pen has a width control and type had nothing, which left the one
    /// measure a reader most often wants to change with no way to change it.
    pub text_size: f32,
    /// What the outline rail shows. Pages are deliberately absent: page
    /// navigation belongs to the document controls, while this rail is for
    /// the document's authored structure (and form review where applicable).
    pub outline: OutlineView,
    /// One page across the column, or two facing pages.
    pub spread: PageSpread,
    /// The reader's view rotation, applied to every page.
    ///
    /// Session-scoped and never persisted: a view transform like the crop,
    /// not a document edit — nothing here sets the dirty flag or reaches the
    /// file, and the audience never sees it.
    pub rotation: PageRotation,
    /// The marquee crop: armed, mid-choice, or in force.
    pub crop: CropState,
    //
    // Whether the outline rail is out is deliberately *not* here. It is
    // chrome around the document rather than a control of it, it outlives any
    // one document, and this struct is reset wholesale whenever a document is
    // opened — which is how the rail used to reopen itself. The application
    // owns it, as a `crate::disclosure::Disclosure`.
}

impl Default for ReaderControls {
    fn default() -> Self {
        Self {
            zoom: Zoom::default(),
            offset: 0.0,
            offset_x: 0.0,
            page: PageIndex(0),
            tool: None,
            tool_options: None,
            tool_wheel: None,
            navigation_overflow: false,
            tool_overflow: false,
            ink_color: pulpit_core::annotate::MarkStyle::default().color,
            ink_width: pulpit_core::annotate::MarkStyle::default().width,
            highlight_color: pulpit_core::annotate::MarkStyle::highlighter().color,
            select_kind: pulpit_core::annotation::SelectKind::default(),
            markup_kind: pulpit_core::annotation::MarkupKind::default(),
            shape_kind: pulpit_core::annotation::ShapeKind::default(),
            stamp_mark: pulpit_core::annotation::StampChoice::default(),
            text_color: pulpit_core::annotate::MarkStyle::default().color,
            text_size: pulpit_core::annotate::MarkStyle::default().font_size,
            outline: OutlineView::default(),
            spread: PageSpread::default(),
            rotation: PageRotation::default(),
            crop: CropState::default(),
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
    /// The document's form fields, in the order the file lists them (§6.4).
    ///
    /// Offered only for a document that has a form: a rail tab that is always
    /// empty for a deck of slides is a control that teaches the reader to
    /// ignore the rail.
    Fields,
    /// Every mark in the document, in page order (§8.4).
    ///
    /// Reached from the sidebar's own icon row rather than from the small tab
    /// row below it: the marks in a document are not a way of looking at its
    /// authored structure, they are a second thing the sidebar can hold,
    /// beside the outline and the search results.
    Annotations,
}

/// Stable identity of an item in one of the outline rail's finite views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineItemId {
    Bookmark {
        source_ordinal: usize,
    },
    Page(PageIndex),
    Field {
        name: String,
        source_ordinal: usize,
    },
    /// One annotation, by the identity the document carries (`/NM`), so a row
    /// survives the list being rebuilt after an edit — which it is, on every
    /// revision that touches the page it is on.
    Annotation(pulpit_core::annotate::AnnotationId),
}

impl OutlineView {
    pub fn label(self) -> &'static str {
        match self {
            OutlineView::Bookmarks => "Outline",
            OutlineView::Thumbnails => "Pages",
            OutlineView::Fields => "Fields",
            OutlineView::Annotations => "Annotations",
        }
    }
}

/// Which of the three things the document's one shared sidebar is holding.
///
/// The outline rail and the search pane are drawn by different widgets and
/// each draws the icon row; this is what the row highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Outline,
    Search,
    Annotations,
}

/// The longest a row's description runs before it is cut.
///
/// A `/Contents` may be a page of selected text and a rail row has one line
/// for it. Cut to roughly what a rail's width holds at the label size —
/// search's own result rows cut at forty characters for the same reason — so
/// that the line below it, which says what kind the mark is and what page it
/// is on, is never pushed out of the row by a mark that says a lot. The row
/// also draws its text without wrapping, so a long word cannot do it either.
const ANNOTATION_ROW_TEXT: usize = 48;

/// One mark, as the annotations panel lists it.
///
/// Built from the engine's `AnnotationSummary` and carrying only what a row
/// shows: the panel is a *view* of what the document says is on its pages
/// (A1), never a second store of the marks.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationRow {
    pub id: pulpit_core::annotate::AnnotationId,
    pub page: PageIndex,
    pub kind: pulpit_core::annotate::AnnotationKind,
    /// What the mark says, on one line: its Typst source where it has one,
    /// its `/Contents` otherwise, and empty for an ink stroke that says
    /// nothing at all.
    pub text: String,
    /// True when the text shown is shorter than the text the mark carries,
    /// either because the engine cut it or because this row did.
    pub truncated: bool,
    pub support: pulpit_render::document::AnnotationSupport,
    /// The colour the mark is drawn in, for the row's swatch.
    pub color: pulpit_core::annotation::InkColor,
}

impl AnnotationRow {
    pub fn of(summary: &pulpit_render::document::AnnotationSummary) -> Self {
        // The Typst source first: for a mark pulpit wrote, `/Contents` holds
        // the typeset result and the source is what the reader typed, which
        // is the thing they would recognise in a list.
        let (text, source_truncated) = match &summary.contents.pulpit_source {
            Some(source) => (source.as_str(), false),
            None => (summary.contents.text.as_str(), summary.contents.truncated),
        };
        let (text, cut) = one_line(text, ANNOTATION_ROW_TEXT);
        Self {
            id: summary.id.clone(),
            page: summary.page,
            kind: summary.kind,
            text,
            truncated: cut || source_truncated,
            support: summary.support,
            color: summary.style.color,
        }
    }

    /// What the row says when the mark says nothing: an ink stroke carries no
    /// text at all, and a row that was blank would read as a mark that failed
    /// to load rather than one that is a line on a page.
    pub fn description(&self) -> String {
        if self.text.is_empty() {
            format!("{} on page {}", self.kind.label(), self.page.get() + 1)
        } else if self.truncated {
            format!("{}…", self.text)
        } else {
            self.text.clone()
        }
    }

    /// Can this mark be taken out of the document from the list?
    ///
    /// Only what pulpit round-trips. Deleting is a rewrite, and pulpit does
    /// not rewrite what it does not model (A5) — so the row says what the
    /// mark is instead of offering a control that would refuse.
    pub fn deletable(&self) -> bool {
        self.support.is_editable()
    }

    /// What the row says about a mark pulpit did not fully understand, in
    /// words rather than by dimming a control (§10.1).
    pub fn support_note(&self) -> Option<&'static str> {
        use pulpit_render::document::AnnotationSupport;

        match self.support {
            AnnotationSupport::Editable => None,
            AnnotationSupport::ReadOnlySupported => Some("read-only"),
            AnnotationSupport::Unsupported => Some("not editable here"),
            AnnotationSupport::Malformed => Some("malformed"),
        }
    }
}

/// Collapse a mark's text onto one line and cut it to `limit` characters.
///
/// Characters rather than bytes: the cut is for a row of a rail, and a
/// boundary in the middle of a code point is not a place a string can be cut
/// at all.
fn one_line(text: &str, limit: usize) -> (String, bool) {
    let mut out = String::new();
    let mut written = 0usize;
    let mut spaced = true;
    let mut cut = false;
    for character in text.chars() {
        if character.is_whitespace() {
            if !spaced && !out.is_empty() {
                out.push(' ');
                written += 1;
                spaced = true;
            }
            continue;
        }
        if written >= limit {
            cut = true;
            break;
        }
        out.push(character);
        written += 1;
        spaced = false;
    }
    while out.ends_with(' ') {
        out.pop();
    }
    (out, cut)
}

/// A page rectangle, converted to the sheet it is drawn on (§8.6).
///
/// Everything pulpit draws over a page — the calendar, the focus ring, the
/// hint beside a field — starts from a rectangle in canonical page points and
/// has to end up in layout points on a sheet that is scrolled, zoomed and
/// possibly cropped. That conversion is one piece of arithmetic, so it lives
/// here once: computed from the geometry the sheet has *now*, never cached,
/// which is what makes an overlay track a scroll and a zoom instead of
/// sliding off the field it belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl Anchor {
    /// Place `bounds` on a sheet drawn at `drawn` layout points showing
    /// `shown` page points from the crop window's corner `origin`.
    pub fn of(
        bounds: pulpit_core::page::PageRect,
        shown: (f32, f32),
        origin: (f32, f32),
        drawn: (f32, f32),
    ) -> Self {
        let (scale_x, scale_y) = super::page_to_screen(shown, drawn);
        let left = (bounds.left - origin.0) * scale_x;
        let top = (bounds.top - origin.1) * scale_y;
        Self {
            left,
            top,
            width: ((bounds.right - bounds.left) * scale_x).max(0.0),
            height: ((bounds.bottom - bounds.top) * scale_y).max(0.0),
        }
    }

    /// Grow the rectangle by `margin` on every side, staying on the sheet.
    ///
    /// A ring drawn exactly on a widget's own edge reads as part of the
    /// field's border; a couple of points of air is what makes it read as an
    /// indicator around it.
    pub fn inflated(self, margin: f32, drawn: (f32, f32)) -> Self {
        let left = (self.left - margin).max(0.0);
        let top = (self.top - margin).max(0.0);
        Self {
            left,
            top,
            width: (self.width + 2.0 * margin).min((drawn.0 - left).max(0.0)),
            height: (self.height + 2.0 * margin).min((drawn.1 - top).max(0.0)),
        }
    }

    /// Where a panel of `size` goes: under the anchored rectangle by
    /// preference, which is where a dropdown goes, and above it when there is
    /// no room below — so a field near the foot of the page still opens
    /// something the reader can see all of.
    pub fn place_beside(self, size: (f32, f32), drawn: (f32, f32)) -> (f32, f32) {
        let left = self.left.clamp(0.0, (drawn.0 - size.0).max(0.0));
        let below = self.top + self.height;
        let above = self.top - size.1;
        let top = if below + size.1 <= drawn.1 || above < 0.0 {
            below.clamp(0.0, (drawn.1 - size.1).max(0.0))
        } else {
            above
        };
        (left, top)
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
    fn a_two_page_spread_puts_facing_pages_on_one_line() {
        let column = Column::lay_out(&letter(5), 1.0, 2_000.0, PageSpread::Double);
        // Three rows: 1–2, 3–4 and a last page with nothing beside it.
        assert_eq!(column.pages[0].top, 0.0);
        assert_eq!(column.pages[1].top, 0.0);
        assert_eq!(column.pages[2].top, 792.0 + PAGE_GAP);
        assert_eq!(column.height, 792.0 * 3.0 + PAGE_GAP * 2.0);
        // Side by side, a gap apart, and the pair centred in the column.
        assert_eq!(
            column.pages[1].left - column.pages[0].left,
            612.0 + PAGE_GAP
        );
        let pair = 612.0 * 2.0 + PAGE_GAP;
        assert_eq!(column.pages[0].left, (2_000.0 - pair) / 2.0);
        // The odd page at the end is centred on its own, not left where its
        // absent neighbour would have put it.
        assert_eq!(column.pages[4].left, (2_000.0 - 612.0) / 2.0);
    }

    #[test]
    fn both_halves_of_a_spread_are_visible_and_the_left_one_is_the_page() {
        let column = Column::lay_out(&letter(4), 1.0, 2_000.0, PageSpread::Double);
        let visible = column.visible(0.0, 792.0);
        assert_eq!(
            visible.iter().map(|placed| placed.page).collect::<Vec<_>>(),
            vec![PageIndex(0), PageIndex(1)]
        );
        assert_eq!(column.current(0.0, 792.0), Some(PageIndex(0)));
    }

    #[test]
    fn a_row_is_as_tall_as_its_taller_half() {
        let pages = vec![
            PageGeometry::upright(612.0, 792.0),
            PageGeometry::upright(612.0, 400.0),
        ];
        let column = Column::lay_out(&pages, 1.0, 2_000.0, PageSpread::Double);
        assert_eq!(column.height, 792.0);
        // The short page's row still ends where the tall one does, which is
        // what keeps the rows' bottoms in order down the column.
        assert_eq!(column.pages[1].row_bottom, 792.0);
        assert_eq!(column.pages[1].bottom(), 400.0);
    }

    #[test]
    fn pages_stack_with_a_gap_between_them() {
        let column = Column::lay_out(&letter(3), 1.0, 800.0, PageSpread::Single);
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
        let column = Column::lay_out(&pages, 1.0, 400.0, PageSpread::Single);
        assert_eq!(column.pages[1].height, 612.0);
        assert_eq!(column.pages[2].top, 792.0 + 612.0 + PAGE_GAP * 2.0);
        assert_eq!(column.width, 792.0, "the widest page sets the column");
    }

    #[test]
    fn an_empty_document_lays_out_to_nothing_rather_than_to_a_negative_column() {
        let column = Column::lay_out(&[], 1.0, 500.0, PageSpread::Single);
        assert_eq!(column.height, 0.0);
        assert!(column.pages.is_empty());
        assert_eq!(column.current(0.0, 100.0), None);
        assert_eq!(column.clamp_offset(50.0, 100.0), 0.0);
    }

    #[test]
    fn only_the_pages_in_the_window_are_visible() {
        let column = Column::lay_out(&letter(20), 1.0, 800.0, PageSpread::Single);
        let visible = column.visible(0.0, 900.0);
        assert_eq!(visible.len(), 2, "a page and the top of the next");
        assert_eq!(visible[0].page, PageIndex(0));

        let far = column.visible(column.pages[10].top, 100.0);
        assert_eq!(far.len(), 1);
        assert_eq!(far[0].page, PageIndex(10));
    }

    #[test]
    fn the_page_counter_names_the_page_you_are_mostly_looking_at() {
        let column = Column::lay_out(&letter(5), 1.0, 800.0, PageSpread::Single);
        // A sliver of page two at the bottom does not make it the page you are
        // on.
        assert_eq!(column.current(0.0, 800.0), Some(PageIndex(0)));
        // Past the midpoint of the crossing, it does.
        let crossing = column.pages[1].top - 100.0;
        assert_eq!(column.current(crossing, 800.0), Some(PageIndex(1)));
    }

    #[test]
    fn scrolling_to_a_page_puts_its_top_at_the_top() {
        let column = Column::lay_out(&letter(5), 0.5, 400.0, PageSpread::Single);
        let offset = column.offset_of(PageIndex(3)).unwrap();
        assert_eq!(offset, column.pages[3].top);
        assert_eq!(column.current(offset, 400.0), Some(PageIndex(3)));
        assert_eq!(column.offset_of(PageIndex(9)), None);
    }

    #[test]
    fn the_scroll_offset_stays_inside_the_document() {
        let column = Column::lay_out(&letter(3), 1.0, 800.0, PageSpread::Single);
        assert_eq!(column.clamp_offset(-500.0, 600.0), 0.0);
        assert_eq!(column.clamp_offset(f32::NAN, 600.0), 0.0);
        assert_eq!(column.clamp_offset(1e9, 600.0), column.height - 600.0);
        // A document shorter than the window does not scroll at all.
        let short = Column::lay_out(&letter(1), 0.1, 800.0, PageSpread::Single);
        assert_eq!(short.clamp_offset(400.0, 2_000.0), 0.0);
    }

    #[test]
    fn a_nonsense_scale_lays_out_at_one_rather_than_collapsing_the_column() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let column = Column::lay_out(&letter(2), scale, 500.0, PageSpread::Single);
            assert!(column.height > 0.0, "scale {scale} collapsed the column");
        }
    }

    #[test]
    fn the_outline_rail_has_two_views_and_opens_on_the_bookmarks() {
        assert_eq!(OutlineView::default(), OutlineView::Bookmarks);
        assert_eq!(OutlineView::Bookmarks.label(), "Outline");
        assert_eq!(OutlineView::Thumbnails.label(), "Pages");
        // Each tab names itself, so a view that is in front is still labelled.
        assert!(!ReaderControls::default().navigation_overflow);
        assert!(!ReaderControls::default().tool_overflow);
    }

    #[test]
    fn a_turned_page_swaps_its_displayed_axes_and_nothing_else() {
        let page = PageGeometry::upright(612.0, 792.0);
        let turned = view_rotated(&page, PageRotation::Clockwise90);
        assert_eq!(turned.width, 792.0);
        assert_eq!(turned.height, 612.0);
        // The document's own description is untouched: this is the reader's
        // window, not a rewrite of the page.
        assert_eq!(turned.rotation, page.rotation);
        assert_eq!(turned.crop_width, page.crop_width);
        assert_eq!(view_rotated(&page, PageRotation::Clockwise180), page);
    }

    #[test]
    fn a_two_page_spread_of_turned_pages_lays_out_landscape_rows() {
        let pages: Vec<PageGeometry> = letter(5)
            .iter()
            .map(|page| view_rotated(page, PageRotation::Clockwise90))
            .collect();
        let column = Column::lay_out(&pages, 1.0, 2_000.0, PageSpread::Double);
        // Rows are as tall as the turned page's height and the halves stand a
        // gap apart, exactly as upright pages would.
        assert_eq!(column.pages[0].height, 612.0);
        assert_eq!(column.pages[2].top, 612.0 + PAGE_GAP);
        assert_eq!(
            column.pages[1].left - column.pages[0].left,
            792.0 + PAGE_GAP
        );
        // Row bottoms stay ordered down the column, which is what the
        // binary search over visible pages depends on.
        assert!(column
            .pages
            .windows(2)
            .all(|pair| pair[0].row_bottom <= pair[1].row_bottom));
    }

    #[test]
    fn the_fit_follows_the_turned_page() {
        let page = PageGeometry::upright(600.0, 800.0);
        let turned = view_rotated(&page, PageRotation::Clockwise90);
        // Fitting the width of a landscape-turned portrait page is fitting
        // its former height.
        assert!((Zoom::FitWidth.scale(&turned, (400.0, 400.0)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_region_turns_with_the_page_and_turns_back() {
        let region = Region::new(0.1, 0.2, 0.3, 0.4);
        for rotation in PageRotation::ALL {
            let turned = rotated_region(region, rotation);
            assert!(turned.x >= 0.0 && turned.y >= 0.0);
            assert!(turned.x + turned.width <= 1.0 + 1e-6);
            assert!(turned.y + turned.height <= 1.0 + 1e-6);
            let back = rotated_region(turned, rotation.inverse());
            assert!((back.x - region.x).abs() < 1e-6, "{rotation:?}");
            assert!((back.y - region.y).abs() < 1e-6, "{rotation:?}");
            assert!((back.width - region.width).abs() < 1e-6);
            assert!((back.height - region.height).abs() < 1e-6);
        }
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

    #[test]
    fn an_anchor_follows_the_zoom_and_the_crop() {
        use pulpit_core::page::PageRect;

        let bounds = PageRect::new(100.0, 200.0, 200.0, 220.0);
        // The whole page, drawn at twice its size.
        let anchor = Anchor::of(bounds, (600.0, 800.0), (0.0, 0.0), (1200.0, 1600.0));
        assert_eq!(anchor.left, 200.0);
        assert_eq!(anchor.top, 400.0);
        assert_eq!(anchor.width, 200.0);
        assert_eq!(anchor.height, 40.0);
        // The same page cropped to its lower right quarter and drawn at the
        // same size: the field moves with the window's corner.
        let cropped = Anchor::of(bounds, (300.0, 400.0), (100.0, 200.0), (1200.0, 1600.0));
        assert_eq!(cropped.left, 0.0);
        assert_eq!(cropped.top, 0.0);
    }

    #[test]
    fn a_panel_goes_under_the_field_unless_the_page_ends_first() {
        use pulpit_core::page::PageRect;

        let sheet = (600.0, 800.0);
        let near_the_top = Anchor::of(
            PageRect::new(10.0, 10.0, 110.0, 30.0),
            sheet,
            (0.0, 0.0),
            sheet,
        );
        assert_eq!(near_the_top.place_beside((80.0, 24.0), sheet), (10.0, 30.0));
        let near_the_foot = Anchor::of(
            PageRect::new(10.0, 790.0, 110.0, 799.0),
            sheet,
            (0.0, 0.0),
            sheet,
        );
        let (_, top) = near_the_foot.place_beside((80.0, 24.0), sheet);
        assert_eq!(top, 766.0, "no room below, so it opens above");
    }

    #[test]
    fn an_inflated_anchor_stays_on_the_sheet() {
        use pulpit_core::page::PageRect;

        let sheet = (600.0, 800.0);
        let corner = Anchor::of(
            PageRect::new(0.0, 0.0, 20.0, 20.0),
            sheet,
            (0.0, 0.0),
            sheet,
        )
        .inflated(3.0, sheet);
        assert_eq!(corner.left, 0.0);
        assert_eq!(corner.top, 0.0);
        let far = Anchor::of(
            PageRect::new(580.0, 780.0, 600.0, 800.0),
            sheet,
            (0.0, 0.0),
            sheet,
        )
        .inflated(3.0, sheet);
        assert_eq!(far.left + far.width, 600.0);
        assert_eq!(far.top + far.height, 800.0);
    }

    fn summary(
        contents: pulpit_render::document::AnnotationContents,
        kind: pulpit_core::annotate::AnnotationKind,
        support: pulpit_render::document::AnnotationSupport,
    ) -> pulpit_render::document::AnnotationSummary {
        pulpit_render::document::AnnotationSummary {
            id: pulpit_core::annotate::IdGenerator::new(1).next_id(),
            page: PageIndex(11),
            kind,
            bounds: pulpit_core::page::PageRect::new(10.0, 10.0, 100.0, 40.0),
            style: pulpit_core::annotate::MarkStyle::default(),
            contents,
            support,
            revision: pulpit_render::document::DocumentRevision::INITIAL,
            path: Vec::new(),
            quads: Vec::new(),
            geometry_elided: false,
            stamp: None,
        }
    }

    /// What a reader would recognise is what they typed, not the typeset
    /// result pulpit put in `/Contents` for other viewers to draw.
    #[test]
    fn a_row_shows_the_typst_source_of_a_mark_pulpit_wrote() {
        use pulpit_core::annotate::AnnotationKind;
        use pulpit_render::document::{AnnotationContents, AnnotationSupport};

        let row = AnnotationRow::of(&summary(
            AnnotationContents {
                text: "E = mc²".into(),
                truncated: false,
                pulpit_source: Some("$E = m c^2$".into()),
            },
            AnnotationKind::FreeText,
            AnnotationSupport::Editable,
        ));
        assert_eq!(row.text, "$E = m c^2$");
        assert_eq!(row.description(), "$E = m c^2$");
        assert!(row.deletable());
        assert_eq!(row.support_note(), None);
    }

    /// A `/Contents` may be a page of selected text and a row is one line.
    #[test]
    fn a_row_collapses_the_text_onto_one_line_and_cuts_it() {
        use pulpit_core::annotate::AnnotationKind;
        use pulpit_render::document::{AnnotationContents, AnnotationSupport};

        let long = "word ".repeat(60);
        let row = AnnotationRow::of(&summary(
            AnnotationContents {
                text: format!("  two\n\nlines  {long}"),
                truncated: false,
                pulpit_source: None,
            },
            AnnotationKind::Highlight,
            AnnotationSupport::Editable,
        ));
        assert!(row.text.starts_with("two lines word"), "{}", row.text);
        assert!(!row.text.contains('\n'));
        assert!(row.truncated);
        assert!(row.description().ends_with('…'));
    }

    /// An ink stroke says nothing at all, and a blank row would read as a
    /// mark that failed to load rather than as a line on a page.
    #[test]
    fn a_row_for_a_mark_with_no_text_says_what_it_is_and_where() {
        use pulpit_core::annotate::AnnotationKind;
        use pulpit_render::document::{AnnotationContents, AnnotationSupport};

        let row = AnnotationRow::of(&summary(
            AnnotationContents::default(),
            AnnotationKind::Ink,
            AnnotationSupport::Malformed,
        ));
        assert_eq!(row.description(), "Ink on page 12");
        // A mark pulpit only preserves says so, and offers no delete: the
        // words are the answer, never a dimmed control (§10.1).
        assert_eq!(row.support_note(), Some("malformed"));
        assert!(!row.deletable());
    }

    #[test]
    fn the_marks_are_a_view_of_their_own_with_a_name() {
        assert_eq!(OutlineView::Annotations.label(), "Annotations");
        assert_eq!(OutlineView::default(), OutlineView::Bookmarks);
    }
}
