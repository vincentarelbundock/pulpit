//! Every widget, once.
//!
//! Label, tooltip, group, compound constituents, instance policy and base
//! minimum size were five separate `match` statements over `WidgetKind`. They
//! are now one table, so adding a widget is one registration rather than a
//! hunt for the arms somebody forgot.

use super::plan::WidgetPlan;
use super::{Widget, WidgetCapability, WidgetGroup, WidgetKind};
use std::num::NonZeroUsize;

/// A sketch of one pane's contents, chosen so a glance distinguishes a notes
/// pane from a clock from a row of buttons.
///
/// Deliberately coarse: a thumbnail that tried to be a screenshot would be
/// unreadable at this size, and would go stale the moment a widget changed.
/// Lives here rather than in `layout::thumbnail` because it is a catalog
/// fact — what a kind looks like — not a layout decision; that module reads
/// [`WidgetDefinition::thumbnail`] instead of matching on [`WidgetKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailContent {
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

/// How many times a widget may appear in one layout.
///
/// Deliberately minimal: this answers "how many", not "which cell claims
/// it" — a shared/exclusive resource-claims system is a different, larger
/// feature and out of scope here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementPolicy {
    /// `None` means unlimited.
    pub max_instances: Option<NonZeroUsize>,
}

impl PlacementPolicy {
    pub const UNLIMITED: PlacementPolicy = PlacementPolicy {
        max_instances: None,
    };

    pub const fn single() -> PlacementPolicy {
        PlacementPolicy {
            max_instances: NonZeroUsize::new(1),
        }
    }

    /// Is this many instances too many?
    pub fn exceeded_by(self, count: usize) -> bool {
        match self.max_instances {
            Some(max) => count > max.get(),
            None => false,
        }
    }
}

/// The static facts about a widget kind.
// No `PartialEq`: a `plan` function pointer's address is not a meaningful
// comparison (`unpredictable_function_pointer_comparisons`), and nothing
// needs whole-struct equality — the tests below compare individual fields.
#[derive(Debug, Clone, Copy)]
pub struct WidgetDefinition {
    pub kind: WidgetKind,
    pub group: WidgetGroup,
    pub label: &'static str,
    /// The short form used inside cells and thumbnails.
    pub short_label: &'static str,
    pub tooltip: &'static str,
    /// What a compound widget counts as. Empty for simple widgets.
    ///
    /// This is what makes "placing Slide Navigation satisfies a rule that
    /// wants forward navigation" and "Slide Navigation and Slide Slider are
    /// mutually exclusive" fall out of one mechanism instead of two.
    pub parts: &'static [WidgetKind],
    /// How many times this widget may appear in one layout.
    pub placement: PlacementPolicy,
    /// What this widget does, on its own — not counting what its `parts`
    /// contribute. [`WidgetKind::capabilities`] folds those in.
    pub capabilities: &'static [WidgetCapability],
    /// Smallest size at which this widget is still usable, in layout points
    /// at scale 1. Below this the editor warns rather than saving something
    /// unusable.
    pub minimum_size: (f32, f32),
    /// What a thumbnail should sketch for this kind. See [`ThumbnailContent`].
    pub thumbnail: ThumbnailContent,
    /// What frames this kind needs rendered, given its placed widget and its
    /// cell's share of the window's width. See [`super::plan`].
    ///
    /// Folded in here rather than kept in a second, `registry.rs`-owned
    /// table (§81): the two tables were both a total, per-kind map with
    /// nothing distinguishing which one a given fact belonged in.
    pub plan: fn(&Widget, f32) -> WidgetPlan,
}

impl WidgetDefinition {
    /// Thin wrapper kept for existing callers; `placement` is the real
    /// policy and knows more than yes/no.
    pub fn multi_instance(&self) -> bool {
        self.placement.max_instances.is_none()
    }
}

