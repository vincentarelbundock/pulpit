//! The immutable built-in layouts, led by the presentation-mode default.
//!
//! These are the layouts a first-time user meets, and the reference for what
//! a good presenter screen looks like: strong hierarchy, readable at a
//! glance, large controls, restrained colour.

use crate::layout::model::{AspectRatio, Layout, LayoutId, Origin, PrimaryViewer};
use crate::layout::tree::{Cell, CellBackground, CellExtent, Direction, Node, NodeId, Split};
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

    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    fn cell(&mut self, kind: WidgetKind) -> Node {
        Node::Leaf(Cell::with_widget(self.id(), Widget::new(kind)))
    }

    /// A cell holding a widget with adjusted style.
    /// A cell holding a widget with adjusted family configuration.
    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
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
        cell.sizing.vertical = CellExtent::Hug;
        Node::Leaf(cell)
    }

    /// A toolbar cell which also keeps only its functional width. At least
    /// one sibling in the row remains flexible and receives the space this
    /// cell releases.
    fn hug_button(&mut self, kind: WidgetKind) -> Node {
        let mut cell = match self.button(kind) {
            Node::Leaf(cell) => cell,
            Node::Split(_) => unreachable!("button always creates a leaf"),
        };
        cell.sizing.horizontal = CellExtent::Hug;
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
fn finish(name: &str, id: &str, root: Node, ratio: AspectRatio) -> Layout {
    let mut layout = Layout::from_parts(
        LayoutId(id.to_string()),
        name.to_string(),
        Origin::BuiltIn,
        ratio,
        root,
    );
    layout.renumber();
    debug_assert!(layout.is_canonical(), "{name} is not canonical");
    layout
}

/// **Presenter** — the live-presentation layout. Most of the width is the live
/// slide; the remaining rail stacks the two readings (clock and timer
/// side by side), the next slide, and the notes; a shallow band along the
/// bottom carries the controls.
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
    // Notes take most of the rail, the next slide takes most of the rest, and
    // the clock and timer get the shallow band their digits and one line need.
    let rail = b.split(
        "Look-ahead rail",
        Direction::Vertical,
        &[0.15, 0.30, 0.55],
        rail_children,
    );

    let stage_children = vec![b.slide(WidgetKind::CurrentSlide), rail];
    let stage = b.split(
        "Stage",
        Direction::Horizontal,
        &[0.72, 0.28],
        stage_children,
    );

    let control_children = vec![
        b.button(WidgetKind::SlideSlider),
        b.button(WidgetKind::SlideButtons),
        b.button(WidgetKind::Annotations),
    ];
    let controls = b.split(
        "Navigation and tools",
        Direction::Horizontal,
        // The slider and navigation each have ample room at a quarter of the
        // band. The palette gets the other half so its tools remain on the
        // row instead of collapsing behind overflow at ordinary widths.
        &[0.25, 0.25, 0.50],
        control_children,
    );

    // The controls are a row of buttons and a slider; they need the height of
    // a button and no more, and every point past that is taken from the slide.
    let root = b.split(
        "Presenter screen",
        Direction::Vertical,
        &[0.915, 0.085],
        vec![stage, controls],
    );
    finish(
        "Presenter",
        "presenter-default",
        root,
        AspectRatio::SixteenNine,
    )
}

