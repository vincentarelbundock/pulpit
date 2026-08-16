//! What a layout thumbnail should draw, decided before anything is drawn.
//!
//! A thumbnail made of nested boxes says how a layout is divided and nothing
//! else. The questions people actually bring to a shelf of layouts — will my
//! deck fit, where does the slide land, how much of that pane is letterbox —
//! need the same arithmetic the editor uses, at thumbnail scale.
//!
//! So the thumbnail is a value first: every rectangle, the widget each one
//! holds, a hint at what that widget looks like, and for slide panes the
//! fitted slide and the bars around it. Rendering is then a short traversal
//! with no decisions left in it, and every decision is unit-testable without
//! a window.

use crate::layout::fit::FittedSlide;
use crate::layout::model::AspectRatio;
use crate::layout::tree::{self, Divider, Frame, Node, NodeId};
use crate::widgets::{Family, WidgetKind};

/// A sketch of one pane's contents, chosen so a glance distinguishes a notes
/// pane from a clock from a row of buttons.
///
/// Deliberately coarse: a thumbnail that tried to be a screenshot would be
/// unreadable at this size, and would go stale the moment a widget changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Nothing placed here yet.
    Empty,
    /// A slide surface; the pane's `slide` field says where it lands.
    Slide,
    /// Ragged lines of prose, as speaker notes look from across the room.
    Lines(u8),
    /// A large figure: a clock, a timer, a slide counter.
    Readout,
    /// A row of press targets.
    Buttons(u8),
    /// A slider's track and handle.
    Track,
    /// A single line of status or title text.
    Caption,
    /// Freehand marks over the slide.
    Marks,
}

impl Content {
    /// What a pane holding `kind` should sketch.
    pub fn of(kind: WidgetKind) -> Content {
        match kind {
            WidgetKind::CurrentSlide
            | WidgetKind::PreviousSlide
            | WidgetKind::NextSlide
            | WidgetKind::PreviousCurrentNext => Content::Slide,
            WidgetKind::SpeakerNotes => Content::Lines(4),
            WidgetKind::Timer | WidgetKind::Clock => Content::Readout,
            WidgetKind::SlideCounter => Content::Readout,
            WidgetKind::SlideButtons => Content::Buttons(2),
            WidgetKind::PauseResume | WidgetKind::EndPresentation | WidgetKind::MainMenu => {
                Content::Buttons(1)
            }
            WidgetKind::AudienceControls => Content::Buttons(2),
            // A scrub bar is the most recognisable thing about it, so the
            // thumbnail sketches the track rather than the button.
            WidgetKind::SlideSlider | WidgetKind::MediaTransport => Content::Track,
            WidgetKind::PresentationTitle
            | WidgetKind::CurrentSection
            | WidgetKind::AudienceScreenStatus
            | WidgetKind::ConnectionStatus => Content::Caption,
            WidgetKind::Annotations | WidgetKind::AnnotationTools => Content::Marks,
            // The reader.
            WidgetKind::DocumentPage => Content::Slide,
            WidgetKind::DocumentNav => Content::Buttons(2),
            // A rail of bookmark titles and page numbers sketches as lines.
            WidgetKind::DocumentOutline => Content::Lines(5),
            // A query field over a short list of what it found.
            WidgetKind::Search => Content::Lines(3),
        }
    }
}

/// One pane of a thumbnail.
#[derive(Debug, Clone, PartialEq)]
pub struct ThumbnailCell {
    pub id: NodeId,
    /// The pane itself, in thumbnail coordinates.
    pub frame: Frame,
    pub widget: Option<WidgetKind>,
    pub content: Content,
    /// Where the slide lands inside the pane, for slide panes only. The
    /// difference between this and `frame` is the letterbox.
    pub slide: Option<FittedSlide>,
}

impl ThumbnailCell {
    /// The short name to draw when there is room for one.
    pub fn label(&self) -> &'static str {
        self.widget.map(WidgetKind::short_label).unwrap_or("")
    }

    /// Does this pane letterbox its slide?
    pub fn has_letterbox(&self) -> bool {
        self.slide.is_some_and(|slide| slide.bars().is_some())
    }
}

/// Everything a thumbnail draws, at the size it will be drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub bounds: Frame,
    /// The slide aspect ratio the slide panes were fitted at.
    pub slide_ratio: f32,
    /// Panes in the order the tree gives them, which is reading order.
    pub cells: Vec<ThumbnailCell>,
    /// Internal relationships between panes. The renderer draws these as
    /// hairlines instead of outlining every cell.
    pub dividers: Vec<Divider>,
}