/// A slide panel's plan hook, bound to its own kind so one function works for
/// `CurrentSlide`, `PreviousSlide` and `NextSlide` alike.
fn plan_current_slide(widget: &Widget, cell_width: f32) -> WidgetPlan {
    super::plan::slide_panel(widget.kind(), cell_width)
}

fn plan_previous_current_next(_widget: &Widget, cell_width: f32) -> WidgetPlan {
    super::plan::previous_current_next(cell_width)
}

const SLIDE_PARTS: &[WidgetKind] = &[
    WidgetKind::PreviousSlide,
    WidgetKind::CurrentSlide,
    WidgetKind::NextSlide,
];
const NONE: &[WidgetKind] = &[];
const NO_CAPS: &[WidgetCapability] = &[];

/// The catalog. Order is the order of the library sidebar within each group.
pub const CATALOG: [WidgetDefinition; 26] = [
    WidgetDefinition {
        kind: WidgetKind::CurrentSlide,
        plan: plan_current_slide,
        group: WidgetGroup::Slides,
        label: "Current Slide",
        short_label: "Current",
        tooltip: "What the audience is seeing right now.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: &[WidgetCapability::ShowsCurrentSlide],
        minimum_size: (200.0, 120.0),
        thumbnail: ThumbnailContent::Slide,
    },
    WidgetDefinition {
        kind: WidgetKind::PreviousSlide,
        plan: plan_current_slide,
        group: WidgetGroup::Slides,
        label: "Previous Slide",
        short_label: "Prev",
        tooltip: "The slide you just came from.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (120.0, 80.0),
        thumbnail: ThumbnailContent::Slide,
    },
    WidgetDefinition {
        kind: WidgetKind::NextSlide,
        plan: plan_current_slide,
        group: WidgetGroup::Slides,
        label: "Next Slide",
        short_label: "Next",
        tooltip: "What comes next, so you can lead into it.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (120.0, 80.0),
        thumbnail: ThumbnailContent::Slide,
    },
    WidgetDefinition {
        kind: WidgetKind::PreviousCurrentNext,
        plan: plan_previous_current_next,
        group: WidgetGroup::Slides,
        label: "Previous + Current + Next",
        short_label: "P·C·N",
        tooltip: "All three previews as one strip, with the current slide dominant.",
        parts: SLIDE_PARTS,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (420.0, 120.0),
        thumbnail: ThumbnailContent::Slide,
    },
    WidgetDefinition {
        kind: WidgetKind::SpeakerNotes,
        plan: super::plan::none,
        group: WidgetGroup::PresenterInformation,
        label: "Speaker Notes",
        short_label: "Notes",
        tooltip: "Your talking points for the current slide.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: NO_CAPS,
        minimum_size: (260.0, 120.0),
        thumbnail: ThumbnailContent::Lines(4),
    },
    WidgetDefinition {
        kind: WidgetKind::Timer,
        plan: super::plan::none,
        group: WidgetGroup::PresenterInformation,
        label: "Timer",
        short_label: "Timer",
        tooltip: "Elapsed time, counting up or down towards a target.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (140.0, 56.0),
        thumbnail: ThumbnailContent::Readout,
    },
    WidgetDefinition {
        kind: WidgetKind::Clock,
        plan: super::plan::none,
        group: WidgetGroup::PresenterInformation,
        label: "Clock",
        short_label: "Clock",
        tooltip: "The wall clock, for keeping to a schedule.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (110.0, 48.0),
        thumbnail: ThumbnailContent::Readout,
    },
    WidgetDefinition {
        kind: WidgetKind::SlideButtons,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Back and Forward",
        short_label: "Buttons",
        tooltip: "The two buttons you press to move through the deck.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::NavigatesForward, WidgetCapability::NavigatesBackward],
        // Two hit targets side by side and nothing else.
        minimum_size: (150.0, 40.0),
        thumbnail: ThumbnailContent::Buttons(2),
    },
    WidgetDefinition {
        kind: WidgetKind::SlideSlider,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Slide Slider",
        short_label: "Slider",
        tooltip: "Scrub through the deck; the audience follows on release.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::NavigatesForward, WidgetCapability::NavigatesBackward],
        minimum_size: (180.0, 32.0),
        thumbnail: ThumbnailContent::Track,
    },
    WidgetDefinition {
        kind: WidgetKind::SlideCounter,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Slide Counter",
        short_label: "Count",
        tooltip: "Position in the deck, as “12 / 40”.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (90.0, 28.0),
        thumbnail: ThumbnailContent::Readout,
    },
    WidgetDefinition {
        kind: WidgetKind::PauseResume,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Pause or Resume",
        short_label: "Pause",
        tooltip: "Pause or resume the timer without touching the slides.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::ControlsTimer],
        minimum_size: (120.0, 40.0),
        thumbnail: ThumbnailContent::Buttons(1),
    },
    WidgetDefinition {
        kind: WidgetKind::EndPresentation,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "End Presentation",
        short_label: "End",
        tooltip: "Leave presentation mode and release the projector.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: NO_CAPS,
        minimum_size: (120.0, 40.0),
        thumbnail: ThumbnailContent::Buttons(1),
    },
    WidgetDefinition {
        kind: WidgetKind::Annotations,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Annotations",
        short_label: "Annotate",
        tooltip: "Draw, highlight or erase marks on the slide; marks go when the slide does.",
        parts: NONE,
        // One palette: two would show two different armed tools, and only
        // one of them could be telling the truth.
        placement: PlacementPolicy::single(),
        capabilities: NO_CAPS,
        // The responsive row keeps the primary tools and moves every control
        // that does not fit into a labelled overflow menu. At 160pt it keeps
        // several direct tools plus that menu; requiring the full expanded
        // row would make the contract describe a preferred width as a minimum.
        minimum_size: (160.0, 32.0),
        thumbnail: ThumbnailContent::Marks,
    },
    WidgetDefinition {
        kind: WidgetKind::MediaTransport,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Media Transport",
        short_label: "Media",
        tooltip: "Play, pause and scrub the video or animation on this slide.",
        parts: NONE,
        // One transport: it always means the media on the current slide, so a
        // second copy would be the same controls twice.
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::ControlsMedia],
        // A button, a scrub bar wide enough to be worth dragging, and a
        // time readout in one row.
        minimum_size: (260.0, 34.0),
        thumbnail: ThumbnailContent::Track,
    },
    WidgetDefinition {
        kind: WidgetKind::MainMenu,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Menu",
        short_label: "Menu",
        tooltip: "The way in to opening a deck, the layouts, the settings and the rest.",
        parts: NONE,
        // One way in. Two hamburgers would be two buttons opening the same
        // menu, and the open one would have to be told which was pressed.
        placement: PlacementPolicy::single(),
        capabilities: NO_CAPS,
        // One square toolbar button and nothing else. Its height matches the
        // document controls that share the built-in Reader's top band.
        minimum_size: (32.0, 32.0),
        thumbnail: ThumbnailContent::Buttons(1),
    },
    WidgetDefinition {
        kind: WidgetKind::AudienceControls,
        plan: super::plan::none,
        group: WidgetGroup::PresentationControls,
        label: "Start and Stop",
        short_label: "Start",
        tooltip: "Open the audience window on a display, and close it again.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::ControlsAudience],
        // Start, its display arrow, and Stop side by side.
        minimum_size: (230.0, 40.0),
        thumbnail: ThumbnailContent::Buttons(2),
    },
    WidgetDefinition {
        kind: WidgetKind::PresentationTitle,
        plan: super::plan::none,
        group: WidgetGroup::OptionalInformation,
        label: "Presentation Title",
        short_label: "Title",
        tooltip: "The document title, useful on a shared machine.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (160.0, 28.0),
        thumbnail: ThumbnailContent::Caption,
    },
    WidgetDefinition {
        kind: WidgetKind::CurrentSection,
        plan: super::plan::none,
        group: WidgetGroup::OptionalInformation,
        label: "Current Section",
        short_label: "Section",
        tooltip: "The section the current slide belongs to.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (140.0, 24.0),
        thumbnail: ThumbnailContent::Caption,
    },
    WidgetDefinition {
        kind: WidgetKind::AudienceScreenStatus,
        plan: super::plan::none,
        group: WidgetGroup::OptionalInformation,
        label: "Audience Screen Status",
        short_label: "Audience",
        tooltip: "Whether the audience output is live or blanked.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (140.0, 24.0),
        thumbnail: ThumbnailContent::Caption,
    },
    WidgetDefinition {
        kind: WidgetKind::ConnectionStatus,
        plan: super::plan::none,
        group: WidgetGroup::OptionalInformation,
        label: "Connection Status",
        short_label: "Connection",
        tooltip: "Whether a separate audience display is connected.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        minimum_size: (140.0, 24.0),
        thumbnail: ThumbnailContent::Caption,
    },
    WidgetDefinition {
        kind: WidgetKind::BlankSpace,
        plan: super::plan::none,
        group: WidgetGroup::OptionalInformation,
        label: "Blank Space",
        short_label: "Blank",
        tooltip: "An empty, themed panel — a deliberate gap, not a mistake.",
        parts: NONE,
        placement: PlacementPolicy::UNLIMITED,
        capabilities: NO_CAPS,
        // A sliver still worth drawing a panel around.
        minimum_size: (40.0, 24.0),
        thumbnail: ThumbnailContent::Empty,
    },
    WidgetDefinition {
        kind: WidgetKind::DocumentPage,
        plan: super::plan::document_page,
        group: WidgetGroup::Document,
        label: "Page",
        short_label: "Page",
        tooltip: "The document itself: scroll it, zoom it, mark it and fill it in.",
        parts: NONE,
        // One page surface. Two would be two scroll positions in one document,
        // and the marks would be going into whichever one had the pointer.
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::ShowsDocument],
        // A page is the artifact; below this there is not enough of it left to
        // read a line of it.
        minimum_size: (240.0, 240.0),
        thumbnail: ThumbnailContent::Slide,
    },
    WidgetDefinition {
        kind: WidgetKind::DocumentNav,
        plan: super::plan::none,
        group: WidgetGroup::Document,
        label: "Page Navigation",
        short_label: "Pages",
        tooltip: "Which page you are on, and the zoom: fit the width, fit the page, set it, or crop to a rectangle.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::ShowsDocument],
        // A counter, a page box and four controls in one row.
        minimum_size: (280.0, 32.0),
        thumbnail: ThumbnailContent::Buttons(2),
    },
    WidgetDefinition {
        kind: WidgetKind::DocumentOutline,
        plan: super::plan::none,
        group: WidgetGroup::Document,
        label: "Bookmarks",
        short_label: "Bookmarks",
        tooltip: "The document's bookmarks, for getting somewhere quickly.",
        parts: NONE,
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::ShowsDocument],
        // Narrow, because it holds page numbers and bookmark titles rather
        // than prose; every point past that is a point the page is not getting.
        minimum_size: (150.0, 160.0),
        thumbnail: ThumbnailContent::Lines(5),
    },
    WidgetDefinition {
        kind: WidgetKind::AnnotationTools,
        plan: super::plan::none,
        group: WidgetGroup::Document,
        label: "Annotation Tools",
        short_label: "Tools",
        tooltip: "Choose what a press does to the page: draw, highlight, type, note or erase.",
        parts: NONE,
        // One toolbar, for the same reason as the presenter palette: two would
        // show two armed tools and only one of them could be telling the truth.
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::ShowsDocument],
        // The compact row keeps the hand, the armed tool and labelled More
        // access visible. The built-in Reader hugs this functional compact
        // width; wider custom cells still reveal the full run responsively.
        minimum_size: (144.0, 32.0),
        thumbnail: ThumbnailContent::Marks,
    },
    WidgetDefinition {
        kind: WidgetKind::Search,
        plan: super::plan::none,
        group: WidgetGroup::Document,
        label: "Search",
        short_label: "Find",
        tooltip: "Find a string in the pages, the speaker notes and the bookmarks.",
        parts: NONE,
        // One search, for the same reason as one toolbar: two boxes over one
        // model would show two queries and only one of them would be running.
        placement: PlacementPolicy::single(),
        capabilities: &[WidgetCapability::SearchesDocument],
        // A query field with its stepper beside it, and room for a few results
        // beneath: less than this and the list is a single cut-off row.
        minimum_size: (260.0, 120.0),
        thumbnail: ThumbnailContent::Lines(3),
    },
];