/// **Reader** — the layout a new document opens with.
///
/// The page gets everything that is not a control: a shallow band along the
/// top carries navigation and the annotation tools, and a narrow rail carries
/// the outline. Search and outline share that rail.
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
        b.hug_button(WidgetKind::MainMenu),
        // These are icon runs just like the menu button. The ordinary panel
        // inset puts 24 points plus the split gutter between neighbouring
        // runs; the button inset packs the whole band around its icons.
        //
        // The menu and the navigation run take exactly what they draw, so the
        // tools begin where the zoom controls end instead of across a gap.
        // The tools then take everything that is left: they draw from their
        // own left edge, so the band's spare width ends up to their right,
        // where it reads as room rather than as a hole in the middle.
        b.hug_button(WidgetKind::DocumentNav),
        b.button(WidgetKind::AnnotationTools),
    ];
    let band = b.split(
        "Navigation and tools",
        Direction::Horizontal,
        &[0.05, 0.45, 0.50],
        band_children,
    );

    let body_children = vec![b.panel(WidgetKind::DocumentOutline), b.page()];
    let body = b.split(
        "Document",
        Direction::Horizontal,
        &[0.24, 0.76],
        body_children,
    );

    let root = b.split(
        "Reader",
        Direction::Vertical,
        &[0.07, 0.93],
        vec![band, body],
    );
    finish("Reader", "reader-default", root, ratio)
}

/// The built-ins in canonical presentation-first order. The layout library's
/// view may choose a different display order without changing this fallback.
///
/// The list is bimodal (§2.1): **Reader** is what a new PDF opens with and
/// **Presenter** is the live-presentation view, and neither is a variant of
/// the other. `built_in_layouts` passes the `SixteenNine` fallback so the list
/// stays parameterless, display-free and testable; a caller with a live window
/// builds a Reader at that window's ratio instead.
pub fn built_in_layouts() -> Vec<Layout> {
    vec![
        presenter_default(),
        reader_default(AspectRatio::SixteenNine),
    ]
}