impl Thumbnail {
    /// Work out a thumbnail of `root` at `width`×`height`, previewing slide
    /// content at `slide_ratio`.
    ///
    /// The slide ratio is a separate argument from anything in the tree: the
    /// same layout letterboxes differently for a 4:3 deck than for a 16:9
    /// one, and that difference is the whole reason to draw the slide bounds.
    pub fn of(root: &Node, width: f32, height: f32, slide_ratio: AspectRatio) -> Thumbnail {
        let bounds = Frame::new(0.0, 0.0, width.max(0.0), height.max(0.0));
        let ratio = slide_ratio.ratio();
        let (placements, dividers) = tree::compute(root, bounds, false);
        let mut widgets = Vec::new();
        collect_leaves(root, &mut widgets);

        let cells = placements
            .into_iter()
            .filter_map(|placement| {
                let widget = widgets
                    .iter()
                    .find(|(id, _)| *id == placement.id)
                    .map(|(_, widget)| *widget)?;
                let content = widget.map(Content::of).unwrap_or(Content::Empty);
                Some(ThumbnailCell {
                    id: placement.id,
                    frame: placement.frame,
                    widget,
                    content,
                    slide: (content == Content::Slide)
                        .then(|| FittedSlide::fit(placement.frame, ratio))
                        .flatten(),
                })
            })
            .collect();

        Thumbnail {
            bounds,
            slide_ratio: ratio,
            cells,
            dividers,
        }
    }

    /// The panes that hold slide content, in reading order.
    pub fn slide_cells(&self) -> Vec<&ThumbnailCell> {
        self.cells
            .iter()
            .filter(|cell| cell.slide.is_some())
            .collect()
    }
}

/// The frames of the panes that hold slide content.
///
/// This is the bridge between the widget catalogue and [`crate::layout::fit`],
/// which stays free of widget knowledge on purpose: everything that has to
/// know *which* widgets show slides asks here instead.
pub fn slide_cells(root: &Node, area: Frame) -> Vec<Frame> {
    let (placements, _) = tree::compute(root, area, false);
    let mut widgets = Vec::new();
    collect_leaves(root, &mut widgets);
    placements
        .into_iter()
        .filter(|placement| {
            widgets.iter().any(|(id, widget)| {
                *id == placement.id && widget.is_some_and(|kind| kind.family() == Family::Slides)
            })
        })
        .map(|placement| placement.frame)
        .collect()
}