/// `CATALOG`'s position for each `WidgetKind`, computed once so
/// [`definition`] is an array index rather than a linear scan run from
/// `Widget::minimum_size` on every frame (§81, §82.8-adjacent). `CATALOG`'s
/// own order is the library sidebar's, grouped by [`WidgetGroup`], so this
/// is the permutation between that order and `kind as usize`.
///
/// Every slot is written exactly once: `every_kind_is_registered_exactly_once`
/// below proves `CATALOG` has one entry per `WidgetKind`, which is what
/// makes this total rather than leaving an untouched (and wrongly aliased)
/// slot behind.
const INDEX_BY_KIND: [usize; WidgetKind::ALL.len()] = {
    let mut table = [0usize; WidgetKind::ALL.len()];
    let mut position = 0;
    while position < CATALOG.len() {
        table[CATALOG[position].kind as usize] = position;
        position += 1;
    }
    table
};

/// The definition of a kind. Total by construction, and proved so by the
/// tests below.
pub fn definition(kind: WidgetKind) -> &'static WidgetDefinition {
    &CATALOG[INDEX_BY_KIND[kind as usize]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_is_registered_exactly_once() {
        for kind in WidgetKind::ALL {
            let matches = CATALOG
                .iter()
                .filter(|definition| definition.kind == kind)
                .count();
            assert_eq!(matches, 1, "{kind:?} is registered {matches} times");
        }
        assert_eq!(
            CATALOG.len(),
            WidgetKind::ALL.len(),
            "the catalog and WidgetKind::ALL describe the same set"
        );
    }

    #[test]
    fn definition_finds_the_matching_entry_for_every_kind() {
        // §81: `definition` indexes `CATALOG` through `INDEX_BY_KIND` rather
        // than scanning it, which only proves anything once each kind's
        // slot is checked to point back at itself.
        for kind in WidgetKind::ALL {
            assert_eq!(definition(kind).kind, kind);
        }
    }

    #[test]
    fn every_definition_says_something() {
        for definition in CATALOG {
            assert!(!definition.label.is_empty(), "{:?}", definition.kind);
            assert!(!definition.short_label.is_empty(), "{:?}", definition.kind);
            assert!(
                definition.tooltip.len() > 20,
                "{:?} needs a real tooltip",
                definition.kind
            );
            let (width, height) = definition.minimum_size;
            assert!(width > 0.0 && height > 0.0, "{:?}", definition.kind);
        }
    }

    #[test]
    fn compounds_refer_only_to_registered_widgets() {
        for definition in CATALOG {
            for part in definition.parts {
                assert!(
                    CATALOG.iter().any(|other| other.kind == *part),
                    "{:?} contains unregistered {part:?}",
                    definition.kind
                );
            }
        }
    }

    #[test]
    fn no_compound_contains_itself_however_indirectly() {
        fn reaches(from: WidgetKind, target: WidgetKind, depth: usize) -> bool {
            assert!(depth < 8, "the compound graph has a cycle at {from:?}");
            definition(from)
                .parts
                .iter()
                .any(|part| *part == target || reaches(*part, target, depth + 1))
        }
        for definition in CATALOG {
            assert!(
                !reaches(definition.kind, definition.kind, 0),
                "{:?} contains itself",
                definition.kind
            );
        }
    }

    #[test]
    fn single_instance_widgets_are_exactly_the_documented_ones() {
        let single: Vec<WidgetKind> = CATALOG
            .iter()
            .filter(|definition| definition.placement.max_instances.is_some())
            .map(|definition| definition.kind)
            .collect();
        assert_eq!(
            single,
            vec![
                WidgetKind::SpeakerNotes,
                WidgetKind::SlideButtons,
                WidgetKind::SlideSlider,
                WidgetKind::PauseResume,
                WidgetKind::EndPresentation,
                WidgetKind::Annotations,
                WidgetKind::MediaTransport,
                WidgetKind::MainMenu,
                WidgetKind::AudienceControls,
                WidgetKind::DocumentPage,
                WidgetKind::DocumentNav,
                WidgetKind::DocumentOutline,
                WidgetKind::AnnotationTools,
                WidgetKind::Search,
            ]
        );
    }

    #[test]
    fn the_annotation_palette_is_registered_as_a_presentation_control() {
        let definition = definition(WidgetKind::Annotations);
        assert_eq!(definition.group, WidgetGroup::PresentationControls);
        assert!(definition.parts.is_empty(), "the palette is not compound");
        assert_eq!(
            definition.placement.max_instances,
            std::num::NonZeroUsize::new(1),
            "two palettes could disagree about which tool is armed"
        );
        let (width, height) = definition.minimum_size;
        assert!(
            width >= 160.0 && height >= crate::theme::target::MINIMUM,
            "the palette keeps usable targets and moves the rest into overflow"
        );
    }

    #[test]
    fn the_catalog_is_grouped_in_sidebar_order() {
        // The library draws groups in `WidgetGroup::ALL` order and the widgets
        // within each in catalog order; a group split in two would draw the
        // same heading twice.
        let mut seen: Vec<WidgetGroup> = Vec::new();
        for definition in CATALOG {
            if seen.last() != Some(&definition.group) {
                assert!(
                    !seen.contains(&definition.group),
                    "{:?} appears in two runs",
                    definition.group
                );
                seen.push(definition.group);
            }
        }
    }

    /// The capability table has to reproduce exactly what the old
    /// hand-written predicates said, for every kind — this is the port of
    /// those predicates the table is checked against, not a second copy of
    /// the table itself.
    #[test]
    fn capabilities_reproduce_the_old_predicates_for_every_kind() {
        fn old_shows_current_slide(kind: WidgetKind) -> bool {
            kind.occupies().contains(&WidgetKind::CurrentSlide)
        }
        fn old_family_is_document(kind: WidgetKind) -> bool {
            matches!(
                kind,
                WidgetKind::DocumentPage
                    | WidgetKind::DocumentNav
                    | WidgetKind::DocumentOutline
                    | WidgetKind::AnnotationTools
            )
        }
        // `provides_forward_navigation`/`provides_backward_navigation` also
        // read widget *configuration*; capability only answers the static
        // half — "can this kind be configured to do it at all".
        fn old_can_navigate_forward(kind: WidgetKind) -> bool {
            matches!(kind, WidgetKind::SlideButtons | WidgetKind::SlideSlider)
        }
        fn old_can_navigate_backward(kind: WidgetKind) -> bool {
            matches!(kind, WidgetKind::SlideButtons | WidgetKind::SlideSlider)
        }

        for kind in WidgetKind::ALL {
            assert_eq!(
                kind.has_capability(WidgetCapability::ShowsCurrentSlide),
                old_shows_current_slide(kind),
                "{kind:?}: ShowsCurrentSlide"
            );
            assert_eq!(
                kind.has_capability(WidgetCapability::ShowsDocument),
                old_family_is_document(kind),
                "{kind:?}: ShowsDocument"
            );
            assert_eq!(
                kind.has_capability(WidgetCapability::NavigatesForward),
                old_can_navigate_forward(kind),
                "{kind:?}: NavigatesForward"
            );
            assert_eq!(
                kind.has_capability(WidgetCapability::NavigatesBackward),
                old_can_navigate_backward(kind),
                "{kind:?}: NavigatesBackward"
            );
        }
    }
}
