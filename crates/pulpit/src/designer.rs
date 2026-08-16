//! The layout designer's state and command handling.
//!
//! Kept apart from the views so every editing rule — what a drop does to an
//! occupied , what a delete confirms, what undo covers, when a name is
//! required — is one readable state machine rather than something smeared
//! across widget callbacks.

use iced::Point;

use crate::layout::{
    AspectRatio, Direction, History, Issue, Layout, LayoutId, LayoutStore, NodeId, Origin, Side,
};
use crate::widgets::{Widget, WidgetKind};

/// Which page of the application is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Presenter,
    /// The library of layout cards.
    Library,
    Editor,
    /// Settings, displays and diagnostics, taking the whole window.
    Settings,
}

/// What is being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragSource {
    /// A card from the widget library.
    Library(WidgetKind),
    /// A widget already placed in a cell.
    Cell(NodeId),
}

/// Where inside a pane a drop landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// Every edge, in the order the toolbar shows them.
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

    /// The toolbar button that adds a pane here.
    pub fn add_label(self) -> &'static str {
        match self {
            Edge::Left => "Pane left",
            Edge::Right => "Pane right",
            Edge::Top => "Pane above",
            Edge::Bottom => "Pane below",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Edge::Left => "←",
            Edge::Right => "→",
            Edge::Top => "↑",
            Edge::Bottom => "↓",
        }
    }

    pub fn direction(self) -> Direction {
        match self {
            Edge::Left | Edge::Right => Direction::Horizontal,
            Edge::Top | Edge::Bottom => Direction::Vertical,
        }
    }

    pub fn side(self) -> Side {
        match self {
            Edge::Left | Edge::Top => Side::Before,
            Edge::Right | Edge::Bottom => Side::After,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Edge::Left => "to the left",
            Edge::Right => "to the right",
            Edge::Top => "above",
            Edge::Bottom => "below",
        }
    }
}

/// A divider drag in progress.
///
/// More than one divider when the grab was at a junction: a whole line of
/// them moves together.
#[derive(Debug, Clone)]
pub struct DividerDrag {
    pub members: Vec<DividerMember>,
    /// Pointer position at the last step, in canvas coordinates.
    pub from: Point,
    pub whole_line: bool,
}

impl DividerDrag {
    pub fn holds(&self, split: NodeId, index: usize) -> bool {
        self.members
            .iter()
            .any(|member| member.split == split && member.index == index)
    }
}

/// One divider inside a drag, with the position the pointer is asking for
/// before any snapping is applied.
#[derive(Debug, Clone, Copy)]
pub struct DividerMember {
    pub split: NodeId,
    pub index: usize,
    pub free: f32,
}

/// The choice offered when a widget is dropped onto an occupied cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropChoice {
    Replace,
    Swap,
    Cancel,
}

/// A modal question the editor is waiting on.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    /// Dropping onto an occupied cell.
    Drop {
        target: NodeId,
        source: DragSource,
        /// Swapping only makes sense between two cells.
        allow_swap: bool,
    },
    /// A custom layout needs a name the first time it is saved.
    NameLayout { name: String, then_close: bool },
    /// Renaming the layout being edited, from its title.
    RenameLayout { name: String },
    /// Leaving the editor with unsaved changes.
    UnsavedChanges,
    /// Save-time validation report.
    Validation { issues: Vec<Issue>, name: String },
}

/// The editor session.
#[derive(Debug)]
pub struct Designer {
    pub history: History<Layout>,
    pub selected: Option<NodeId>,
    /// A focused divider, for keyboard resizing.
    pub selected_divider: Option<(NodeId, usize)>,
    pub canvas_ratio: AspectRatio,
    /// The shape of the deck being previewed, which is *not* the shape of the
    /// presenter screen: a 4:3 deck shown on a 16:9 display is an ordinary
    /// evening, and the editor has to be able to draw it without that
    /// projector attached.
    ///
    /// Kept in the session rather than on the `Layout`, and deliberately so.
    /// A layout is a description of a screen, not of a document; persisting
    /// the deck's shape into it would make "the layout I use for my 4:3
    /// talks" a different layout from the same arrangement used for a 16:9
    /// talk, and would silently change what a shared layout previews as when
    /// somebody else opens it with a different deck. The consequence — that
    /// the choice resets when the editor is reopened — is the right one: it
    /// is a question about the talk in hand, asked again for the next talk.
    pub slide_ratio: AspectRatio,
    /// A recommendation the presenter asked to look at. Nothing is ever put
    /// here without a press, and putting it here changes no layout.
    pub previewed_recommendation: Option<LayoutId>,
    pub dragging: Option<DragSource>,
    pub hovered_cell: Option<NodeId>,
    /// Ranked built-ins, memoised on (canvas ratio, deck ratio): the inputs
    /// change on explicit presses, the view asks on every pass.
    recommendations_cache: RecommendationsCache,
    /// Which edge of the hovered pane the pointer is over, if any.
    pub hovered_edge: Option<Edge>,
    pub dialog: Option<Dialog>,
    /// Divider being dragged: split, index, and the pointer position where
    /// the drag started.
    pub dragging_divider: Option<DividerDrag>,
    pub pointer: Point,
    /// Canvas size in points, needed to turn pixel drags into proportions.
    pub canvas_size: iced::Size,
    pub snap: bool,
    /// Snapping is suspended while a modifier is held.
    pub snap_suspended: bool,
    pub issues: Vec<Issue>,
    pub status: Option<String>,
}

/// The memoised layout recommendations: the (canvas ratio, deck ratio) they
/// were ranked for, and the ranking.
type RecommendationsCache =
    std::cell::RefCell<Option<((u32, u32), Vec<crate::layout::fit::Recommendation>)>>;

impl Designer {
    pub fn open(layout: Layout) -> Designer {
        let selected = Some(layout.root.id());
        let slide_ratio = layout.design_ratio;
        Designer {
            canvas_ratio: AspectRatio::SixteenNine,
            // A first guess, not a decision: the deck most likely matches the
            // shape the layout was designed for, and the presenter can say
            // otherwise in one press.
            slide_ratio,
            previewed_recommendation: None,
            history: History::new(layout),
            selected,
            selected_divider: None,
            dragging: None,
            hovered_cell: None,
            hovered_edge: None,
            dialog: None,
            dragging_divider: None,
            pointer: Point::ORIGIN,
            canvas_size: iced::Size::new(960.0, 540.0),
            snap: true,
            snap_suspended: false,
            issues: Vec::new(),
            status: None,
            recommendations_cache: std::cell::RefCell::new(None),
        }
    }

    /// A brand-new custom layout that has never been saved.
    pub fn create(name: &str) -> Designer {
        let layout = Layout::empty(name);
        let selected = Some(layout.root.id());
        Designer {
            history: History::unsaved(layout),
            selected,
            ..Designer::open(Layout::empty(name))
        }
    }

    pub fn layout(&self) -> &Layout {
        self.history.current()
    }

    pub fn is_editable(&self) -> bool {
        self.layout().is_editable()
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.is_editable() && self.history.has_unsaved_changes()
    }

    /// Run an edit through the history, so undo covers it. Read-only layouts
    /// refuse every edit.
    fn edit(&mut self, action: impl FnOnce(&mut Layout)) {
        if !self.is_editable() {
            self.status = Some(
                "This is a built-in layout. Use Duplicate to Customize to make an editable copy."
                    .into(),
            );
            return;
        }
        self.history.edit(action);
        self.revalidate();
    }

    pub fn revalidate(&mut self) {
        let area = self.design_area();
        self.issues = crate::layout::validate(self.layout(), area);
    }

