//! What a widget is allowed to know.
//!
//! Widgets never see `App`, the presentation state, the display coordinator
//! or the frame cache. They see these facets: immutable values assembled by
//! the application, one per domain, so a renderer's dependencies are visible
//! in its signature.

use std::time::Duration;

use iced::widget::image::Handle as ImageHandle;

use pulpit_core::Blank;
use pulpit_render::cache::FrameKind;

/// What the renderer is drawing for.
///
/// Explicit, never inferred from missing data: an empty deck in live mode and
/// a deliberate sample in the editor are different situations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The live presenter screen: controls work.
    Live,
    /// Preview inside the editor: realistic content, inert controls.
    #[allow(dead_code)] // the editor draws in Editing mode; kept for the audience preview
    Preview,
    /// The editor canvas: representations, selection, no live data.
    Editing,
}

impl Mode {
    pub fn interactive(self) -> bool {
        self == Mode::Live
    }

    /// Is the content sample data rather than the real document?
    pub fn is_sample(self) -> bool {
        self != Mode::Live
    }

    /// Empty cells collapse at presentation time but stay visible in the
    /// editor so they can still be selected.
    pub fn collapse_empty(self) -> bool {
        self != Mode::Editing
    }
}

/// Rendered slide images, by slide, kind and the width the pane can use.
///
/// A widget-facing contract, not the application's cache: it can ask for a
/// picture and nothing else. Handing a 4K frame to a thumbnail wastes upload
/// bandwidth, so the caller picks the best frame *for that size*.
pub trait FrameSource {
    fn frame(&self, slide: usize, kind: FrameKind, max_width: u32) -> Option<ImageHandle>;
}

impl<F> FrameSource for F
where
    F: Fn(usize, FrameKind, u32) -> Option<ImageHandle>,
{
    fn frame(&self, slide: usize, kind: FrameKind, max_width: u32) -> Option<ImageHandle> {
        self(slide, kind, max_width)
    }
}

/// The deck and its pictures.
/// Text speaker notes, with everything needed to find one slide's note.
///
/// Borrowed rather than resolved into a vector per frame: the mapping can
/// change while the document is open, and a lookup that reads the mapping in
/// force cannot go stale behind one that was resolved earlier.
#[derive(Debug, Clone, Copy)]
pub struct TextNotesData<'a> {
    pub notes: &'a pulpit_core::TextNotes,
    pub mapping: &'a pulpit_core::NotesMapping,
    pub pdf_pages: usize,
}

impl<'a> TextNotesData<'a> {
    pub fn for_slide(self, slide: usize) -> Option<&'a str> {
        self.notes.for_slide(slide, self.mapping, self.pdf_pages)
    }
}

pub struct SlideData<'a> {
    /// What the audience is seeing.
    pub current: usize,
    /// What the presenter is looking at, which may be ahead.
    pub preview: usize,
    pub count: usize,
    pub frames: &'a dyn FrameSource,
    /// Roughly how wide a preview pane is, in pixels.
    pub preview_width: u32,
    /// Aspect ratio (width / height) of the current slide's content, used to
    /// undo the letterbox when mapping pointer positions onto the page.
    pub aspect: f32,
    /// Text notes the document carries, when it carries any. Preferred over a
    /// rendered notes region: text reflows, scales with the pane, and does not
    /// need a second render of every page.
    pub text_notes: Option<TextNotesData<'a>>,
    /// Does the current slide carry any link annotations? Purely a cursor
    /// affordance; hit-testing happens in the application.
    pub has_links: bool,
    /// Link rectangles to outline, and why. Presenter-only: an affordance is
    /// chrome, and chrome never reaches the audience window.
    pub link_highlights: Vec<LinkHighlight>,
    /// Media overlays drawn over the current slide, back to front. Each is
    /// the overlay's page rectangle and its most recent complete frame; an
    /// overlay with no frame yet is absent, so the PDF page shows through.
    pub overlays: Vec<SlideOverlay>,
    /// The part of the page currently visible, so an overlay follows a
    /// `/FitR` zoom instead of staying pinned to the panel.
    pub crop: pulpit_core::notes::Region,
    /// The presenter's own marks over the current slide, and what the
    /// pointer is armed to do. Shared: the panel that draws them keeps a
    /// reference-counted handle rather than a copy of every stroke.
    pub annotations: &'a std::sync::Arc<pulpit_core::annotation::Annotations>,
    pub rendered_text:
        &'a std::sync::Arc<std::collections::HashMap<u64, crate::typst_annotation::RenderedText>>,
    /// The geometry cache the marks layer replays between annotation
    /// changes. Owned: an `Arc` clone is two words, and `canvas::Cache` is
    /// not `Sync`, so the sample context could not borrow one from a static.
    pub marks_cache: std::rc::Rc<iced::widget::canvas::Cache>,
    /// Live colour/size choices and which tool's option popover is open.
    pub annotation_controls: crate::widgets::AnnotationControls,
    /// How big those marks are drawn, as fractions of the page.
    pub annotation_style: pulpit_core::annotation::AnnotationStyle,
}

