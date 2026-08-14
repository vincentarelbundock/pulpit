//! The immutable built-in layouts, led by the one a first run opens with.
//!
//! These are the layouts a first-time user meets, and the reference for what
//! a good presenter screen looks like: strong hierarchy, readable at a
//! glance, large controls, restrained colour.

use crate::layout::model::{AspectRatio, Layout, LayoutId, Origin};
use crate::layout::tree::{Cell, CellBackground, Direction, Node, NodeId, Split};
use crate::widgets::{Widget, WidgetKind};

/// Builder state: hands out ids while a tree is assembled.
struct Builder {
    next: u32,
}

impl Builder {
    fn new() -> Self {
        Self { next: 0 }
    }

    fn id(&mut self) -> NodeId {
        let id = NodeId(self.next);
        self.next += 1;
        id
    }

    fn cell(&mut self, kind: WidgetKind) -> Node {
        Node::Leaf(Cell::with_widget(self.id(), Widget::new(kind)))
    }

    /// A cell holding a widget with adjusted style.
    /// A cell holding a widget with adjusted family configuration.
    fn configured(
        &mut self,
        kind: WidgetKind,
        tune: impl FnOnce(&mut crate::widgets::WidgetConfig),
    ) -> Node {
        let mut widget = Widget::new(kind);
        tune(widget.config_mut());
        widget.sanitise();
        Node::Leaf(Cell::with_widget(self.id(), widget))
    }

    /// A slide cell with nothing behind it.
    ///
    /// A slide keeps the document's aspect ratio, so it rarely fills its cell
    /// exactly. Painting the cell light turns that leftover space into a wide
    /// grey mount around the page; leaving it dark lets the slide itself be
    /// the only bright thing, which is also how it will look on the wall.
    fn slide(&mut self, kind: WidgetKind) -> Node {
        let widget = Widget::new(kind);
        let mut cell = Cell::with_widget(self.id(), widget);
        cell.background = CellBackground::None;
        cell.padding = 0.0;
        Node::Leaf(cell)
    }

    /// A presenter-tool cell: a dark panel separated by its split's gutter.
    fn panel(&mut self, kind: WidgetKind) -> Node {
        let mut cell = Cell::with_widget(self.id(), Widget::new(kind));
        cell.background = CellBackground::Panel;
        cell.padding = 12.0;
        Node::Leaf(cell)
    }

    fn split(
        &mut self,
        name: &str,
        direction: Direction,
        sizes: &[f32],
        children: Vec<Node>,
    ) -> Node {
        assert_eq!(sizes.len(), children.len(), "one size per child");
        Node::Split(Split {
            id: self.id(),
            name: Some(name.to_string()),
            direction,
            children,
            sizes: sizes.to_vec(),
            gap: crate::widgets::tokens::SPLIT_GAP,
            min_child: 0.05,
        })
    }
}

fn finish(name: &str, id: &str, root: Node) -> Layout {
    let mut layout = Layout::from_parts(
        LayoutId(id.to_string()),
        name.to_string(),
        Origin::BuiltIn,
        AspectRatio::SixteenNine,
        root,
    );
    layout.renumber();
    debug_assert!(layout.is_canonical(), "{name} is not canonical");
    layout
}