    /// The reference area validation and geometry use: the canvas ratio at a
    /// nominal 1600pt width.
    pub fn design_area(&self) -> crate::layout::Frame {
        let width = 1600.0;
        crate::layout::Frame::new(0.0, 0.0, width, width / self.canvas_ratio.ratio())
    }

    // -------------------------------------------------------- space and fit

    /// The panes of the layout being edited that hold slide content, measured
    /// against the design area.
    pub fn slide_cells(&self) -> Vec<crate::layout::Frame> {
        crate::layout::thumbnail::slide_cells(&self.layout().root, self.design_area())
    }

    /// What this layout does with its slide panes for the deck being
    /// previewed. Measurement only: it never changes anything.
    pub fn space_report(&self) -> crate::layout::fit::SpaceReport {
        crate::layout::fit::SpaceReport::measure(
            self.design_area(),
            self.slide_ratio.ratio(),
            &self.slide_cells(),
        )
    }

    /// The sentence to show beside the canvas, or nothing when the layout is
    /// using its space well enough not to be worth interrupting for.
    pub fn space_warning(&self) -> Option<String> {
        self.space_report().warning()
    }

    /// Built-in layouts ranked by how little slide-pane space they waste for
    /// the deck being previewed.
    ///
    /// This ranks and nothing else. The result is a list to read; adopting
    /// one is a separate, deliberate press, and no code path here writes to
    /// the layout being edited.
    pub fn recommendations(&self) -> Vec<crate::layout::fit::Recommendation> {
        // The answer depends only on the design area's shape and the deck
        // ratio, yet the view asks per pass — and computing it constructs
        // all four built-in layouts and measures each one. Memoised on the
        // two inputs.
        let key = (
            self.canvas_ratio.ratio().to_bits(),
            self.slide_ratio.ratio().to_bits(),
        );
        if let Some((cached, recommendations)) = self.recommendations_cache.borrow().as_ref() {
            if *cached == key {
                return recommendations.clone();
            }
        }
        let area = self.design_area();
        let candidates: Vec<(String, Vec<crate::layout::Frame>)> =
            crate::layout::builtin::built_in_layouts()
                .into_iter()
                .map(|layout| {
                    let cells = crate::layout::thumbnail::slide_cells(&layout.root, area);
                    (layout.name, cells)
                })
                .collect();
        let recommendations =
            crate::layout::fit::recommend(area, self.slide_ratio.ratio(), &candidates);
        *self.recommendations_cache.borrow_mut() = Some((key, recommendations.clone()));
        recommendations
    }

    /// Look at a recommended layout without adopting it.
    pub fn preview_recommendation(&mut self, id: LayoutId) {
        self.previewed_recommendation = Some(id);
    }

    // ------------------------------------------------------------ selection

    pub fn select(&mut self, node: NodeId) {
        self.selected = Some(node);
        self.selected_divider = None;
    }

    pub fn select_divider(&mut self, split: NodeId, index: usize) {
        self.selected_divider = Some((split, index));
        self.selected = Some(split);
    }
    // ------------------------------------------------------------ structure

