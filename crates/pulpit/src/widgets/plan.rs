//! Declarative frame demands: what a placed widget needs rendered, not how
//! the host goes and gets it.
//!
//! Before this module, [`crate::layout::panels`] matched on [`WidgetKind`]
//! directly to decide which page a leaf's fraction of the window fed into.
//! That match is the same kind of scattering the registry (`registry.rs`)
//! already closed off for `view`: a new slide-shaped widget had to be added
//! to the layout walk *and* to the registry *and* to the catalog, in three
//! places, or its frames were silently never requested.
//!
//! A widget now declares its needs — which page, relative to the committed
//! one, at what share of its cell, and how urgently — through the same
//! per-kind table `registry.rs` already uses for `view`. The host
//! ([`crate::layout::panels`]) only aggregates: it does not know what a
//! `PreviousCurrentNext` strip is, only that some widget asked for three
//! pages at three fractions.
//!
//! Only frames are covered here. Media wakeups and input are out of scope
//! for this phase.

use super::{Widget, WidgetKind};

/// Which page, relative to the committed one, a frame need is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageRef {
    /// The committed page — `slides.current` in the widget model.
    Current,
    /// The page before the committed one.
    Previous,
    /// The page after the committed one.
    Next,
}

/// How urgently a declared frame is needed, mirroring the Current/Neighbour
/// distinction [`crate::layout::panels::Role`] already drew, plus a slot for
/// work that is nice to have ready but never on screen by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FramePriority {
    /// Background warming: nothing draws this today, but having it ready
    /// avoids a stall if the presenter moves there.
    ///
    /// No shipped widget declares this yet — the aggregation this phase adds
    /// only replaces `layout::panels`' `Role::Current`/`Role::Neighbour`
    /// split, which has no prefetch-only case — but the variant is part of
    /// the vocabulary a future widget (or `request_renders`' own prefetch
    /// step, ported later) needs, so it is declared now rather than added
    /// alongside its first user.
    #[allow(dead_code)]
    Prefetch,
    /// A neighbour panel — visible, but not what the presenter is reading.
    Neighbour,
    /// On screen right now, and what the presenter is reading.
    Visible,
}

/// One frame a widget wants: a page, at a share of the window, at some
/// priority.
///
/// `width_fraction` is the widget's own share of its cell's width — the same
/// quantity [`crate::layout::panels::walk`] used to carry as a bare `f32`
/// before this module existed. It is not yet a pixel width: converting a
/// fraction of the window into a bucketed, floored pixel count is the host's
/// job, because only the host knows the window size, the scale factor and
/// the page aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameNeed {
    pub page: PageRef,
    pub width_fraction: f32,
    pub priority: FramePriority,
}

/// Everything one placed widget needs rendered.
#[derive(Debug, Clone, Default)]
pub struct WidgetPlan {
    pub frames: Vec<FrameNeed>,
}

impl WidgetPlan {
    pub const fn empty() -> WidgetPlan {
        WidgetPlan { frames: Vec::new() }
    }
}

/// The three-up strip divides its own cell, so its panels are not the size
/// of the cell that holds them. These are the portions in
/// `widgets::slides::view::strip`; `layout::panels` used to hold its own
/// copy of the same two constants.
const STRIP_CURRENT: f32 = 46.0 / 100.0;
const STRIP_SIDE: f32 = 27.0 / 100.0;

/// The plan for a single slide panel: the current page, or a neighbour.
pub fn slide_panel(kind: WidgetKind, cell_width: f32) -> WidgetPlan {
    let need = match kind {
        WidgetKind::CurrentSlide => FrameNeed {
            page: PageRef::Current,
            width_fraction: cell_width,
            priority: FramePriority::Visible,
        },
        WidgetKind::PreviousSlide => FrameNeed {
            page: PageRef::Previous,
            width_fraction: cell_width,
            priority: FramePriority::Neighbour,
        },
        WidgetKind::NextSlide => FrameNeed {
            page: PageRef::Next,
            width_fraction: cell_width,
            priority: FramePriority::Neighbour,
        },
        _ => return WidgetPlan::empty(),
    };
    WidgetPlan { frames: vec![need] }
}

/// The plan for the previous/current/next strip: one cell, three pages, at
/// the strip's own internal split rather than the cell's full width.
pub fn previous_current_next(cell_width: f32) -> WidgetPlan {
    WidgetPlan {
        frames: vec![
            FrameNeed {
                page: PageRef::Previous,
                width_fraction: cell_width * STRIP_SIDE,
                priority: FramePriority::Neighbour,
            },
            FrameNeed {
                page: PageRef::Current,
                width_fraction: cell_width * STRIP_CURRENT,
                priority: FramePriority::Visible,
            },
            FrameNeed {
                page: PageRef::Next,
                width_fraction: cell_width * STRIP_SIDE,
                priority: FramePriority::Neighbour,
            },
        ],
    }
}

/// A widget with no frame needs of its own.
pub fn none(_widget: &Widget, _cell_width: f32) -> WidgetPlan {
    WidgetPlan::empty()
}

/// The document-page widget's plan.
///
/// Deliberately empty for now. `PageRef` only names pages relative to the
/// *committed slide* (Previous/Current/Next), which is not what the reader
/// shows: `DocumentPage` draws whatever page(s) the reader has scrolled to,
/// by absolute index, and its frames already flow through
/// `App::request_reader_renders` and `Reader::visible_pages` — a pipeline
/// this phase does not touch (see module docs; `layout::panels` only ever
/// aggregated slide panels, never reader pages). Registered here, returning
/// nothing, so the table stays honest about what has and has not been
/// ported rather than silently omitting the kind.
pub fn document_page(_widget: &Widget, _cell_width: f32) -> WidgetPlan {
    WidgetPlan::empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_slide_wants_the_current_page_visible() {
        let plan = slide_panel(WidgetKind::CurrentSlide, 0.72);
        assert_eq!(plan.frames.len(), 1);
        assert_eq!(plan.frames[0].page, PageRef::Current);
        assert_eq!(plan.frames[0].priority, FramePriority::Visible);
        assert_eq!(plan.frames[0].width_fraction, 0.72);
    }

    #[test]
    fn neighbour_panels_want_their_page_as_neighbour() {
        let previous = slide_panel(WidgetKind::PreviousSlide, 0.28);
        assert_eq!(previous.frames[0].page, PageRef::Previous);
        assert_eq!(previous.frames[0].priority, FramePriority::Neighbour);

        let next = slide_panel(WidgetKind::NextSlide, 0.28);
        assert_eq!(next.frames[0].page, PageRef::Next);
        assert_eq!(next.frames[0].priority, FramePriority::Neighbour);
    }

    #[test]
    fn the_strip_divides_its_own_cell_into_three() {
        let plan = previous_current_next(1.0);
        assert_eq!(plan.frames.len(), 3);
        assert_eq!(plan.frames[0].page, PageRef::Previous);
        assert_eq!(plan.frames[0].width_fraction, STRIP_SIDE);
        assert_eq!(plan.frames[1].page, PageRef::Current);
        assert_eq!(plan.frames[1].width_fraction, STRIP_CURRENT);
        assert_eq!(plan.frames[2].page, PageRef::Next);
        assert_eq!(plan.frames[2].width_fraction, STRIP_SIDE);
    }

    #[test]
    fn other_kinds_declare_nothing() {
        assert!(slide_panel(WidgetKind::Timer, 1.0).frames.is_empty());
    }
}