/// **Presenter Default** — the layout a first run opens with. Most of the
/// width is the live slide; the remaining rail stacks the two readings (clock
/// and timer side by side), the next slide, and the notes; a shallow band
/// along the bottom carries the controls.
///
/// The rail is a little over a quarter of the width, which is what the notes
/// want: text set in a narrower column than this wraps every few words. The
/// readings above them take the shallowest band their digits will fit in,
/// because that height is otherwise the notes'.
///
/// Every proportion here is a round fraction, so the layout reads the same at
/// any display size and stays easy to reason about when edited.
pub fn presenter_default() -> Layout {
    let mut b = Builder::new();

    let reading_children = vec![b.panel(WidgetKind::Clock), b.panel(WidgetKind::Timer)];
    let readings = b.split("Time", Direction::Horizontal, &[0.5, 0.5], reading_children);
    let rail_children = vec![
        readings,
        b.slide(WidgetKind::NextSlide),
        b.panel(WidgetKind::SpeakerNotes),
    ];
    // Notes take half the rail, the next slide most of the rest, and the
    // clock and timer the shallow band their digits and one line need — a
    // reading is read at a glance, and height past the digits is height the
    // notes are not getting.
    let rail = b.split(
        "Look-ahead rail",
        Direction::Vertical,
        &[0.16, 0.32, 0.52],
        rail_children,
    );

    let stage_children = vec![b.slide(WidgetKind::CurrentSlide), rail];
    let stage = b.split(
        "Stage",
        Direction::Horizontal,
        &[0.72, 0.28],
        stage_children,
    );

    let third = 1.0 / 3.0;
    let control_children = vec![
        b.panel(WidgetKind::SlideSlider),
        b.panel(WidgetKind::SlideButtons),
        b.panel(WidgetKind::Annotations),
    ];
    let controls = b.split(
        "Navigation and tools",
        Direction::Horizontal,
        &[third, third, third],
        control_children,
    );

    // The controls are a row of buttons and a slider; they need the height of
    // a button and no more, and every point past that is taken from the slide.
    let root = b.split(
        "Presenter screen",
        Direction::Vertical,
        &[0.92, 0.08],
        vec![stage, controls],
    );
    finish("Presenter Default", "presenter-default", root)
}

/// **Slide + Next + Notes** — a slide-first alternative. The current slide gets
/// the full height and nearly three quarters of the width; the information a
/// presenter looks ahead to lives in a narrow rail.
pub fn slide_next_notes() -> Layout {
    let mut b = Builder::new();

    let rail_children = vec![
        b.slide(WidgetKind::NextSlide),
        b.panel(WidgetKind::SpeakerNotes),
        b.panel(WidgetKind::Annotations),
        b.panel(WidgetKind::SlideSlider),
        b.panel(WidgetKind::SlideButtons),
    ];
    let rail = b.split(
        "Look-ahead rail",
        Direction::Vertical,
        // The next slide and the notes share the rail equally; the palette,
        // the scrubber and the buttons take only the height their controls
        // need, so the reading matter keeps the rest.
        &[0.385, 0.385, 0.06, 0.07, 0.10],
        rail_children,
    );

    let root_children = vec![b.slide(WidgetKind::CurrentSlide), rail];
    let root = b.split(
        "Presenter screen",
        Direction::Horizontal,
        &[0.72, 0.28],
        root_children,
    );
    finish("Slide + Next + Notes", "slide-next-notes", root)
}

/// **Slide + Notes Beside** — a full-height slide with notes and compact
/// navigation in a 25% side rail. On a 16:9 presenter display the 75/25 split
/// gives a 4:3 deck an exact, padding-free slide region.
pub fn slide_notes_beside() -> Layout {
    let mut b = Builder::new();

    let rail_children = vec![
        b.panel(WidgetKind::SpeakerNotes),
        b.panel(WidgetKind::SlideSlider),
        b.panel(WidgetKind::SlideButtons),
    ];
    let rail = b.split(
        "Notes rail",
        Direction::Vertical,
        &[0.77, 0.07, 0.16],
        rail_children,
    );

    let root_children = vec![b.slide(WidgetKind::CurrentSlide), rail];
    let root = b.split(
        "Presenter screen",
        Direction::Horizontal,
        &[0.75, 0.25],
        root_children,
    );
    finish("Slide + Notes Beside", "slide-notes-beside", root)
}

/// **Slide + Time Below** — only the current slide and a shallow timing strip.
/// The 90/10 split exactly fits a 16:9 deck above the otherwise unused strip
/// on a 16:10 presenter display.
pub fn slide_time_below() -> Layout {
    let mut b = Builder::new();

    // Four widgets share the strip end to end. The timer is given more room
    // than the clock, being the reading that is glanced at; the buttons get
    // the largest share, being what you press without looking.
    let timer = b.panel(WidgetKind::Timer);
    let clock = b.panel(WidgetKind::Clock);
    let slider = b.panel(WidgetKind::SlideSlider);
    let buttons = b.panel(WidgetKind::SlideButtons);
    let tools = b.split(
        "Time and navigation",
        Direction::Horizontal,
        &[0.17, 0.13, 0.32, 0.38],
        vec![timer, clock, slider, buttons],
    );
    let slide = b.slide(WidgetKind::CurrentSlide);

    let root = b.split(
        "Presenter screen",
        Direction::Vertical,
        &[0.90, 0.10],
        vec![slide, tools],
    );
    finish("Slide + Time Below", "slide-time-below", root)
}