    /// Divide a pane in two. `add_pane` is how the editor reaches this;
    /// tests build fixtures with it directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn split(&mut self, node: NodeId, direction: Direction) {
        let mut created = None;
        self.edit(|layout| {
            if let Ok(crate::layout::Change::Created(id)) = layout.split_cell(node, direction) {
                created = Some(id);
            }
        });
        if let Some(id) = created {
            self.select(id);
        }
    }

    /// Add an empty pane on one side of a pane, as one undoable edit.
    ///
    /// Whatever is in the target stays where it is; the new pane is the empty
    /// one, and it becomes the selection so a widget can be dropped straight
    /// into it.
    pub fn add_pane(&mut self, target: NodeId, edge: Edge) {
        // A pane is divided, not a whole split: dividing a split would have
        // to decide which of its children to disturb.
        if self.layout().cell(target).is_none() {
            self.status = Some("Select a single pane to add one beside it.".into());
            return;
        }

        let mut created = None;
        self.edit(|layout| {
            if let Ok(crate::layout::Change::Created(id)) =
                layout.split_cell_at(target, edge.direction(), edge.side())
            {
                created = Some(id);
            }
        });
        if let Some(id) = created {
            self.select(id);
        }
    }

    pub fn clear_cell(&mut self, node: NodeId) {
        self.edit(|layout| {
            let _ = layout.clear_cell(node);
        });
    }

    /// Remove a pane, or a split whose panes are all empty.
    ///
    /// Nothing is ever destroyed in one press, and nothing asks "are you
    /// sure": a confirmation is a question about work you can see on the
    /// canvas. A pane holding a widget gives up the widget first and the pane
    /// itself on the next press, both undoable.
    ///
    /// A split is removed only when its own panes are the ones that go — its
    /// direct children. Removing one that contains further splits would take
    /// a whole subtree with it, which is not what pointing at a divider looks
    /// like it should do.
    pub fn request_delete(&mut self, node: NodeId) {
        if !self.is_editable() {
            self.edit(|_| {});
            return;
        }
        // A pane with a widget in it empties first, and goes on the second
        // press. One button, two steps, in the order you would do them by
        // hand — and the first step is undoable, so a press too many is not a
        // decision. This is what a right-click on a pane does, so the two
        // ways of reaching it behave alike.
        //
        // This comes before the check below, because a layout of one pane is
        // a pane *and* the whole canvas: clearing its widget is an ordinary
        // thing to want, and refusing on the grounds that the canvas cannot
        // be removed would be answering a question nobody asked.
        if self
            .layout()
            .cell(node)
            .is_some_and(|cell| cell.widget.is_some())
        {
            self.clear_cell(node);
            return;
        }

        if self.layout().root.id() == node {
            self.status = Some("The whole canvas cannot be removed.".into());
            return;
        }

        let widgets = self.layout().widgets_in(node);
        if !widgets.is_empty() {
            let names: Vec<&str> = widgets.iter().map(|kind| kind.label()).collect();
            self.status = Some(format!(
                "{} would be removed with this pane. Clear the panes that hold them first.",
                names.join(", ")
            ));
            return;
        }

        if let Some(split) = self.layout().split(node) {
            if split
                .children
                .iter()
                .any(|child| child.as_split().is_some())
            {
                self.status = Some(
                    "This holds further panes. Remove them one at a time — only the panes on \
                     either side of a divider go with it."
                        .into(),
                );
                return;
            }
        }

        self.delete(node);
    }

    pub fn delete(&mut self, node: NodeId) {
        let parent = self.layout().root.parent_of(node);
        self.edit(|layout| {
            let _ = layout.delete_node(node);
        });
        self.dialog = None;
        self.selected = parent
            .filter(|id| self.layout().find(*id).is_some())
            .or(Some(self.layout().root.id()));
        self.selected_divider = None;
    }

    // -------------------------------------------------------------- widgets

    /// Place a widget in a cell, honouring instance limits.
    pub fn place(&mut self, target: NodeId, kind: WidgetKind) {
        if self
            .layout()
            .check_instances(&Widget::new(kind), Some(target))
            .is_err()
        {
            self.status = Some(format!(
                "{} is already in this layout and may only appear once.",
                kind.label()
            ));
            return;
        }
        self.edit(|layout| {
            let _ = layout.set_widget(target, Widget::new(kind));
        });
        self.select(target);
    }

    /// Begin a drag from the library or from a placed widget.
    pub fn start_drag(&mut self, source: DragSource) {
        if !self.is_editable() {
            return;
        }
        if let DragSource::Library(kind) = source {
            if self.layout().already_placed(kind) {
                self.status = Some(format!("{} is already in this layout.", kind.label()));
                return;
            }
        }
        self.dragging = Some(source);
    }

    /// A press on a pane selects it, and starts dragging whatever is in it.
    pub fn press_cell(&mut self, cell: NodeId) {
        self.select(cell);
        let has_widget = self
            .layout()
            .cell(cell)
            .is_some_and(|cell| cell.widget.is_some());
        if self.is_editable() && has_widget && self.dragging.is_none() {
            self.start_drag(DragSource::Cell(cell));
        }
    }

    pub fn hover(&mut self, cell: NodeId) {
        if self.dragging.is_some() {
            self.hovered_cell = Some(cell);
            self.hovered_edge = None;
        }
    }

    /// The pointer is over one edge of a pane: dropping here will split it.
    pub fn hover_edge(&mut self, cell: NodeId, edge: Option<Edge>) {
        if self.dragging.is_some() {
            self.hovered_cell = Some(cell);
            self.hovered_edge = edge;
        }
    }

    /// Finish a drag on one edge of `target`: split that pane and place the
    /// widget in the new half, in one gesture.
    ///
    /// This is the whole point of the edge zones — without them, adding a
    /// widget beside another means splitting first, aiming at the resulting
    /// empty pane second, and holding the tree in your head in between.
    pub fn drop_on_edge(&mut self, target: NodeId, edge: Edge) {
        let Some(source) = self.dragging.take() else {
            return;
        };
        self.hovered_cell = None;
        self.hovered_edge = None;

        if let DragSource::Cell(from) = source {
            if from == target {
                // Dropping a pane's own widget on its own edge would split
                // the pane and move the widget into the new half: a lot of
                // structure for what looks like a slip of the hand.
                return;
            }
        }

        // The instance limit is checked *before* splitting: refusing after
        // would leave an empty pane behind as a souvenir of a rejected drop.
        if let DragSource::Library(kind) = source {
            if self
                .layout()
                .check_instances(&Widget::new(kind), None)
                .is_err()
            {
                self.status = Some(format!(
                    "{} is already in this layout and may only appear once.",
                    kind.label()
                ));
                return;
            }
        }

        let mut created = None;
        self.edit(|layout| {
            if let Ok(crate::layout::Change::Created(id)) =
                layout.split_cell_at(target, edge.direction(), edge.side())
            {
                created = Some(id);
            }
        });
        let Some(fresh) = created else { return };

        let label = match source {
            DragSource::Library(kind) => {
                self.edit(|layout| {
                    let _ = layout.set_widget(fresh, Widget::new(kind));
                });
                kind.label()
            }
            DragSource::Cell(from) => {
                let moved = self
                    .layout()
                    .cell(from)
                    .and_then(|cell| cell.widget.as_ref())
                    .map(|widget| widget.kind().label())
                    .unwrap_or("The widget");
                self.edit(|layout| {
                    let _ = layout.move_widget(from, fresh);
                });
                moved
            }
        };
        self.status = Some(format!("{label} placed {}.", edge.describe()));
        self.select(fresh);
    }

    /// Finish a drag over `target`.
    pub fn drop_on(&mut self, target: NodeId) {
        let Some(source) = self.dragging.take() else {
            return;
        };
        self.hovered_cell = None;

        let occupied = self
            .layout()
            .cell(target)
            .is_some_and(|cell| !cell.is_empty());
        let from_cell = matches!(source, DragSource::Cell(_));

        if !occupied {
            self.apply_drop(target, source, DropChoice::Replace);
            return;
        }
        if let DragSource::Cell(from) = source {
            if from == target {
                return;
            }
        }
        // An occupied cell asks: replace, swap, or cancel.
        self.dialog = Some(Dialog::Drop {
            target,
            source,
            allow_swap: from_cell,
        });
    }

    pub fn resolve_drop(&mut self, choice: DropChoice) {
        let Some(Dialog::Drop { target, source, .. }) = self.dialog.clone() else {
            return;
        };
        self.dialog = None;
        if choice == DropChoice::Cancel {
            return;
        }
        self.apply_drop(target, source, choice);
    }

    fn apply_drop(&mut self, target: NodeId, source: DragSource, choice: DropChoice) {
        match source {
            DragSource::Library(kind) => self.place(target, kind),
            DragSource::Cell(from) => {
                self.edit(|layout| {
                    let _ = match choice {
                        DropChoice::Swap => layout.swap_widgets(from, target),
                        _ => layout.move_widget(from, target),
                    };
                });
                self.select(target);
            }
        }
    }

    pub fn cancel_drag(&mut self) {
        self.dragging = None;
        self.hovered_cell = None;
    }

    // ------------------------------------------------------------- resizing

    pub fn start_divider_drag(&mut self, split: NodeId, index: usize, at: Point) {
        if !self.is_editable() {
            return;
        }

        // Grabbing a gutter near one of its ends — where it meets the
        // dividers of the rows above and below — takes hold of the whole
        // line. Grabbing it in the middle moves that segment alone. This is
        // how a column of panes is kept flush without dragging each row and
        // hoping.
        let whole_line = self.near_a_junction(split, index, at);
        let mut members = vec![self.member(split, index)];
        if whole_line {
            for (other, other_index) in self.aligned_dividers(split, index) {
                members.push(self.member(other, other_index));
            }
        }

        self.dragging_divider = Some(DividerDrag {
            members,
            from: at,
            whole_line,
        });
        self.select_divider(split, index);
    }

    /// A divider and the unsnapped position it starts from. Keeping the
    /// unsnapped value is what lets a drag pull free of a snap point.
    fn member(&self, split: NodeId, index: usize) -> DividerMember {
        DividerMember {
            split,
            index,
            free: self
                .layout()
                .split(split)
                .and_then(|split| split.sizes.get(index).copied())
                .unwrap_or(0.5),
        }
    }

    /// Is the grab point within a corner's reach of either end of the gutter?
    fn near_a_junction(&self, split_id: NodeId, index: usize, at: Point) -> bool {
        /// How close to an end counts as the corner, in canvas points.
        const CORNER: f32 = 28.0;

        let Some(frame) = self.divider_frame(split_id, index) else {
            return false;
        };
        let Some(direction) = self.layout().split(split_id).map(|split| split.direction) else {
            return false;
        };
        // Along the gutter's length, not across it.
        let (start, end, position) = match direction {
            Direction::Horizontal => (frame.y, frame.y + frame.height, at.y),
            Direction::Vertical => (frame.x, frame.x + frame.width, at.x),
        };
        // A gutter shorter than two corners is all corner; treat it as middle
        // so a small pane is still resizable on its own.
        if end - start <= CORNER * 2.5 {
            return false;
        }
        position - start <= CORNER || end - position <= CORNER
    }

    /// The gutter's rectangle in canvas coordinates.
    fn divider_frame(&self, split_id: NodeId, index: usize) -> Option<crate::layout::Frame> {
        let area = self.design_area();
        let (_, dividers) = self.layout().compute(area, false);
        let divider = dividers
            .into_iter()
            .find(|divider| divider.split == split_id && divider.index == index)?;
        let scale_x = self.canvas_size.width / area.width.max(1.0);
        let scale_y = self.canvas_size.height / area.height.max(1.0);
        Some(crate::layout::Frame::new(
            divider.frame.x * scale_x,
            divider.frame.y * scale_y,
            divider.frame.width * scale_x,
            divider.frame.height * scale_y,
        ))
    }

    /// Dividers elsewhere that run the same way and sit on the same line.
    fn aligned_dividers(&self, split_id: NodeId, index: usize) -> Vec<(NodeId, usize)> {
        let area = self.design_area();
        let (_, dividers) = self.layout().compute(area, false);
        let Some(anchor) = dividers
            .iter()
            .find(|divider| divider.split == split_id && divider.index == index)
        else {
            return Vec::new();
        };
        let centre = |divider: &crate::layout::Divider| match divider.direction {
            Direction::Horizontal => divider.frame.x + divider.frame.width / 2.0,
            Direction::Vertical => divider.frame.y + divider.frame.height / 2.0,
        };
        let span = match anchor.direction {
            Direction::Horizontal => area.width,
            Direction::Vertical => area.height,
        };
        let tolerance = span * 0.004;
        let target = centre(anchor);
        dividers
            .iter()
            .filter(|divider| {
                divider.direction == anchor.direction
                    && !(divider.split == split_id && divider.index == index)
                    && (centre(divider) - target).abs() <= tolerance
            })
            .map(|divider| (divider.split, divider.index))
            .collect()
    }

    /// Continue a divider drag. `at` is in canvas coordinates.
    pub fn drag_divider_to(&mut self, at: Point) {
        self.pointer = at;
        let Some(drag) = self.dragging_divider.clone() else {
            return;
        };
        let area = self.design_area();
        let snap = self.snap && !self.snap_suspended;
        let mut moved_any = false;
        let mut members = drag.members.clone();

        for member in &mut members {
            let Some(split) = self.layout().split(member.split) else {
                continue;
            };
            let direction = split.direction;

            // The drag is a fraction of the *split's* span, not the whole
            // canvas, so a divider inside a small panel still tracks the
            // pointer.
            let span = self
                .layout()
                .frame_of(member.split, area, false)
                .map(|frame| match direction {
                    Direction::Horizontal => frame.width,
                    Direction::Vertical => frame.height,
                })
                .unwrap_or(area.width);
            if span <= 0.0 {
                continue;
            }
            let canvas_span = match direction {
                Direction::Horizontal => self.canvas_size.width,
                Direction::Vertical => self.canvas_size.height,
            };
            let scale = if canvas_span > 0.0 {
                (match direction {
                    Direction::Horizontal => area.width,
                    Direction::Vertical => area.height,
                }) / canvas_span
            } else {
                1.0
            };
            let moved = match direction {
                Direction::Horizontal => at.x - drag.from.x,
                Direction::Vertical => at.y - drag.from.y,
            } * scale;
            let delta = moved / span;
            if delta.abs() < 1e-4 {
                continue;
            }

            member.free += delta;
            let (split_id, index, free) = (member.split, member.index, member.free);
            // A line moves as one: each divider snaps to the same rules, and
            // they were flush to begin with, so they stay flush.
            self.edit(|layout| {
                let _ = layout.resize_divider_to(split_id, index, free, snap);
            });
            moved_any = true;
        }

        if moved_any {
            self.dragging_divider = Some(DividerDrag {
                members,
                from: at,
                whole_line: drag.whole_line,
            });
        }
    }

    pub fn end_divider_drag(&mut self) {
        self.dragging_divider = None;
    }

    /// Keyboard resizing: 1% per step, 5% with shift.
    pub fn nudge_divider(&mut self, steps: i32, large: bool) {
        let Some((split, index)) = self.selected_divider else {
            return;
        };
        let delta = steps as f32 * if large { 0.05 } else { 0.01 };
        let snap = false;
        self.edit(|layout| {
            let _ = layout.resize_divider(split, index, delta, snap);
        });
    }

    pub fn equalize(&mut self, split: NodeId) {
        self.edit(|layout| {
            let _ = layout.equalize(split);
        });
    }

    // ------------------------------------------------------------ properties

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_child_size(&mut self, split: NodeId, index: usize, fraction: f32) {
        self.edit(|layout| {
            let _ = layout.set_child_size(split, index, fraction);
        });
    }

    // ------------------------------------------------------------ undo/redo

    pub fn undo(&mut self) {
        if self.history.undo() {
            self.repair_selection();
            self.revalidate();
        }
    }

    pub fn redo(&mut self) {
        if self.history.redo() {
            self.repair_selection();
            self.revalidate();
        }
    }

    /// After undo or redo the selected node may no longer exist.
    fn repair_selection(&mut self) {
        let root = self.layout().root.id();
        if self
            .selected
            .is_none_or(|id| self.layout().find(id).is_none())
        {
            self.selected = Some(root);
        }
        if self.selected_divider.is_some_and(|(split, index)| {
            self.layout()
                .split(split)
                .is_none_or(|split| index + 1 >= split.children.len())
        }) {
            self.selected_divider = None;
        }
    }

    // ---------------------------------------------------------------- saving

    /// Save, asking for a name the first time.
    pub fn save(&mut self, store: &mut LayoutStore, then_close: bool) -> Option<LayoutId> {
        if !self.is_editable() {
            self.status = Some("Built-in layouts are read only.".into());
            return None;
        }
        self.revalidate();
        if crate::layout::validate::is_blocked(&self.issues) {
            self.dialog = Some(Dialog::Validation {
                issues: self.issues.clone(),
                name: self.layout().name.clone(),
            });
            return None;
        }
        let needs_name = self.history.saved_state().is_none()
            || self.layout().name.trim().is_empty()
            || self.layout().name == "Untitled layout";
        if needs_name {
            self.dialog = Some(Dialog::NameLayout {
                name: self.layout().name.clone(),
                then_close,
            });
            return None;
        }
        self.write(store)
    }

    pub fn save_named(&mut self, store: &mut LayoutStore, name: String) -> Option<LayoutId> {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.status = Some("A layout needs a name.".into());
            return None;
        }
        let taken = store
            .all()
            .any(|layout| layout.name.eq_ignore_ascii_case(&name) && layout.id != self.layout().id);
        if taken {
            self.status = Some(format!("A layout named “{name}” already exists."));
            return None;
        }
        self.history.edit(|layout| {
            layout.name = name;
            layout.origin = Origin::Custom;
        });
        self.dialog = None;
        self.write(store)
    }

    fn write(&mut self, store: &mut LayoutStore) -> Option<LayoutId> {
        self.revalidate();
        let mut layout = self.layout().clone();
        layout.design_ratio = self.canvas_ratio;
        match store.save(&layout) {
            Ok(id) => {
                self.history.edit(|current| {
                    current.id = id.clone();
                    current.design_ratio = layout.design_ratio;
                });
                self.history.mark_saved();
                let warnings = self.issues.len();
                self.status = Some(if warnings == 0 {
                    format!("Saved “{}”.", self.layout().name)
                } else {
                    format!(
                        "Saved “{}” with {warnings} warning{}.",
                        self.layout().name,
                        if warnings == 1 { "" } else { "s" }
                    )
                });
                Some(id)
            }
            Err(e) => {
                self.status = Some(format!("Could not save: {e}"));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::builtin::{slide_next_notes, slide_notes_beside, slide_time_below};

    fn designer_with_two_cells() -> Designer {
        let mut designer = Designer::create("Test");
        let root = designer.layout().root.id();
        designer.split(root, Direction::Horizontal);
        designer
    }

    fn cells(designer: &Designer) -> Vec<NodeId> {
        designer
            .layout()
            .cells()
            .iter()
            .map(|cell| cell.id)
            .collect()
    }

    #[test]
    fn dropping_on_an_edge_splits_and_places_in_one_gesture() {
        for edge in Edge::ALL {
            let mut designer = Designer::create("Edges");
            let root = designer.layout().root.id();
            designer.place(root, WidgetKind::CurrentSlide);
            assert_eq!(cells(&designer).len(), 1);

            designer.start_drag(DragSource::Library(WidgetKind::SpeakerNotes));
            designer.hover_edge(root, Some(edge));
            designer.drop_on_edge(root, edge);

            let ids = cells(&designer);
            assert_eq!(ids.len(), 2, "{edge:?}: the pane was split");
            let fresh = designer.selected.expect("the new pane is selected");
            assert_eq!(
                designer
                    .layout()
                    .cell(fresh)
                    .and_then(|cell| cell.widget.as_ref())
                    .map(|widget| widget.kind()),
                Some(WidgetKind::SpeakerNotes),
                "{edge:?}: the widget went into the new pane"
            );
            // The pane that was dropped on keeps what it had.
            assert_eq!(
                designer
                    .layout()
                    .cell(root)
                    .and_then(|cell| cell.widget.as_ref())
                    .map(|widget| widget.kind()),
                Some(WidgetKind::CurrentSlide)
            );

            // And it landed on the side the pointer said.
            let expected_first = match edge {
                Edge::Left | Edge::Top => fresh,
                Edge::Right | Edge::Bottom => root,
            };
            assert_eq!(ids[0], expected_first, "{edge:?}: wrong side");
        }
    }

    #[test]
    fn an_edge_drop_respects_the_one_instance_rule() {
        // Speaker notes may appear only once; slide views may repeat.
        let mut designer = Designer::create("Once");
        let root = designer.layout().root.id();
        designer.place(root, WidgetKind::SpeakerNotes);

        // A stale drag of a widget that is now placed must not slip past the
        // limit by taking the edge route.
        designer.dragging = Some(DragSource::Library(WidgetKind::SpeakerNotes));
        designer.drop_on_edge(root, Edge::Right);

        assert_eq!(cells(&designer).len(), 1, "no pane was created");
        assert_eq!(
            designer
                .layout()
                .widgets()
                .iter()
                .filter(|widget| widget.kind() == WidgetKind::SpeakerNotes)
                .count(),
            1
        );
        assert!(designer.status.is_some_and(|note| note.contains("once")));
    }

    #[test]
    fn a_widget_dropped_on_its_own_edge_does_nothing() {
        let mut designer = Designer::create("Same");
        let root = designer.layout().root.id();
        designer.place(root, WidgetKind::CurrentSlide);
        let before = designer.layout().clone();

        designer.start_drag(DragSource::Cell(root));
        designer.drop_on_edge(root, Edge::Left);

        assert_eq!(
            designer.layout(),
            &before,
            "a slip of the hand is not an edit"
        );
    }

    #[test]
    fn a_pane_can_be_added_on_any_of_the_four_sides() {
        for edge in Edge::ALL {
            let mut designer = Designer::create("Sides");
            let root = designer.layout().root.id();
            designer.place(root, WidgetKind::CurrentSlide);
            designer.add_pane(root, edge);

            let cells = cells(&designer);
            assert_eq!(cells.len(), 2, "{}", edge.add_label());
            assert!(designer.layout().is_canonical(), "{}", edge.add_label());

            // What was there stays where it was, and the new pane is empty
            // and selected, ready for a widget to be dropped into it.
            assert_eq!(
                designer
                    .layout()
                    .cell(root)
                    .and_then(|cell| cell.widget.as_ref())
                    .map(|widget| widget.kind()),
                Some(WidgetKind::CurrentSlide),
                "{}",
                edge.add_label()
            );
            let selected = designer.selected.expect("the new pane is selected");
            assert!(designer
                .layout()
                .cell(selected)
                .is_some_and(|cell| cell.is_empty()));

            // …and on the side that was asked for.
            let new_is_first = cells.first() == Some(&selected);
            assert_eq!(
                new_is_first,
                matches!(edge, Edge::Left | Edge::Top),
                "{} put the new pane on the wrong side",
                edge.add_label()
            );
        }
    }

    #[test]
    fn adding_a_pane_to_a_whole_split_says_what_to_do_instead() {
        let mut designer = designer_with_two_cells();
        let root = designer.layout().root.id();

        designer.add_pane(root, Edge::Right);

        assert!(
            designer
                .status
                .is_some_and(|note| note.contains("single pane")),
            "it says why instead of disturbing a subtree"
        );
    }

    #[test]
    fn a_built_in_layout_refuses_every_edit_and_says_why() {
        let mut designer = Designer::open(slide_next_notes());
        let before = designer.layout().clone();
        let root = designer.layout().root.id();

        designer.split(root, Direction::Vertical);
        assert_eq!(designer.layout(), &before, "nothing changed");
        assert!(designer
            .status
            .as_ref()
            .unwrap()
            .contains("Duplicate to Customize"));
        assert!(!designer.history.can_undo());
    }

    #[test]
    fn dropping_on_an_empty_cell_places_immediately() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.start_drag(DragSource::Library(WidgetKind::CurrentSlide));
        designer.drop_on(ids[0]);

        assert!(designer.dialog.is_none(), "no question for an empty cell");
        assert_eq!(
            designer
                .layout()
                .cell(ids[0])
                .unwrap()
                .widget
                .as_ref()
                .unwrap()
                .kind(),
            WidgetKind::CurrentSlide
        );
    }

    #[test]
    fn dropping_on_an_occupied_cell_asks_replace_swap_or_cancel() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.place(ids[0], WidgetKind::CurrentSlide);
        designer.place(ids[1], WidgetKind::SpeakerNotes);

        designer.start_drag(DragSource::Cell(ids[0]));
        designer.drop_on(ids[1]);
        assert!(matches!(
            designer.dialog,
            Some(Dialog::Drop {
                allow_swap: true,
                ..
            })
        ));

        designer.resolve_drop(DropChoice::Cancel);
        assert!(designer.dialog.is_none());
        assert_eq!(
            designer
                .layout()
                .cell(ids[0])
                .unwrap()
                .widget
                .as_ref()
                .unwrap()
                .kind(),
            WidgetKind::CurrentSlide,
            "cancel changes nothing"
        );

        designer.start_drag(DragSource::Cell(ids[0]));
        designer.drop_on(ids[1]);
        designer.resolve_drop(DropChoice::Swap);
        assert_eq!(
            designer
                .layout()
                .cell(ids[0])
                .unwrap()
                .widget
                .as_ref()
                .unwrap()
                .kind(),
            WidgetKind::SpeakerNotes
        );
        assert_eq!(
            designer
                .layout()
                .cell(ids[1])
                .unwrap()
                .widget
                .as_ref()
                .unwrap()
                .kind(),
            WidgetKind::CurrentSlide
        );
    }

    #[test]
    fn a_library_drop_onto_an_occupied_cell_cannot_swap() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.place(ids[0], WidgetKind::CurrentSlide);

        designer.start_drag(DragSource::Library(WidgetKind::Clock));
        designer.drop_on(ids[0]);
        assert!(matches!(
            designer.dialog,
            Some(Dialog::Drop {
                allow_swap: false,
                ..
            })
        ));
        designer.resolve_drop(DropChoice::Replace);
        assert_eq!(
            designer
                .layout()
                .cell(ids[0])
                .unwrap()
                .widget
                .as_ref()
                .unwrap()
                .kind(),
            WidgetKind::Clock
        );
    }

    #[test]
    fn a_single_instance_widget_cannot_be_dragged_twice() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.place(ids[0], WidgetKind::SpeakerNotes);

        designer.start_drag(DragSource::Library(WidgetKind::SpeakerNotes));
        assert!(
            designer.dragging.is_none(),
            "the card refuses to be dragged"
        );
        assert!(designer.status.as_ref().unwrap().contains("already"));

        designer.place(ids[1], WidgetKind::SpeakerNotes);
        assert!(designer.layout().cell(ids[1]).unwrap().is_empty());
    }

    #[test]
    fn the_only_pane_still_gives_up_its_widget() {
        // A layout of one pane is a pane *and* the whole canvas. Clearing its
        // widget is ordinary; it used to be refused with "the whole canvas
        // cannot be removed", which answered a question nobody had asked.
        let mut designer = Designer::create("One pane");
        let root = designer.layout().root.id();
        designer.place(root, WidgetKind::CurrentSlide);

        designer.request_delete(root);

        assert!(designer.layout().cell(root).unwrap().is_empty());
        assert_eq!(designer.status, None, "and says nothing about it");

        // The pane itself is the canvas, so that is where it stops.
        designer.request_delete(root);
        assert_eq!(designer.layout().cells().len(), 1);
        assert!(designer
            .status
            .as_ref()
            .is_some_and(|note| note.contains("canvas")));
    }

    #[test]
    fn removing_takes_the_widget_first_and_the_pane_next() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.place(ids[1], WidgetKind::SpeakerNotes);

        // First press: the widget goes, the pane stays.
        designer.request_delete(ids[1]);
        assert_eq!(designer.layout().cells().len(), 2, "the pane stays");
        assert!(designer.layout().cell(ids[1]).unwrap().is_empty());
        assert!(designer.dialog.is_none(), "and it does not ask");

        // Second press: the empty pane goes too.
        designer.request_delete(ids[1]);
        assert_eq!(designer.layout().cells().len(), 1);

        // Both steps are undoable, so a press too many costs nothing.
        designer.undo();
        designer.undo();
        assert_eq!(
            designer
                .layout()
                .cell(ids[1])
                .and_then(|cell| cell.widget.as_ref())
                .map(|widget| widget.kind()),
            Some(WidgetKind::SpeakerNotes)
        );
    }

    #[test]
    fn a_split_holding_further_panes_is_not_removed() {
        // Only the panes on either side of a divider go with it; removing a
        // split that contains more structure would take a subtree.
        let (designer, _, _) = two_aligned_rows();
        let mut designer = designer;
        let root = designer.layout().root.id();
        let inner = designer
            .layout()
            .root
            .as_split()
            .unwrap()
            .children
            .first()
            .unwrap()
            .id();
        assert_ne!(inner, root);

        let before = designer.layout().cells().len();
        designer.request_delete(root);
        assert_eq!(designer.layout().cells().len(), before);
        assert!(designer.status.is_some());
    }

    #[test]
    fn deleting_an_empty_cell_needs_no_confirmation() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.request_delete(ids[1]);
        assert!(designer.dialog.is_none());
        assert_eq!(designer.layout().cells().len(), 1);
    }

    #[test]
    fn every_kind_of_edit_is_undoable() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.place(ids[0], WidgetKind::SpeakerNotes);
        let split = designer.layout().root.id();
        designer.set_child_size(split, 0, 0.7);
        designer.add_pane(ids[1], Edge::Bottom);
        designer.clear_cell(ids[0]);

        let steps = designer.history.depth();
        assert!(steps >= 5, "each action recorded, got {steps}");

        // Undo everything except the split that set the fixture up.
        for _ in 0..steps - 1 {
            designer.undo();
        }
        assert_eq!(designer.layout().cells().len(), 2);
        assert!(designer.layout().cells()[0].is_empty());

        designer.redo();
        assert!(
            !designer.layout().cells()[0].is_empty(),
            "redo brings it back"
        );
    }

    #[test]
    fn undo_repairs_a_selection_that_no_longer_exists() {
        let mut designer = designer_with_two_cells();
        let ids = cells(&designer);
        designer.split(ids[1], Direction::Vertical);
        let deep = *cells(&designer).last().unwrap();
        designer.select(deep);

        designer.undo();
        let selected = designer.selected.expect("something is always selected");
        assert!(
            designer.layout().find(selected).is_some(),
            "the selection points at a live node"
        );
    }

    #[test]
    fn keyboard_resizing_moves_the_focused_divider() {
        let mut designer = designer_with_two_cells();
        let split = designer.layout().root.id();
        designer.select_divider(split, 0);
        let before = designer.layout().split(split).unwrap().sizes[0];

        designer.nudge_divider(1, false);
        let after = designer.layout().split(split).unwrap().sizes[0];
        assert!((after - before - 0.01).abs() < 1e-4, "1% step");

        designer.nudge_divider(-1, true);
        let large = designer.layout().split(split).unwrap().sizes[0];
        assert!((large - after + 0.05).abs() < 1e-4, "5% step with shift");
    }

    #[test]
    fn saving_a_new_layout_asks_for_a_name_first() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = LayoutStore::load(directory.path());
        let mut designer = Designer::create("Untitled layout");
        let ids = cells(&designer);
        designer.place(ids[0], WidgetKind::CurrentSlide);

        assert!(designer.save(&mut store, false).is_none());
        assert!(matches!(designer.dialog, Some(Dialog::NameLayout { .. })));

        let id = designer.save_named(&mut store, "Conference".into());
        assert!(id.is_some());
        assert_eq!(store.custom().len(), 1);
        assert_eq!(store.custom()[0].name, "Conference");
        assert!(!designer.has_unsaved_changes());
    }

    #[test]
    fn a_duplicate_name_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = LayoutStore::load(directory.path());
        let mut first = Designer::create("Untitled layout");
        first.save_named(&mut store, "Taken".into()).unwrap();

        let mut second = Designer::create("Untitled layout");
        assert!(second.save_named(&mut store, "Taken".into()).is_none());
        assert!(second.status.as_ref().unwrap().contains("already exists"));
    }

    #[test]
    fn saving_warns_but_does_not_block_on_an_incomplete_layout() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = LayoutStore::load(directory.path());
        let mut designer = Designer::create("Untitled layout");

        // No current slide, no navigation, an empty cell: all warnings.
        let id = designer.save_named(&mut store, "Sparse".into());
        assert!(id.is_some(), "a presenter may deliberately want this");
        assert!(!designer.issues.is_empty());
        assert!(designer.status.as_ref().unwrap().contains("warning"));
    }

    #[test]
    fn dragging_a_divider_tracks_the_pointer_in_canvas_coordinates() {
        let mut designer = designer_with_two_cells();
        designer.canvas_size = iced::Size::new(800.0, 450.0);
        designer.snap = false;
        let split = designer.layout().root.id();
        let before = designer.layout().split(split).unwrap().sizes[0];

        designer.start_divider_drag(split, 0, Point::new(400.0, 200.0));
        // A quarter of the canvas to the right.
        designer.drag_divider_to(Point::new(600.0, 200.0));
        designer.end_divider_drag();

        let after = designer.layout().split(split).unwrap().sizes[0];
        assert!(
            (after - (before + 0.25)).abs() < 0.02,
            "expected roughly +25%, got {before} → {after}"
        );
    }

    /// Two rows, each divided in two, with their dividers flush.
    fn two_aligned_rows() -> (Designer, NodeId, NodeId) {
        let mut designer = Designer::create("Aligned");
        let root = designer.layout().root.id();
        designer.add_pane(root, Edge::Bottom);
        let rows = cells(&designer);
        designer.add_pane(rows[0], Edge::Right);
        designer.add_pane(rows[1], Edge::Right);

        let splits: Vec<NodeId> = designer
            .layout()
            .root
            .as_split()
            .unwrap()
            .children
            .iter()
            .map(|child| child.id())
            .collect();
        designer.canvas_size = iced::Size::new(800.0, 450.0);
        (designer, splits[0], splits[1])
    }

    #[test]
    fn grabbing_a_junction_moves_the_whole_line() {
        let (mut designer, top, bottom) = two_aligned_rows();
        designer.snap = false;

        // The gutters run vertically down the middle of each row, so their
        // ends are at the top and bottom of that row. Grab near one.
        let frame = designer.divider_frame(top, 0).expect("a gutter");
        let at = Point::new(frame.x + frame.width / 2.0, frame.y + 4.0);
        designer.start_divider_drag(top, 0, at);
        assert!(
            designer
                .dragging_divider
                .as_ref()
                .is_some_and(|drag| drag.whole_line && drag.holds(bottom, 0)),
            "a corner grab takes the line with it"
        );

        designer.drag_divider_to(Point::new(at.x + 80.0, at.y));
        designer.end_divider_drag();

        let top_size = designer.layout().split(top).unwrap().sizes[0];
        let bottom_size = designer.layout().split(bottom).unwrap().sizes[0];
        assert!(top_size > 0.55, "the grabbed row moved, got {top_size}");
        assert!(
            (top_size - bottom_size).abs() < 0.01,
            "the rows stayed flush: {top_size} vs {bottom_size}"
        );
    }

    #[test]
    fn grabbing_the_middle_of_a_gutter_moves_only_that_segment() {
        let (mut designer, top, bottom) = two_aligned_rows();
        designer.snap = false;
        let before = designer.layout().split(bottom).unwrap().sizes[0];

        let frame = designer.divider_frame(top, 0).expect("a gutter");
        let at = Point::new(frame.x + frame.width / 2.0, frame.y + frame.height / 2.0);
        designer.start_divider_drag(top, 0, at);
        assert!(
            designer
                .dragging_divider
                .as_ref()
                .is_some_and(|drag| !drag.whole_line),
            "the middle is a single-segment grab"
        );

        designer.drag_divider_to(Point::new(at.x + 80.0, at.y));
        designer.end_divider_drag();

        let after = designer.layout().split(bottom).unwrap().sizes[0];
        assert!(
            (after - before).abs() < 1e-3,
            "the other row is untouched: {before} → {after}"
        );
    }

    #[test]
    fn leaving_with_unsaved_changes_asks_before_the_work_is_lost() {
        // There is no discard button: this dialog is the only place the
        // question is asked, and it is asked at the moment it matters.
        let directory = tempfile::tempdir().unwrap();
        let mut store = LayoutStore::load(directory.path());
        let mut designer = Designer::create("Never saved");
        designer.place(designer.layout().root.id(), WidgetKind::CurrentSlide);
        assert!(designer.has_unsaved_changes());

        assert_eq!(designer.handle(Msg::Back, &mut store), Effect::None);
        assert_eq!(designer.dialog, Some(Dialog::UnsavedChanges));

        assert_eq!(designer.handle(Msg::ForceBack, &mut store), Effect::Back);
    }

    #[test]
    fn saving_from_the_leaving_dialog_saves_and_leaves() {
        // It used to save and stay, with the question still on the screen,
        // which is indistinguishable from a button that does nothing.
        let directory = tempfile::tempdir().unwrap();
        let mut store = LayoutStore::load(directory.path());
        let mut designer = Designer::create("Already named");
        designer.place(designer.layout().root.id(), WidgetKind::CurrentSlide);
        designer
            .save_named(&mut store, "Already named".into())
            .unwrap();
        designer.add_pane(designer.layout().root.id(), Edge::Right);
        assert!(designer.has_unsaved_changes());

        assert_eq!(designer.handle(Msg::Back, &mut store), Effect::None);
        assert_eq!(
            designer.handle(Msg::SaveAndLeave, &mut store),
            Effect::Back,
            "it saves and then goes"
        );
        assert_eq!(designer.dialog, None, "and takes its question with it");
        assert!(!designer.has_unsaved_changes());
    }

    #[test]
    fn saving_a_layout_that_needs_a_name_still_leaves_afterwards() {
        // The name is asked for in place of the leaving question, and
        // answering it carries the "then leave" through.
        let directory = tempfile::tempdir().unwrap();
        let mut store = LayoutStore::load(directory.path());
        let mut designer = Designer::create("Untitled layout");
        designer.place(designer.layout().root.id(), WidgetKind::CurrentSlide);

        assert_eq!(designer.handle(Msg::Back, &mut store), Effect::None);
        assert_eq!(designer.handle(Msg::SaveAndLeave, &mut store), Effect::None);
        assert!(matches!(
            designer.dialog,
            Some(Dialog::NameLayout {
                then_close: true,
                ..
            })
        ));

        designer.handle(Msg::NameInput("Conference".into()), &mut store);
        assert_eq!(designer.handle(Msg::CommitName, &mut store), Effect::Back);
        assert!(store.all().any(|layout| layout.name == "Conference"));
    }

    #[test]
    fn a_drag_can_pull_free_of_a_snap_point() {
        // A split starts at 50/50, which is a snap point. Dragging a little
        // should hold there; dragging well past it should let go.
        let mut designer = designer_with_two_cells();
        designer.canvas_size = iced::Size::new(800.0, 450.0);
        designer.snap = true;
        let split = designer.layout().root.id();

        designer.start_divider_drag(split, 0, Point::new(400.0, 200.0));

        // A nudge of about 1% of the canvas: inside the snap's pull.
        designer.drag_divider_to(Point::new(408.0, 200.0));
        let held = designer.layout().split(split).unwrap().sizes[0];
        assert!(
            (held - 0.5).abs() < 1e-3,
            "the snap should still hold at {held}"
        );

        // Keep going, well past the tolerance, one step at a time as a real
        // pointer would.
        for x in [420.0, 440.0, 460.0, 480.0] {
            designer.drag_divider_to(Point::new(x, 200.0));
        }
        let escaped = designer.layout().split(split).unwrap().sizes[0];
        designer.end_divider_drag();

        assert!(
            escaped > 0.55,
            "the drag should have pulled free of 50%, got {escaped}"
        );
    }

    #[test]
    fn the_deck_shape_is_previewed_independently_of_the_presenter_screen() {
        // A 4:3 deck on a 16:9 presenter screen: the editor must be able to
        // show this without that projector being attached.
        let mut designer = Designer::open(slide_time_below());
        designer.canvas_ratio = AspectRatio::SixteenNine;
        let before = designer.layout().clone();

        designer.slide_ratio = AspectRatio::SixteenNine;
        let wide = designer.space_report().wasted_fraction();
        designer.slide_ratio = AspectRatio::FourThree;
        let classic = designer.space_report().wasted_fraction();

        assert!(
            classic > wide,
            "a 4:3 deck leaves more of a wide slide pane empty: {classic} vs {wide}"
        );
        assert_eq!(designer.layout(), &before, "and the layout is untouched");
    }

    #[test]
    fn a_layout_that_letterboxes_most_of_its_slide_pane_says_so() {
        let mut designer = Designer::open(slide_time_below());
        designer.slide_ratio = AspectRatio::FourThree;

        let warning = designer.space_warning().expect("this wastes real space");
        assert!(warning.contains('%'), "{warning}");
        // Factual, and it does not tell anyone what to do about it.
        assert!(!warning.to_lowercase().contains("should"), "{warning}");
    }

    #[test]
    fn a_layout_that_fits_the_deck_is_not_warned_about() {
        let mut designer = Designer::open(slide_notes_beside());
        // A 75/25 split of a 16:9 screen gives the slide an exactly 4:3 pane.
        designer.slide_ratio = AspectRatio::FourThree;
        assert_eq!(designer.space_warning(), None);
    }

    #[test]
    fn recommendations_are_ranked_and_nothing_is_selected_by_showing_them() {
        let mut designer = Designer::create("Mine");
        designer.place(designer.layout().root.id(), WidgetKind::CurrentSlide);
        designer.slide_ratio = AspectRatio::FourThree;
        let before = designer.layout().clone();

        let ranked = designer.recommendations();
        assert_eq!(ranked.len(), 6, "one per built-in");
        for pair in ranked.windows(2) {
            assert!(
                pair[0].wasted_fraction <= pair[1].wasted_fraction,
                "ranked best first"
            );
        }

        // Ranking is reading, not choosing: nothing was applied, nothing was
        // marked as chosen, and the layout in hand is exactly as it was.
        assert_eq!(designer.layout(), &before);
        assert_eq!(designer.previewed_recommendation, None);
    }

    #[test]
    fn pressing_a_recommendation_shows_it_and_changes_no_layout() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = LayoutStore::load(directory.path());
        let mut designer = Designer::create("Mine");
        designer.place(designer.layout().root.id(), WidgetKind::CurrentSlide);
        let before = designer.layout().clone();

        let suggested = LayoutId("slide-notes-beside".to_string());
        designer.handle(Msg::PreviewRecommendation(suggested.clone()), &mut store);

        assert_eq!(designer.previewed_recommendation, Some(suggested));
        assert_eq!(designer.layout(), &before, "looking is not adopting");

        designer.handle(Msg::DismissRecommendation, &mut store);
        assert_eq!(designer.previewed_recommendation, None);
    }

    #[test]
    fn the_preview_ratio_is_a_design_aid_and_does_not_change_the_layout() {
        let mut designer = designer_with_two_cells();
        let before = designer.layout().root.clone();
        designer.canvas_ratio = AspectRatio::FourThree;
        designer.revalidate();
        assert_eq!(designer.layout().root, before, "the tree is untouched");
    }
}