/// A link the presenter can see the shape of before pressing it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkHighlight {
    pub rect: pulpit_core::notes::Region,
    pub reason: HighlightReason,
}

/// Why a link is outlined. Keyboard focus is drawn more strongly than hover:
/// the pointer is already an answer to "where am I", and the keyboard is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightReason {
    Hovered,
    Focused,
}

/// Which links to outline, given what the pointer and the keyboard are on.
///
/// Focus is shown even when the pointer has wandered off, but a link that is
/// both hovered and focused is outlined once: two rings on one rectangle
/// reads as a rendering fault rather than as two kinds of attention.
pub fn links_to_highlight(
    hovered: Option<usize>,
    focused: Option<usize>,
) -> Vec<(usize, HighlightReason)> {
    let mut out = Vec::new();
    if let Some(index) = focused {
        out.push((index, HighlightReason::Focused));
    }
    if let Some(index) = hovered {
        if Some(index) != focused {
            out.push((index, HighlightReason::Hovered));
        }
    }
    out
}

/// One overlay ready to be composited over the current slide.
#[derive(Debug, Clone)]
pub struct SlideOverlay {
    pub region: pulpit_core::notes::Region,
    pub handle: iced::widget::image::Handle,
    /// Interactive overlays get a presenter-only focus ring and a pointer
    /// cursor; a video or a GIF does not.
    pub interactive: bool,
}

impl SlideData<'_> {
    /// "12 / 40", or an em dash when there is no deck.
    pub fn label(&self, slide: usize) -> String {
        if self.count == 0 {
            "—".to_string()
        } else {
            format!("{} / {}", slide + 1, self.count)
        }
    }
}

/// The clock and the timer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimingData {
    pub elapsed: Duration,
    pub target: Option<Duration>,
    pub running: bool,
    pub seconds_of_day: u32,
}

/// The document itself.
///
/// The title is owned: it is short, it changes when the document does, and a
/// borrow would mean caching it somewhere to keep the lifetime alive.
pub struct DocumentData<'a> {
    pub title: String,
    pub section: Option<String>,
    /// Sample notes for preview mode, and the long-notes simulation.
    pub sample_notes: &'a str,
}

/// What the audience is seeing, and on what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudienceData {
    pub blank: Blank,
    pub connected: bool,
    pub fullscreen: bool,
}

/// The media on the current slide, as its transport needs to see it.
///
/// Presenter-only, like every other facet: the audience window draws slides
/// and overlay frames, never the layout, so nothing here can reach it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaData {
    /// `None` when the slide carries no video or animation at all.
    pub transport: Option<crate::widgets::media::model::Transport>,
}

/// A page of the open document, ready to be drawn in the scrolled column.
#[derive(Debug, Clone)]
pub struct ReaderPage {
    pub placed: crate::widgets::document::model::PlacedPage,
    /// The page's canonical size in PDF points, so a pointer position inside
    /// the drawn sheet can be turned into a canonical page point (A4).
    ///
    /// Carried with the page rather than derived from the zoom, because a
    /// document that mixes portrait body pages with a landscape appendix has
    /// no single scale and the conversion has to be per page.
    pub canonical: (f32, f32),
    /// The most recent complete frame for this page at roughly this size, or
    /// `None` while one is being rendered. A page with no frame yet draws its
    /// sheet and nothing on it, rather than nothing at all: the column must
    /// not jump when a frame arrives.
    pub frame: Option<ImageHandle>,
}

/// One entry in the outline rail.
#[derive(Debug, Clone)]
pub struct OutlineRow {
    pub title: String,
    pub page: pulpit_core::page::PageIndex,
    /// How deep in the bookmark tree, for the indent.
    pub depth: usize,
}