/// **Slide + Time Beside** — the same deliberately minimal information in a
/// 25% rail. This is the padding-free counterpart for a 4:3 deck on a 16:9
/// presenter display.
pub fn slide_time_beside() -> Layout {
    let mut b = Builder::new();

    // In a rail there is height to spare, so the two readings stack.
    let timer = b.panel(WidgetKind::Timer);
    let clock = b.panel(WidgetKind::Clock);
    let slider = b.panel(WidgetKind::SlideSlider);
    let buttons = b.panel(WidgetKind::SlideButtons);
    let rail = b.split(
        "Time and navigation",
        Direction::Vertical,
        &[0.24, 0.16, 0.16, 0.44],
        vec![timer, clock, slider, buttons],
    );
    let slide = b.slide(WidgetKind::CurrentSlide);
    let root = b.split(
        "Presenter screen",
        Direction::Horizontal,
        &[0.75, 0.25],
        vec![slide, rail],
    );
    finish("Slide + Time Beside", "slide-time-beside", root)
}

/// The built-ins, in the order the library shows them. The first is what a
/// fresh install opens with.
pub fn built_in_layouts() -> Vec<Layout> {
    vec![
        presenter_default(),
        slide_next_notes(),
        slide_notes_beside(),
        slide_time_below(),
        slide_time_beside(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tree::EmptyBehavior;
    use crate::layout::validate::{validate, Severity};

    #[test]
    fn the_built_ins_are_read_only_and_led_by_the_default() {
        let layouts = built_in_layouts();
        assert_eq!(layouts.len(), 5);
        for layout in &layouts {
            assert_eq!(layout.origin, Origin::BuiltIn);
            assert!(!layout.is_editable());
            assert!(!layout.name.is_empty());
        }
        let ids: Vec<&str> = layouts.iter().map(|layout| layout.id.0.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "presenter-default",
                "slide-next-notes",
                "slide-notes-beside",
                "slide-time-below",
                "slide-time-beside"
            ]
        );
    }

    #[test]
    fn every_built_in_is_canonical_and_free_of_warnings() {
        for layout in built_in_layouts() {
            assert!(layout.is_canonical(), "{} is not canonical", layout.name);
            let issues = validate(&layout, crate::layout::Frame::new(0.0, 0.0, 1600.0, 900.0));
            assert!(
                issues.is_empty(),
                "{} should be warning-free, got {issues:?}",
                layout.name
            );
            assert!(!issues
                .iter()
                .any(|issue| issue.severity == Severity::Blocking));
        }
    }

    #[test]
    fn the_default_is_built_from_clean_fractions() {
        let default = presenter_default();
        let root = default.root.as_split().unwrap();
        assert_eq!(root.sizes, vec![0.92, 0.08]);

        let stage = root.children[0].as_split().unwrap();
        assert_eq!(stage.sizes, vec![0.72, 0.28]);

        let rail = stage.children[1].as_split().unwrap();
        assert_eq!(rail.sizes, vec![0.16, 0.32, 0.52]);
        assert_eq!(rail.children[0].as_split().unwrap().sizes, vec![0.5, 0.5]);

        for size in &root.children[1].as_split().unwrap().sizes {
            assert!((size - 1.0 / 3.0).abs() < 1e-6);
        }

        let kinds: Vec<WidgetKind> = default.widgets().iter().map(|w| w.kind()).collect();
        for required in [
            WidgetKind::CurrentSlide,
            WidgetKind::NextSlide,
            WidgetKind::SpeakerNotes,
            WidgetKind::Clock,
            WidgetKind::Timer,
            WidgetKind::SlideSlider,
            WidgetKind::SlideButtons,
            WidgetKind::Annotations,
        ] {
            assert!(kinds.contains(&required), "default is missing {required:?}");
        }
    }

    #[test]
    fn each_built_in_contains_what_its_description_promises() {
        let default = slide_next_notes();
        let kinds: Vec<WidgetKind> = default.widgets().iter().map(|w| w.kind()).collect();
        for required in [
            WidgetKind::CurrentSlide,
            WidgetKind::NextSlide,
            WidgetKind::SpeakerNotes,
            WidgetKind::Annotations,
            WidgetKind::SlideSlider,
            WidgetKind::SlideButtons,
        ] {
            assert!(kinds.contains(&required), "default is missing {required:?}");
        }

        let notes = slide_notes_beside();
        let kinds: Vec<WidgetKind> = notes.widgets().iter().map(|w| w.kind()).collect();
        assert!(kinds.contains(&WidgetKind::CurrentSlide));
        assert!(kinds.contains(&WidgetKind::SpeakerNotes));
        assert!(kinds.contains(&WidgetKind::SlideSlider));
        assert!(kinds.contains(&WidgetKind::SlideButtons));

        let below = slide_time_below();
        let kinds: Vec<WidgetKind> = below.widgets().iter().map(|w| w.kind()).collect();
        assert_eq!(kinds.len(), 5);
        assert!(kinds.contains(&WidgetKind::CurrentSlide));
        assert!(kinds.contains(&WidgetKind::Timer));
        assert!(kinds.contains(&WidgetKind::Clock));
        assert!(kinds.contains(&WidgetKind::SlideSlider));
        assert!(kinds.contains(&WidgetKind::SlideButtons));

        let beside = slide_time_beside();
        let kinds: Vec<WidgetKind> = beside.widgets().iter().map(|w| w.kind()).collect();
        assert!(kinds.contains(&WidgetKind::CurrentSlide));
        assert!(kinds.contains(&WidgetKind::Timer));
        assert!(kinds.contains(&WidgetKind::Clock));
        assert!(kinds.contains(&WidgetKind::SlideSlider));
        assert!(kinds.contains(&WidgetKind::SlideButtons));
    }

    #[test]
    fn built_ins_give_the_current_slide_the_documented_proportions() {
        let default = slide_next_notes();
        assert!((default.root.as_split().unwrap().sizes[0] - 0.72).abs() < 1e-3);

        for layout in [slide_notes_beside(), slide_time_beside()] {
            assert!((layout.root.as_split().unwrap().sizes[0] - 0.75).abs() < 1e-3);
        }

        let below = slide_time_below();
        assert!((below.root.as_split().unwrap().sizes[0] - 0.90).abs() < 1e-3);
    }

    #[test]
    fn slide_cells_have_nothing_behind_them_and_tools_use_dark_panels() {
        for layout in built_in_layouts() {
            for cell in layout.cells() {
                assert_eq!(
                    cell.border,
                    crate::layout::tree::CellBorder::None,
                    "{}: built-ins must not request perimeter borders",
                    layout.name
                );
                let Some(widget) = &cell.widget else { continue };
                match widget.kind() {
                    // The slide is the bright thing; the space its aspect
                    // ratio leaves over must not become a grey mount.
                    WidgetKind::CurrentSlide
                    | WidgetKind::PreviousSlide
                    | WidgetKind::NextSlide => assert_eq!(
                        cell.background,
                        CellBackground::None,
                        "{}: nothing is painted behind a slide",
                        layout.name
                    ),
                    WidgetKind::SpeakerNotes
                    | WidgetKind::Annotations
                    | WidgetKind::SlideSlider
                    | WidgetKind::SlideButtons => assert_eq!(
                        cell.background,
                        CellBackground::Panel,
                        "{}: presenter tools are dark panels",
                        layout.name
                    ),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn built_ins_have_no_empty_cells() {
        for layout in built_in_layouts() {
            for cell in layout.cells() {
                assert!(!cell.is_empty(), "{} has an empty cell", layout.name);
                assert_eq!(cell.empty_behavior, EmptyBehavior::ShowBlankPanel);
            }
        }
    }

    #[test]
    fn built_ins_have_named_structural_nodes_for_the_tree_panel() {
        for layout in built_in_layouts() {
            let root = layout.root.as_split().expect("a real layout has structure");
            assert_eq!(root.name.as_deref(), Some("Presenter screen"));
        }
    }
}