/// Everything the editor UI can ask for.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    // structure
    /// Add an empty pane on one side of the selected pane.
    AddPane(NodeId, Edge),
    RequestDelete(NodeId),
    Equalize(NodeId),
    // drag and drop
    StartDrag(DragSource),
    Hover(NodeId),
    /// A press on a pane: it becomes the selection, and if it holds a widget
    /// the press also begins a drag of that widget.
    PressCell(NodeId),
    /// The pointer is over an edge zone of a pane while dragging.
    HoverEdge(NodeId, Edge),
    Drop(NodeId),
    /// Drop on an edge: split that pane and place the widget in the new half.
    DropOnEdge(NodeId, Edge),
    CancelDrag,
    ResolveDrop(DropChoice),
    // dividers
    /// Press on a divider: take hold of it at the current pointer position.
    /// Nothing moves until the button is down — a divider must be dragged,
    /// not brushed past.
    DividerGrab(NodeId, usize),
    PointerMoved(Point),
    DividerReleased,
    /// The button came up over the canvas but not over a pane.
    PointerReleased,
    CanvasResized(iced::Size),
    /// Preview the deck at another shape. Affects what is drawn, never the
    /// layout.
    SetSlideRatio(AspectRatio),
    /// Show a recommended built-in beside the layout being edited.
    PreviewRecommendation(LayoutId),
    DismissRecommendation,
    ToggleSnap(bool),
    NudgeDivider(i32, bool),
    // naming
    NameInput(String),
    CommitName,
    /// Press the title: rename the layout being edited.
    StartRenameLayout,
    RenameLayoutInput(String),
    CommitRenameLayout,
    // session
    Undo,
    Redo,
    Save,
    /// Save and leave: the "Save" of the unsaved-changes question.
    SaveAndLeave,
    DuplicateToCustomize,
    Back,
    ForceBack,
    CloseDialog,
    DismissStatus,
}