/// The open document, as the reader's widgets need to see it.
///
/// Assembled by the application from the worker's answers; nothing here is a
/// PDFium handle or an object number (A3), and every rectangle is canonical
/// page geometry (A4).
#[allow(dead_code)] // see `crate::reader::ReaderSession`
pub struct ReaderData<'a> {
    /// `None` when no document is open, which is a different thing from a
    /// document with no pages.
    pub open: bool,
    pub page_count: usize,
    /// Where every page sits in the scrolled column at the current zoom.
    pub column: &'a crate::widgets::document::model::Column,
    /// The pages currently in the window, with whatever frames exist.
    pub visible: Vec<ReaderPage>,
    pub controls: &'a crate::widgets::document::model::ReaderControls,
    /// The resolved scale, so the zoom control can say "83%" for a fit.
    pub scale: f32,
    pub outline: &'a [OutlineRow],
    pub fields: &'a [pulpit_render::document::FormField],
    /// Does the document carry an AcroForm at all? A form document whose
    /// fields could not be listed is a different thing from one with no form,
    /// and the inspector says which.
    pub has_form: bool,
    /// What pulpit can honour in this document, and what it cannot (§3.4).
    pub level: pulpit_render::document::CompatibilityLevel,
    pub warnings: &'a [pulpit_render::document::DocumentWarning],
    /// Has anything been changed since the document was opened (§11.2)?
    pub dirty: bool,
    /// What has been typed into the page box, when the reader is typing in it.
    pub page_entry: Option<String>,
    pub can_undo: bool,
    pub can_redo: bool,
}

impl ReaderData<'_> {
    /// "12 / 40", or an em dash when nothing is open.
    pub fn counter(&self) -> String {
        if !self.open || self.page_count == 0 {
            "—".to_string()
        } else {
            format!("{} / {}", self.controls.page.get() + 1, self.page_count)
        }
    }

    /// Can the highlighter, the eraser and the rest do anything here?
    pub fn annotatable(&self) -> bool {
        self.open && self.level.allows_annotation()
    }
}

/// The facets together, for the dispatcher to pick from.
///
/// Only the layout renderer takes this whole thing. A family render function
/// takes the facet it uses, so what it depends on is in its signature.
pub struct Context<'a> {
    pub mode: Mode,
    pub slides: SlideData<'a>,
    pub timing: TimingData,
    /// The wall-clock cues, which the clock widget shows and edits. Borrowed
    /// rather than copied into [`TimingData`]: the list is owned by the
    /// application, the way annotation controls are.
    pub alarms: &'a crate::widgets::AlarmControls,
    /// Which way the timer is running, and how long the talk is. Borrowed for
    /// the same reason as the alarms: it belongs to the talk.
    pub timer_controls: &'a crate::widgets::TimerControls,
    pub document: DocumentData<'a>,
    pub audience: AudienceData,
    pub media: MediaData,
    /// The open document, for the reader's widgets. Present in every context
    /// because a document layout and a presenter layout are the same layout
    /// machinery (§2); a presenter layout simply has no reader widget to hand
    /// it to.
    pub reader: ReaderData<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_pointed_at_is_nothing_outlined() {
        assert!(links_to_highlight(None, None).is_empty());
    }

    #[test]
    fn the_hovered_link_is_outlined_on_its_own() {
        assert_eq!(
            links_to_highlight(Some(2), None),
            vec![(2, HighlightReason::Hovered)]
        );
    }

    #[test]
    fn focus_is_shown_even_when_the_pointer_has_wandered_off() {
        assert_eq!(
            links_to_highlight(None, Some(1)),
            vec![(1, HighlightReason::Focused)]
        );
    }

    #[test]
    fn a_link_that_is_both_hovered_and_focused_is_outlined_once() {
        assert_eq!(
            links_to_highlight(Some(3), Some(3)),
            vec![(3, HighlightReason::Focused)],
            "the stronger reason wins, and only one ring is drawn"
        );
    }

    #[test]
    fn hovering_one_link_while_another_is_focused_shows_both() {
        let highlights = links_to_highlight(Some(0), Some(4));
        assert_eq!(
            highlights,
            vec![(4, HighlightReason::Focused), (0, HighlightReason::Hovered)]
        );
    }
}