fn collect_leaves(node: &Node, into: &mut Vec<(NodeId, Option<WidgetKind>)>) {
    match node {
        Node::Leaf(cell) => into.push((cell.id, cell.widget.as_ref().map(|widget| widget.kind()))),
        Node::Split(split) => {
            for child in &split.children {
                collect_leaves(child, into);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::builtin::{slide_notes_beside, slide_time_below};
    use crate::layout::Layout;

    #[test]
    fn a_thumbnail_has_one_entry_per_pane_and_none_for_the_splits() {
        let layout = slide_notes_beside();
        let thumbnail = Thumbnail::of(&layout.root, 320.0, 180.0, AspectRatio::SixteenNine);
        assert_eq!(thumbnail.cells.len(), layout.cells().len());
        for cell in &thumbnail.cells {
            assert!(layout.cell(cell.id).is_some(), "every pane is a real cell");
        }
    }

    #[test]
    fn a_thumbnail_carries_only_internal_cell_separators() {
        let layout = slide_time_below();
        let thumbnail = Thumbnail::of(&layout.root, 320.0, 180.0, AspectRatio::SixteenNine);
        let (_, dividers) = tree::compute(&layout.root, thumbnail.bounds, false);

        assert_eq!(thumbnail.dividers, dividers);
        assert!(!thumbnail.dividers.is_empty());
    }

    #[test]
    fn every_pane_stays_inside_the_thumbnail() {
        let layout = slide_time_below();
        let thumbnail = Thumbnail::of(&layout.root, 300.0, 120.0, AspectRatio::FourThree);
        for cell in &thumbnail.cells {
            assert!(cell.frame.x >= -0.01);
            assert!(cell.frame.y >= -0.01);
            assert!(cell.frame.x + cell.frame.width <= 300.01);
            assert!(cell.frame.y + cell.frame.height <= 120.01);
        }
    }

    #[test]
    fn only_slide_panes_carry_a_fitted_slide() {
        let layout = slide_notes_beside();
        let thumbnail = Thumbnail::of(&layout.root, 320.0, 180.0, AspectRatio::SixteenNine);
        for cell in &thumbnail.cells {
            let is_slide = cell
                .widget
                .is_some_and(|kind| kind.family() == Family::Slides);
            assert_eq!(
                cell.slide.is_some(),
                is_slide,
                "{:?} should {}carry a fitted slide",
                cell.widget,
                if is_slide { "" } else { "not " }
            );
        }
        assert_eq!(thumbnail.slide_cells().len(), 1);
    }

    #[test]
    fn the_fitted_slide_sits_inside_its_pane_and_is_centred() {
        let layout = slide_notes_beside();
        let thumbnail = Thumbnail::of(&layout.root, 320.0, 180.0, AspectRatio::FourThree);
        let cell = thumbnail.slide_cells()[0];
        let slide = cell.slide.expect("a slide pane");
        assert!(slide.drawn.width <= cell.frame.width + 0.01);
        assert!(slide.drawn.height <= cell.frame.height + 0.01);
        let left = slide.drawn.x - cell.frame.x;
        let right = (cell.frame.x + cell.frame.width) - (slide.drawn.x + slide.drawn.width);
        assert!((left - right).abs() < 0.01, "centred: {left} vs {right}");
    }

    #[test]
    fn the_same_layout_letterboxes_differently_for_different_decks() {
        // Slide + Notes Beside gives its slide a 4:3 pane on a 16:9 screen,
        // so a 4:3 deck fills it and a 16:9 deck does not.
        let layout = slide_notes_beside();
        let classic = Thumbnail::of(&layout.root, 1600.0, 900.0, AspectRatio::FourThree);
        let wide = Thumbnail::of(&layout.root, 1600.0, 900.0, AspectRatio::SixteenNine);
        let classic_waste = classic.slide_cells()[0].slide.unwrap().padding_fraction();
        assert!(classic_waste < 0.01, "a 4:3 deck all but fills it");
        assert!(wide.slide_cells()[0].has_letterbox());
    }

    #[test]
    fn an_empty_pane_sketches_nothing_and_has_no_label() {
        let layout = Layout::empty("Fresh");
        let thumbnail = Thumbnail::of(&layout.root, 200.0, 120.0, AspectRatio::SixteenNine);
        assert_eq!(thumbnail.cells.len(), 1);
        assert_eq!(thumbnail.cells[0].content, Content::Empty);
        assert_eq!(thumbnail.cells[0].label(), "");
        assert_eq!(thumbnail.cells[0].slide, None);
    }

    #[test]
    fn each_widget_family_gets_a_distinguishable_sketch() {
        assert_eq!(Content::of(WidgetKind::CurrentSlide), Content::Slide);
        assert_eq!(Content::of(WidgetKind::SpeakerNotes), Content::Lines(4));
        assert_eq!(Content::of(WidgetKind::Timer), Content::Readout);
        assert_eq!(Content::of(WidgetKind::SlideSlider), Content::Track);
        assert_eq!(Content::of(WidgetKind::SlideButtons), Content::Buttons(2));
        assert_eq!(Content::of(WidgetKind::PresentationTitle), Content::Caption);
        assert_eq!(Content::of(WidgetKind::Annotations), Content::Marks);
    }

    #[test]
    fn a_thumbnail_with_no_room_still_produces_panes_rather_than_panicking() {
        let layout = slide_time_below();
        let thumbnail = Thumbnail::of(&layout.root, 0.0, 0.0, AspectRatio::SixteenNine);
        assert_eq!(thumbnail.cells.len(), layout.cells().len());
        assert!(thumbnail.slide_cells().is_empty(), "nothing to fit");
    }

    #[test]
    fn slide_cells_finds_exactly_the_panes_that_show_slides() {
        let layout = crate::layout::builtin::slide_next_notes();
        let area = Frame::new(0.0, 0.0, 1600.0, 900.0);
        // The current slide and the next slide, and nothing else.
        assert_eq!(slide_cells(&layout.root, area).len(), 2);
        assert!(slide_cells(&Layout::empty("Fresh").root, area).is_empty());
    }
}