/// What the application must do after a designer message.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    /// Return to the layout library.
    Back,
    /// A layout was saved; refresh anything showing it.
    Saved(LayoutId),
    /// Make a copy of the layout being edited and open it.
    Duplicate,
}

impl Designer {
    /// Handle one editor message.
    pub fn handle(&mut self, message: Msg, store: &mut LayoutStore) -> Effect {
        // Any deliberate action clears a stale status line.
        if !matches!(message, Msg::PointerMoved(_) | Msg::CanvasResized(_)) {
            self.status = None;
        }
        match message {
            Msg::AddPane(node, side) => self.add_pane(node, side),
            Msg::RequestDelete(node) => self.request_delete(node),
            Msg::Equalize(split) => self.equalize(split),
            Msg::StartDrag(source) => self.start_drag(source),
            Msg::PressCell(cell) => self.press_cell(cell),
            Msg::Hover(cell) => self.hover(cell),
            Msg::HoverEdge(cell, edge) => self.hover_edge(cell, Some(edge)),
            Msg::Drop(cell) => self.drop_on(cell),
            Msg::DropOnEdge(cell, edge) => self.drop_on_edge(cell, edge),
            Msg::CancelDrag => self.cancel_drag(),
            Msg::ResolveDrop(choice) => self.resolve_drop(choice),
            Msg::DividerGrab(split, index) => {
                let at = self.pointer;
                self.start_divider_drag(split, index, at);
            }
            Msg::PointerMoved(at) => self.drag_divider_to(at),
            Msg::DividerReleased => self.end_divider_drag(),
            Msg::PointerReleased => {
                self.end_divider_drag();
                self.cancel_drag();
            }
            Msg::CanvasResized(size) => self.canvas_size = size,
            Msg::SetSlideRatio(ratio) => self.slide_ratio = ratio,
            Msg::PreviewRecommendation(id) => self.preview_recommendation(id),
            Msg::DismissRecommendation => self.previewed_recommendation = None,
            Msg::ToggleSnap(value) => self.snap = value,
            Msg::NudgeDivider(steps, large) => self.nudge_divider(steps, large),
            Msg::NameInput(text) => {
                if let Some(Dialog::NameLayout { name, .. }) = self.dialog.as_mut() {
                    *name = text;
                }
            }
            Msg::CommitName => {
                if let Some(Dialog::NameLayout { name, then_close }) = self.dialog.clone() {
                    if let Some(id) = self.save_named(store, name) {
                        return if then_close {
                            Effect::Back
                        } else {
                            Effect::Saved(id)
                        };
                    }
                }
            }
            // Renaming is an edit of the layout in hand, not an operation on
            // the library: it goes through the history like everything else,
            // is undoable, and reaches the file when the layout is saved. The
            // library's own rename dialog is not drawn over the editor, which
            // is why pressing the title used to appear to do nothing at all.
            Msg::StartRenameLayout => {
                if self.is_editable() {
                    self.dialog = Some(Dialog::RenameLayout {
                        name: self.layout().name.clone(),
                    });
                } else {
                    self.status = Some("A built-in layout keeps its name.".into());
                }
            }
            Msg::RenameLayoutInput(text) => {
                if let Some(Dialog::RenameLayout { name }) = self.dialog.as_mut() {
                    *name = text;
                }
            }
            Msg::CommitRenameLayout => {
                if let Some(Dialog::RenameLayout { name }) = self.dialog.clone() {
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        self.status = Some("A layout needs a name.".into());
                        return Effect::None;
                    }
                    // Checked here rather than only at save time: finding out
                    // that the name was taken after building a layout under
                    // it is a worse moment to be told.
                    let taken = store.all().any(|layout| {
                        layout.name.eq_ignore_ascii_case(&name) && layout.id != self.layout().id
                    });
                    if taken {
                        self.status = Some(format!("A layout named “{name}” already exists."));
                        return Effect::None;
                    }
                    self.edit(|layout| layout.name = name);
                    self.dialog = None;
                }
            }
            Msg::Undo => self.undo(),
            Msg::Redo => self.redo(),
            Msg::Save => {
                if let Some(id) = self.save(store, false) {
                    return Effect::Saved(id);
                }
            }
            // "Save" in the leaving dialog has to do both halves, and it has
            // to put its own dialog away first: saving used to leave the
            // question on the screen and stay in the editor, which looked
            // exactly like a button that did nothing. If a name is still
            // needed, that dialog replaces this one and carries the
            // "then leave" with it.
            Msg::SaveAndLeave => {
                self.dialog = None;
                if self.save(store, true).is_some() {
                    return Effect::Back;
                }
            }
            Msg::DuplicateToCustomize => return Effect::Duplicate,
            Msg::Back => {
                if self.has_unsaved_changes() {
                    self.dialog = Some(Dialog::UnsavedChanges);
                } else {
                    return Effect::Back;
                }
            }
            Msg::ForceBack => return Effect::Back,
            Msg::CloseDialog => self.dialog = None,
            Msg::DismissStatus => self.status = None,
        }
        Effect::None
    }
}