/// Which built-in a mode opens with (§2.3).
///
/// The fallback for one viewer when it has no last-used layout.
pub fn default_for(viewer: PrimaryViewer) -> LayoutId {
    match viewer {
        PrimaryViewer::Slide => LayoutId("presenter-default".to_string()),
        PrimaryViewer::Document => LayoutId("reader-default".to_string()),
    }
}

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
        let names: Vec<&str> = layouts.iter().map(|layout| layout.name.as_str()).collect();
        assert_eq!(names, vec!["Presenter", "Reader"]);
    }

    /// The primary viewer is always derived from the widget tree.
    #[test]
    fn built_ins_derive_their_viewer_from_their_widgets() {
        assert_eq!(
            PrimaryViewer::of(&presenter_default()),
            PrimaryViewer::Slide
        );
        assert_eq!(
            PrimaryViewer::of(&reader_default(AspectRatio::SixteenNine)),
            PrimaryViewer::Document
        );
    }

    /// §2.1: `reader-default` is a stable identifier, and the assertion on
    /// exact ids above is what keeps a stored user layout's `LayoutId` from
    /// silently colliding with a new built-in. This is the other half: the two
    /// roots are the two modes, and neither is a variant of the other.
    #[test]
    fn the_built_in_list_is_bimodal_and_each_mode_has_one_root() {
        let layouts = built_in_layouts();
        assert_eq!(PrimaryViewer::of(&layouts[0]), PrimaryViewer::Slide);
        assert_eq!(
            default_for(PrimaryViewer::Slide),
            layouts[0].id,
            "the slide viewer falls back to the first presenter built-in"
        );

        let reader = reader_default(AspectRatio::SixteenNine);
        assert_eq!(PrimaryViewer::of(&reader), PrimaryViewer::Document);
        assert_eq!(default_for(PrimaryViewer::Document), reader.id);
        assert_ne!(
            default_for(PrimaryViewer::Document),
            default_for(PrimaryViewer::Slide),
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
            vec![0.05, 0.45, 0.50]
        );
        assert_eq!(root.children[1].as_split().unwrap().sizes, vec![0.24, 0.76]);
        for cell in root.children[0].cells() {
            assert_eq!(cell.padding, 4.0, "Reader controls should hug their icons");
            assert_eq!(
                cell.sizing.vertical,
                CellExtent::Hug,
                "Reader controls should take one toolbar row vertically"
            );
        }
        let heights: Vec<f32> = root.children[0]
            .cells()
            .into_iter()
            .filter_map(|cell| cell.widget.as_ref())
            .map(|widget| widget.minimum_size().1)
            .collect();
        assert!(
            heights.windows(2).all(|pair| pair[0] == pair[1]),
            "Reader controls should have one height, got {heights:?}"
        );

        let (placements, _) =
            reader.compute(crate::layout::Frame::new(0.0, 0.0, 887.0, 1066.0), false);
        let frame = |id| {
            placements
                .iter()
                .find(|placement| placement.id == id)
                .expect("built-in node is placed")
                .frame
        };
        let band = &root.children[0];
        assert_eq!(frame(band.id()).height, 40.0);
        let menu = &band.as_split().unwrap().children[0];
        let navigation = &band.as_split().unwrap().children[1];
        let annotations = &band.as_split().unwrap().children[2];
        assert_eq!(frame(menu.id()).width, 40.0);
        // The navigation run hugs the width it is actually drawn at, so the
        // tools start there rather than at the far end of the band…
        assert_eq!(frame(navigation.id()).width, 568.0);
        assert_eq!(
            frame(annotations.id()).x,
            frame(navigation.id()).x + frame(navigation.id()).width + band.as_split().unwrap().gap,
            "the tools should begin where the navigation run ends"
        );
        // …and everything the band has left over is theirs, which is space to
        // the right of the tools rather than between them and the zoom.
        assert_eq!(
            frame(annotations.id()).x + frame(annotations.id()).width,
            frame(band.id()).x + frame(band.id()).width,
            "the tools should take the rest of the band"
        );

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
        assert!(
            !kinds.contains(&WidgetKind::Search),
            "search is a transient workspace, not a permanent reader rail"
        );
    }

    /// The built-ins mount plainly: what a PDF opens into is the Reader with
    /// its band and rail, and no built-in claims the screen or overrides zoom
    /// on the way in.
    #[test]
    fn the_built_ins_mount_plainly_and_the_reader_is_the_document_default() {
        assert_eq!(
            default_for(PrimaryViewer::Document),
            reader_default(AspectRatio::SixteenNine).id
        );
        for layout in built_in_layouts() {
            assert!(!layout.on_mount.fullscreen);
            assert_eq!(layout.on_mount.zoom, None);
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
            (presenter_default(), PrimaryViewer::Slide),
            (
                reader_default(AspectRatio::SixteenNine),
                PrimaryViewer::Document,
            ),
        ] {
            let mut copy = original.clone();
            copy.id = crate::layout::LayoutId("a-users-copy".into());
            copy.name = "Mine".into();
            copy.origin = Origin::Custom;
            assert_eq!(
                PrimaryViewer::of(&copy),
                mode,
                "a copy of {} changed mode",
                original.name
            );
            assert_eq!(mode.label(), PrimaryViewer::of(&original).label());
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
    fn the_default_uses_deliberate_stage_rail_and_control_proportions() {
        let default = presenter_default();
        let root = default.root.as_split().unwrap();
        assert_eq!(root.sizes, vec![0.915, 0.085]);

        let stage = root.children[0].as_split().unwrap();
        assert_eq!(stage.sizes, vec![0.72, 0.28]);

        let rail = stage.children[1].as_split().unwrap();
        assert_eq!(rail.sizes, vec![0.15, 0.30, 0.55]);
        assert_eq!(rail.children[0].as_split().unwrap().sizes, vec![0.5, 0.5]);

        assert_eq!(
            root.children[1].as_split().unwrap().sizes,
            vec![0.25, 0.25, 0.50]
        );

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
        assert!(
            !kinds.contains(&WidgetKind::Search),
            "search is a transient workspace, not a permanent presenter rail"
        );
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
            let expected = match PrimaryViewer::of(&layout) {
                PrimaryViewer::Slide => "Presenter screen",
                PrimaryViewer::Document => "Reader",
            };
            assert_eq!(root.name.as_deref(), Some(expected), "{}", layout.name);
        }
    }
}
