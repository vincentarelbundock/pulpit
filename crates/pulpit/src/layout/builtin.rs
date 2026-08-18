//! The immutable built-in layouts, led by the one a first run opens with.
//!
//! These are the layouts a first-time user meets, and the reference for what
//! a good presenter screen looks like: strong hierarchy, readable at a
//! glance, large controls, restrained colour.

use crate::layout::model::{AspectRatio, Layout, LayoutId, LayoutPurpose, Origin};
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

    /// A page cell: the document on a mount.
    ///
    /// The inverse of [`Builder::slide`]. A slide is bright against a dark
    /// screen because that is how it will look on the wall; a page has no
    /// wall, and the space a portrait page leaves in a landscape cell should
    /// read as a mount around the sheet rather than as a hole. The page
    /// surface scrolls inside the cell, so the cell takes no padding of its
    /// own.
    fn page(&mut self) -> Node {
        let mut cell = Cell::with_widget(self.id(), Widget::new(WidgetKind::DocumentPage));
        cell.background = CellBackground::Canvas;
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

    /// A single-button cell: a panel whose padding is the button's own
    /// margin, so a fixed 40-point control still fits a band sized for the
    /// controls beside it.
    fn button(&mut self, kind: WidgetKind) -> Node {
        let mut cell = Cell::with_widget(self.id(), Widget::new(kind));
        cell.background = CellBackground::Panel;
        cell.padding = 4.0;
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

/// Assemble a built-in at a design ratio.
///
/// The ratio is a parameter rather than a constant because a Reader is used in
/// an application window at whatever size the user dragged it to, and often a
/// tall one, since a page is portrait (§2.4). Layouts are stored
/// proportionally and scale to whatever they land on either way; the ratio
/// only sets what the designer previews.
fn finish(name: &str, id: &str, root: Node, ratio: AspectRatio, purpose: LayoutPurpose) -> Layout {
    let mut layout = Layout::from_parts(
        LayoutId(id.to_string()),
        name.to_string(),
        Origin::BuiltIn,
        ratio,
        root,
    )
    .with_purpose(purpose);
    layout.renumber();
    debug_assert!(layout.is_canonical(), "{name} is not canonical");
    layout
}

/// **Presenter Default** — the layout a first run opens with. Most of the
/// width is the live slide; the remaining rail stacks the two readings (clock
/// and timer side by side), the next slide, the notes, and search; a shallow band
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
        b.panel(WidgetKind::Search),
    ];
    // Notes and search share most of the rail, the next slide takes most of
    // the rest, and the clock and timer get the shallow band their digits and
    // one line need.
    let rail = b.split(
        "Look-ahead rail",
        Direction::Vertical,
        &[0.125, 0.25, 0.375, 0.25],
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
    finish(
        "Presenter Default",
        "presenter-default",
        root,
        AspectRatio::SixteenNine,
        LayoutPurpose::Presentation,
    )
}

/// **Reader** — the layout a document opens with.
///
/// The page gets everything that is not a control: a shallow band along the
/// top carries navigation and the annotation tools, and a narrow rail carries
/// the outline and search.
///
/// The band is the height of a button and no more. The rail is narrower than
/// the presenter's, because it holds section titles and search results rather
/// than notes set as prose, and every point past what those need is a point
/// the page is not getting.
pub fn reader_default(ratio: AspectRatio) -> Layout {
    let mut b = Builder::new();

    // The menu is in the band rather than above it: a reader's controls are
    // one row of icons, and the way in to the application belongs on that row
    // instead of on a strip of its own that costs the page another line.
    let band_children = vec![
        b.button(WidgetKind::MainMenu),
        b.panel(WidgetKind::DocumentNav),
        b.panel(WidgetKind::AnnotationTools),
    ];
    let band = b.split(
        "Navigation and tools",
        Direction::Horizontal,
        &[0.06, 0.47, 0.47],
        band_children,
    );

    let rail_children = vec![
        b.panel(WidgetKind::DocumentOutline),
        b.panel(WidgetKind::Search),
    ];
    let rail = b.split(
        "Outline and search",
        Direction::Vertical,
        &[0.65, 0.35],
        rail_children,
    );
    let body_children = vec![rail, b.page()];
    let body = b.split(
        "Document",
        Direction::Horizontal,
        &[0.18, 0.82],
        body_children,
    );

    let root = b.split(
        "Reader",
        Direction::Vertical,
        &[0.07, 0.93],
        vec![band, body],
    );
    finish(
        "Reader",
        "reader-default",
        root,
        ratio,
        LayoutPurpose::Document,
    )
}

/// The built-ins, in the order the library shows them.
///
/// The list is bimodal (§2.1): **Presenter Default** is what a presentation
/// opens with and **Reader** is what a document opens with, and neither is a
/// variant of the other. `built_in_layouts` passes the `SixteenNine` fallback
/// so the list stays parameterless, display-free and testable; a caller with a
/// live window builds a Reader at that window's ratio instead.
pub fn built_in_layouts() -> Vec<Layout> {
    vec![
        presenter_default(),
        reader_default(AspectRatio::SixteenNine),
    ]
}

/// Which built-in a mode opens with (§2.3).
///
/// A property of the document rather than of a global setting: choosing a
/// presenter variant never changes what a PDF opens into, and the reverse.
pub fn default_for(mode: LayoutMode) -> LayoutId {
    match mode {
        LayoutMode::Presentation => LayoutId("presenter-default".to_string()),
        LayoutMode::Document => LayoutId("reader-default".to_string()),
    }
}

/// What a layout is for.
///
/// An alias, not a second type: [`LayoutPurpose`] lives in [`crate::layout::model`]
/// now that it is a field on [`Layout`] rather than something computed only
/// here, but every existing caller of `LayoutMode` — there are many — keeps
/// compiling unchanged.
pub type LayoutMode = LayoutPurpose;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tree::EmptyBehavior;
    use crate::layout::validate::{validate, Severity};

    #[test]
    fn the_built_ins_are_read_only_and_led_by_the_default() {
        let layouts = built_in_layouts();
        assert_eq!(layouts.len(), 2);
        for layout in &layouts {
            assert_eq!(layout.origin, Origin::BuiltIn);
            assert!(!layout.is_editable());
            assert!(!layout.name.is_empty());
        }
        let ids: Vec<&str> = layouts.iter().map(|layout| layout.id.0.as_str()).collect();
        assert_eq!(ids, vec!["presenter-default", "reader-default"]);
    }

    /// A built-in without its explicit purpose — the shape a v1 file, or an
    /// early v2 one, would deserialize to — falls back to the same
    /// widget-driven rule the old `LayoutMode::of` always used.
    #[test]
    fn purposeless_layouts_infer_the_same_mode_the_built_ins_declare() {
        for mut layout in built_in_layouts() {
            let declared = LayoutMode::of(&layout);
            layout.purpose = None;
            assert_eq!(
                LayoutMode::of(&layout),
                declared,
                "{} infers a different mode once its explicit purpose is gone",
                layout.name
            );
        }
    }

    /// §2.1: `reader-default` is a stable identifier, and the assertion on
    /// exact ids above is what keeps a stored user layout's `LayoutId` from
    /// silently colliding with a new built-in. This is the other half: the two
    /// roots are the two modes, and neither is a variant of the other.
    #[test]
    fn the_built_in_list_is_bimodal_and_each_mode_has_one_root() {
        let layouts = built_in_layouts();
        assert_eq!(LayoutMode::of(&layouts[0]), LayoutMode::Presentation);
        assert_eq!(
            default_for(LayoutMode::Presentation),
            layouts[0].id,
            "a presentation opens into the first presenter built-in"
        );

        let reader = reader_default(AspectRatio::SixteenNine);
        assert_eq!(LayoutMode::of(&reader), LayoutMode::Document);
        assert_eq!(default_for(LayoutMode::Document), reader.id);
        assert_ne!(
            default_for(LayoutMode::Document),
            default_for(LayoutMode::Presentation),
            "choosing one mode's default must not change the other's"
        );
    }

    #[test]
    fn the_reader_is_a_page_a_control_band_and_an_outline_rail() {
        let reader = reader_default(AspectRatio::SixteenNine);
        let root = reader.root.as_split().unwrap();
        assert_eq!(root.name.as_deref(), Some("Reader"));
        assert_eq!(root.sizes, vec![0.07, 0.93]);
        assert_eq!(
            root.children[0].as_split().unwrap().sizes,
            vec![0.06, 0.47, 0.47]
        );
        assert_eq!(root.children[1].as_split().unwrap().sizes, vec![0.18, 0.82]);

        let kinds: Vec<WidgetKind> = reader.widgets().iter().map(|w| w.kind()).collect();
        for required in [
            WidgetKind::DocumentPage,
            WidgetKind::DocumentNav,
            WidgetKind::DocumentOutline,
            WidgetKind::AnnotationTools,
            // The way in to the application is on the band with the rest of
            // the controls, not on a strip of its own above them.
            WidgetKind::MainMenu,
        ] {
            assert!(kinds.contains(&required), "Reader is missing {required:?}");
        }
    }

    /// §2.2: the page is on a mount, which is the inverse of a slide's cell.
    #[test]
    fn the_page_cell_is_a_document_on_a_canvas_rather_than_a_slide_in_the_dark() {
        let layout = reader_default(AspectRatio::SixteenNine);
        let page = layout
            .cells()
            .into_iter()
            .find(|cell| cell.widget.as_ref().map(|w| w.kind()) == Some(WidgetKind::DocumentPage))
            .expect("a document layout has a page");
        assert_eq!(page.background, CellBackground::Canvas);
        assert_eq!(
            page.padding, 0.0,
            "the page surface scrolls inside the cell and takes no padding of its own"
        );
    }

    /// §2.3: each mode remembers its own last-used layout, so choosing a
    /// presenter variant never changes what a PDF opens into and the reverse.
    /// A user's *copy* of a built-in answers the same way the built-in does,
    /// which is what makes the memory work for a layout somebody edited.
    #[test]
    fn a_copy_of_a_layout_belongs_to_the_same_mode_as_its_original() {
        for (original, mode) in [
            (presenter_default(), LayoutMode::Presentation),
            (
                reader_default(AspectRatio::SixteenNine),
                LayoutMode::Document,
            ),
        ] {
            let mut copy = original.clone();
            copy.id = crate::layout::LayoutId("a-users-copy".into());
            copy.name = "Mine".into();
            copy.origin = Origin::Custom;
            assert_eq!(
                LayoutMode::of(&copy),
                mode,
                "a copy of {} changed mode",
                original.name
            );
            assert_eq!(mode.label(), LayoutMode::of(&original).label());
        }
    }

    /// §2.4: this is the only built-in whose ratio is not a fixed preset, so
    /// the designer previews a Reader the size a Reader is actually used at.
    #[test]
    fn a_reader_is_designed_at_the_ratio_it_is_asked_for() {
        let tall = reader_default(AspectRatio::Detected {
            width: 1200,
            height: 1600,
        });
        assert_eq!(
            tall.design_ratio,
            AspectRatio::Detected {
                width: 1200,
                height: 1600
            }
        );
        // …and the parameterless list stays display-free.
        assert_eq!(
            built_in_layouts()
                .into_iter()
                .find(|layout| layout.id.0 == "reader-default")
                .unwrap()
                .design_ratio,
            AspectRatio::SixteenNine
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
        assert_eq!(rail.sizes, vec![0.125, 0.25, 0.375, 0.25]);
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
            // Named for what the layout *is*, which is what the tree panel
            // shows: a presenter screen and a reader are not the same thing
            // with two names.
            let expected = match LayoutMode::of(&layout) {
                LayoutMode::Presentation => "Presenter screen",
                LayoutMode::Document => "Reader",
            };
            assert_eq!(root.name.as_deref(), Some(expected), "{}", layout.name);
        }
    }
}
