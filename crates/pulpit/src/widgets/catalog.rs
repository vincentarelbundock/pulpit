//! Every widget, once.
//!
//! Label, tooltip, group, compound constituents, instance policy and base
//! minimum size were five separate `match` statements over `WidgetKind`. They
//! are now one table, so adding a widget is one registration rather than a
//! hunt for the arms somebody forgot.

use super::{WidgetGroup, WidgetKind};

/// The static facts about a widget kind.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// May this widget appear more than once in a layout?
    pub multi_instance: bool,
    /// Smallest size at which this widget is still usable, in layout points
    /// at scale 1. Below this the editor warns rather than saving something
    /// unusable.
    pub minimum_size: (f32, f32),
}

const SLIDE_PARTS: &[WidgetKind] = &[
    WidgetKind::PreviousSlide,
    WidgetKind::CurrentSlide,
    WidgetKind::NextSlide,
];
const NONE: &[WidgetKind] = &[];

/// The catalog. Order is the order of the library sidebar within each group.
pub const CATALOG: [WidgetDefinition; 23] = [
    WidgetDefinition {
        kind: WidgetKind::CurrentSlide,
        group: WidgetGroup::Slides,
        label: "Current Slide",
        short_label: "Current",
        tooltip: "What the audience is seeing right now.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (200.0, 120.0),
    },
    WidgetDefinition {
        kind: WidgetKind::PreviousSlide,
        group: WidgetGroup::Slides,
        label: "Previous Slide",
        short_label: "Prev",
        tooltip: "The slide you just came from.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (120.0, 80.0),
    },
    WidgetDefinition {
        kind: WidgetKind::NextSlide,
        group: WidgetGroup::Slides,
        label: "Next Slide",
        short_label: "Next",
        tooltip: "What comes next, so you can lead into it.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (120.0, 80.0),
    },
    WidgetDefinition {
        kind: WidgetKind::PreviousCurrentNext,
        group: WidgetGroup::Slides,
        label: "Previous + Current + Next",
        short_label: "P·C·N",
        tooltip: "All three previews as one strip, with the current slide dominant.",
        parts: SLIDE_PARTS,
        multi_instance: true,
        minimum_size: (420.0, 120.0),
    },
    WidgetDefinition {
        kind: WidgetKind::SpeakerNotes,
        group: WidgetGroup::PresenterInformation,
        label: "Speaker Notes",
        short_label: "Notes",
        tooltip: "Your talking points for the current slide.",
        parts: NONE,
        multi_instance: false,
        minimum_size: (260.0, 120.0),
    },
    WidgetDefinition {
        kind: WidgetKind::Timer,
        group: WidgetGroup::PresenterInformation,
        label: "Timer",
        short_label: "Timer",
        tooltip: "Elapsed time, counting up or down towards a target.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (140.0, 56.0),
    },
    WidgetDefinition {
        kind: WidgetKind::Clock,
        group: WidgetGroup::PresenterInformation,
        label: "Clock",
        short_label: "Clock",
        tooltip: "The wall clock, for keeping to a schedule.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (110.0, 48.0),
    },
    WidgetDefinition {
        kind: WidgetKind::SlideButtons,
        group: WidgetGroup::PresentationControls,
        label: "Back and Forward",
        short_label: "Buttons",
        tooltip: "The two buttons you press to move through the deck.",
        parts: NONE,
        multi_instance: false,
        // Two hit targets side by side and nothing else.
        minimum_size: (150.0, 40.0),
    },
    WidgetDefinition {
        kind: WidgetKind::SlideSlider,
        group: WidgetGroup::PresentationControls,
        label: "Slide Slider",
        short_label: "Slider",
        tooltip: "Scrub through the deck; the audience follows on release.",
        parts: NONE,
        multi_instance: false,
        minimum_size: (180.0, 32.0),
    },
    WidgetDefinition {
        kind: WidgetKind::SlideCounter,
        group: WidgetGroup::PresentationControls,
        label: "Slide Counter",
        short_label: "Count",
        tooltip: "Position in the deck, as “12 / 40”.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (90.0, 28.0),
    },
    WidgetDefinition {
        kind: WidgetKind::PauseResume,
        group: WidgetGroup::PresentationControls,
        label: "Pause or Resume",
        short_label: "Pause",
        tooltip: "Pause or resume the timer without touching the slides.",
        parts: NONE,
        multi_instance: false,
        minimum_size: (120.0, 40.0),
    },
    WidgetDefinition {
        kind: WidgetKind::EndPresentation,
        group: WidgetGroup::PresentationControls,
        label: "End Presentation",
        short_label: "End",
        tooltip: "Leave presentation mode and release the projector.",
        parts: NONE,
        multi_instance: false,
        minimum_size: (120.0, 40.0),
    },
    WidgetDefinition {
        kind: WidgetKind::Annotations,
        group: WidgetGroup::PresentationControls,
        label: "Annotations",
        short_label: "Annotate",
        tooltip: "Draw, highlight or erase marks on the slide; marks go when the slide does.",
        parts: NONE,
        // One palette: two would show two different armed tools, and only
        // one of them could be telling the truth.
        multi_instance: false,
        // Eight controls in one row; they grow into whatever height the cell
        // has, so the minimum is the smallest button still worth pressing.
        // Option panels are overlays and cost the cell nothing.
        minimum_size: (300.0, 26.0),
    },
    WidgetDefinition {
        kind: WidgetKind::MediaTransport,
        group: WidgetGroup::PresentationControls,
        label: "Media Transport",
        short_label: "Media",
        tooltip: "Play, pause and scrub the video or animation on this slide.",
        parts: NONE,
        // One transport: it always means the media on the current slide, so a
        // second copy would be the same controls twice.
        multi_instance: false,
        // A button, a scrub bar wide enough to be worth dragging, and a
        // time readout in one row.
        minimum_size: (260.0, 34.0),
    },
    WidgetDefinition {
        kind: WidgetKind::PresentationTitle,
        group: WidgetGroup::OptionalInformation,
        label: "Presentation Title",
        short_label: "Title",
        tooltip: "The document title, useful on a shared machine.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (160.0, 28.0),
    },
    WidgetDefinition {
        kind: WidgetKind::CurrentSection,
        group: WidgetGroup::OptionalInformation,
        label: "Current Section",
        short_label: "Section",
        tooltip: "The section the current slide belongs to.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (140.0, 24.0),
    },
    WidgetDefinition {
        kind: WidgetKind::AudienceScreenStatus,
        group: WidgetGroup::OptionalInformation,
        label: "Audience Screen Status",
        short_label: "Audience",
        tooltip: "Whether the audience output is live or blanked.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (140.0, 24.0),
    },
    WidgetDefinition {
        kind: WidgetKind::ConnectionStatus,
        group: WidgetGroup::OptionalInformation,
        label: "Connection Status",
        short_label: "Connection",
        tooltip: "Whether a separate audience display is connected.",
        parts: NONE,
        multi_instance: true,
        minimum_size: (140.0, 24.0),
    },
    WidgetDefinition {
        kind: WidgetKind::DocumentPage,
        group: WidgetGroup::Document,
        label: "Page",
        short_label: "Page",
        tooltip: "The document itself: scroll it, zoom it, mark it and fill it in.",
        parts: NONE,
        // One page surface. Two would be two scroll positions in one document,
        // and the marks would be going into whichever one had the pointer.
        multi_instance: false,
        // A page is the artifact; below this there is not enough of it left to
        // read a line of it.
        minimum_size: (240.0, 240.0),
    },
    WidgetDefinition {
        kind: WidgetKind::DocumentNav,
        group: WidgetGroup::Document,
        label: "Page Navigation",
        short_label: "Pages",
        tooltip: "Which page you are on, and the zoom: fit the width, fit the page, or set it.",
        parts: NONE,
        multi_instance: false,
        // A counter, a page box and four controls in one row.
        minimum_size: (280.0, 32.0),
    },
    WidgetDefinition {
        kind: WidgetKind::DocumentOutline,
        group: WidgetGroup::Document,
        label: "Outline",
        short_label: "Outline",
        tooltip: "The document's bookmarks and page thumbnails, for getting somewhere quickly.",
        parts: NONE,
        multi_instance: false,
        // Narrow, because it holds page numbers and bookmark titles rather
        // than prose; every point past that is a point the page is not getting.
        minimum_size: (150.0, 160.0),
    },
    WidgetDefinition {
        kind: WidgetKind::FormFields,
        group: WidgetGroup::Document,
        label: "Form Fields",
        short_label: "Fields",
        tooltip: "Every field in the document, and what it currently holds.",
        parts: NONE,
        multi_instance: false,
        // A field name above its editor, at a width a value is readable at.
        minimum_size: (220.0, 160.0),
    },
    WidgetDefinition {
        kind: WidgetKind::AnnotationTools,
        group: WidgetGroup::Document,
        label: "Annotation Tools",
        short_label: "Tools",
        tooltip: "Choose what a press does to the page: draw, highlight, type, note or erase.",
        parts: NONE,
        // One toolbar, for the same reason as the presenter palette: two would
        // show two armed tools and only one of them could be telling the truth.
        multi_instance: false,
        minimum_size: (300.0, 26.0),
    },
];

/// The definition of a kind. Total by construction, and proved so by the
/// tests below.
pub fn definition(kind: WidgetKind) -> &'static WidgetDefinition {
    CATALOG
        .iter()
        .find(|definition| definition.kind == kind)
        .expect("every WidgetKind is registered; the catalog tests prove it")
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
            .filter(|definition| !definition.multi_instance)
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
                WidgetKind::DocumentPage,
                WidgetKind::DocumentNav,
                WidgetKind::DocumentOutline,
                WidgetKind::FormFields,
                WidgetKind::AnnotationTools,
            ]
        );
    }

    #[test]
    fn the_annotation_palette_is_registered_as_a_presentation_control() {
        let definition = definition(WidgetKind::Annotations);
        assert_eq!(definition.group, WidgetGroup::PresentationControls);
        assert!(definition.parts.is_empty(), "the palette is not compound");
        assert!(
            !definition.multi_instance,
            "two palettes could disagree about which tool is armed"
        );
        let (width, height) = definition.minimum_size;
        assert!(
            width >= 260.0 && (24.0..=32.0).contains(&height),
            "the palette is one row of controls that grows into its cell"
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
}
