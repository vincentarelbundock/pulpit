//! A layout and every editing operation the designer performs on it.

use serde::{Deserialize, Serialize};

use crate::layout::tree::{
    compute, normalise, Cell, CellBackground, CellBorder, Direction, Divider, EmptyBehavior, Frame,
    Node, NodeId, Placement, Split,
};
use crate::widgets::{Widget, WidgetCapability, WidgetKind};

/// The viewer that gives a layout its navigation surface.
///
/// This is derived from the widgets every time; it is not an application mode
/// or persisted layout metadata. All other widgets and interactions are
/// shared. A layout containing a continuous document surface uses page
/// navigation; every other layout uses the fitted single-page surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimaryViewer {
    Slide,
    Document,
}

impl PrimaryViewer {
    pub fn of(layout: &Layout) -> PrimaryViewer {
        if layout.widgets().iter().any(|widget| {
            widget
                .kind()
                .has_capability(WidgetCapability::ShowsDocument)
        }) {
            PrimaryViewer::Document
        } else {
            PrimaryViewer::Slide
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PrimaryViewer::Slide => "slide viewer",
            PrimaryViewer::Document => "document viewer",
        }
    }
}

/// The aspect ratio the canvas previews. A design aid: layouts are stored
/// proportionally and scale to whatever screen they land on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum AspectRatio {
    #[default]
    SixteenNine,
    SixteenTen,
    FourThree,
    /// A ratio detected from a connected monitor.
    Detected {
        width: u32,
        height: u32,
    },
}

impl AspectRatio {
    pub const PRESETS: [AspectRatio; 3] = [
        AspectRatio::SixteenNine,
        AspectRatio::SixteenTen,
        AspectRatio::FourThree,
    ];

    pub fn ratio(self) -> f32 {
        match self {
            AspectRatio::SixteenNine => 16.0 / 9.0,
            AspectRatio::SixteenTen => 16.0 / 10.0,
            AspectRatio::FourThree => 4.0 / 3.0,
            AspectRatio::Detected { width, height } => {
                if height == 0 {
                    16.0 / 9.0
                } else {
                    width as f32 / height as f32
                }
            }
        }
    }

    pub fn label(self) -> String {
        match self {
            AspectRatio::SixteenNine => "16:9".into(),
            AspectRatio::SixteenTen => "16:10".into(),
            AspectRatio::FourThree => "4:3".into(),
            AspectRatio::Detected { width, height } => format!("{width}×{height} (detected)"),
        }
    }

    /// Is the real screen far enough from the design ratio to be worth a
    /// one-time notice? A portrait screen for a 16:9 layout, for instance.
    pub fn differs_substantially_from(self, other: AspectRatio) -> bool {
        let (a, b) = (self.ratio(), other.ratio());
        if a <= 0.0 || b <= 0.0 {
            return false;
        }
        let larger = a.max(b);
        let smaller = a.min(b);
        larger / smaller > 1.25
    }
}

/// Where a layout came from. Built-ins cannot be renamed, overwritten or
/// deleted; a copy is always a custom layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    BuiltIn,
    Custom,
}

impl Origin {
    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub fn label(self) -> &'static str {
        match self {
            Origin::BuiltIn => "Built-in layout · Read only",
            Origin::Custom => "Custom layout",
        }
    }

    pub fn is_editable(self) -> bool {
        matches!(self, Origin::Custom)
    }
}

/// Identifier of a layout: a slug, unique within the store.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LayoutId(pub String);

impl LayoutId {
    pub fn from_name(name: &str) -> LayoutId {
        let slug: String = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let trimmed = slug.trim_matches('-').to_string();
        LayoutId(if trimmed.is_empty() {
            "layout".into()
        } else {
            trimmed
        })
    }
}

impl std::fmt::Display for LayoutId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which side of an existing pane a new one goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Before,
    After,
}

/// What a structural operation did, so the caller can report it.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The new node that resulted, if any.
    Created(NodeId),
    Removed(NodeId),
    Updated(NodeId),
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EditError {
    #[error("no node {0}")]
    NoSuchNode(NodeId),
    #[error("{0} is a split, not a cell")]
    NotACell(NodeId),
    #[error("{0} is a cell, not a split")]
    NotASplit(NodeId),
    #[error("the root cell cannot be deleted")]
    CannotDeleteRoot,
    #[error("{0} may only appear once in a layout")]
    AlreadyPlaced(&'static str),
    #[error("this layout is read only")]
    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    ReadOnly,
}

/// What mounting a layout asks of the reading surface, beyond its tree.
///
/// A layout is a widget tree and nothing else, but two of the reader's states
/// are as much a part of "which layout am I in" as the tree is: whether the
/// chrome is hidden and how the page is fitted. Keeping them here rather than
/// as a hard-coded exception for one built-in id means a user's copy of a
/// layout opens the way its original does, which is what §2.3 already
/// promises for the mode a layout belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct OnMount {
    /// Mount with the reader's chrome hidden and the page on the screen.
    #[serde(default)]
    pub fullscreen: bool,
    /// The fit to put the page in when the layout is mounted. `None` leaves
    /// the reader in whatever fit it was already using.
    #[serde(default)]
    pub zoom: Option<crate::widgets::document::model::Zoom>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub id: LayoutId,
    pub name: String,
    pub origin: Origin,
    /// The ratio the layout was designed at, kept for the review notice.
    #[serde(default)]
    pub design_ratio: AspectRatio,
    /// What mounting this layout asks of the reading surface.
    #[serde(default)]
    pub on_mount: OnMount,
    pub root: Node,
    /// Next node id to hand out.
    #[serde(default = "default_next_id")]
    next_id: u32,
}

fn default_next_id() -> u32 {
    1000
}

impl Layout {
    /// Assemble a layout from parts. Used by the built-ins and by import.
    pub fn from_parts(
        id: LayoutId,
        name: String,
        origin: Origin,
        design_ratio: AspectRatio,
        root: Node,
    ) -> Layout {
        let mut layout = Layout {
            id,
            name,
            origin,
            design_ratio,
            on_mount: OnMount::default(),
            root,
            next_id: 0,
        };
        layout.renumber();
        layout
    }

    /// A new custom layout: one empty root cell.
    pub fn empty(name: &str) -> Layout {
        Layout {
            id: LayoutId::from_name(name),
            name: name.to_string(),
            origin: Origin::Custom,
            design_ratio: AspectRatio::default(),
            on_mount: OnMount::default(),
            root: Node::Leaf(Cell::new(NodeId(0))),
            next_id: 1,
        }
    }

    pub fn is_editable(&self) -> bool {
        self.origin.is_editable()
    }

    fn allocate(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Re-number every node and make `next_id` safe. Used after import and
    /// duplication so ids from another file cannot collide.
    pub fn renumber(&mut self) {
        let mut next = 0;
        renumber_node(&mut self.root, &mut next);
        self.next_id = next;
    }

    pub fn find(&self, id: NodeId) -> Option<&Node> {
        self.root.find(id)
    }

    pub fn cell(&self, id: NodeId) -> Option<&Cell> {
        self.root.find(id)?.as_cell()
    }

    pub fn split(&self, id: NodeId) -> Option<&Split> {
        self.root.find(id)?.as_split()
    }

    pub fn cells(&self) -> Vec<&Cell> {
        self.root.cells()
    }

    pub fn widgets(&self) -> Vec<&Widget> {
        self.root.widgets()
    }

    pub fn is_canonical(&self) -> bool {
        self.root.is_canonical()
    }

    /// Geometry for drawing or validating. `collapse_empty` distinguishes
    /// presentation time from the editor.
    pub fn compute(&self, area: Frame, collapse_empty: bool) -> (Vec<Placement>, Vec<Divider>) {
        compute(&self.root, area, collapse_empty)
    }

    /// The frame of one node inside `area`.
    pub fn frame_of(&self, id: NodeId, area: Frame, collapse_empty: bool) -> Option<Frame> {
        self.compute(area, collapse_empty)
            .0
            .into_iter()
            .find(|placement| placement.id == id)
            .map(|placement| placement.frame)
    }

    /// The cell at a point, for hit testing in the editor.
    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn cell_at(&self, x: f32, y: f32, area: Frame) -> Option<NodeId> {
        let (placements, _) = self.compute(area, false);
        placements
            .into_iter()
            .filter(|placement| self.cell(placement.id).is_some())
            .find(|placement| placement.frame.contains(x, y))
            .map(|placement| placement.id)
    }

    // ---------------------------------------------------------------- splits

    /// Split a cell in two.
    ///
    /// The canonical rule: splitting in the same direction as the parent
    /// inserts a divider into the *parent*, so a three-across row becomes
    /// four across rather than three-across-with-a-nested-pair. Splitting
    /// perpendicular always nests. The selected cell's space is halved and
    /// its siblings keep their sizes.
    pub fn split_cell(&mut self, id: NodeId, direction: Direction) -> Result<Change, EditError> {
        self.split_cell_at(id, direction, Side::After)
    }

    /// Split a cell, choosing which side the new pane lands on.
    ///
    /// Dropping a widget on the left edge of a pane has to put it on the
    /// *left*; `split_cell` always appends, which would put it on the right
    /// and quietly do the opposite of what the pointer said.
    pub fn split_cell_at(
        &mut self,
        id: NodeId,
        direction: Direction,
        side: Side,
    ) -> Result<Change, EditError> {
        if self.cell(id).is_none() {
            return Err(if self.find(id).is_some() {
                EditError::NotACell(id)
            } else {
                EditError::NoSuchNode(id)
            });
        }
        let new_id = self.allocate();
        let parent = self.root.parent_of(id);

        // Same direction as the parent split: flatten into the parent.
        if let Some(parent_id) = parent {
            let same_direction = self
                .split(parent_id)
                .is_some_and(|split| split.direction == direction);
            if same_direction {
                let split = self.root.split_mut(parent_id).expect("checked above");
                let index = split
                    .children
                    .iter()
                    .position(|child| child.id() == id)
                    .expect("parent_of found it");
                let half = split.sizes[index] / 2.0;
                split.sizes[index] = half;
                let at = match side {
                    Side::Before => index,
                    Side::After => index + 1,
                };
                split.children.insert(at, Node::Leaf(Cell::new(new_id)));
                split.sizes.insert(at, half);
                normalise(&mut split.sizes);
                return Ok(Change::Created(new_id));
            }
        }

        // Otherwise nest a new split in place of the cell.
        let split_id = self.allocate();
        let node = self.root.find_mut(id).expect("checked above");
        let existing = node.clone();
        let fresh = Node::Leaf(Cell::new(new_id));
        let children = match side {
            Side::Before => vec![fresh, existing],
            Side::After => vec![existing, fresh],
        };
        *node = Node::Split(Split {
            id: split_id,
            name: None,
            direction,
            children,
            sizes: vec![0.5, 0.5],
            gap: crate::widgets::tokens::SPLIT_GAP,
            min_child: 0.05,
        });
        Ok(Change::Created(new_id))
    }

    /// Remove a node and give its space to its siblings in proportion.
    ///
    /// When a split is left with one child, the child replaces it — and if
    /// that child is a split of the same direction as *its* new parent, the
    /// tree is flattened again so it stays canonical.
    pub fn delete_node(&mut self, id: NodeId) -> Result<Change, EditError> {
        if self.find(id).is_none() {
            return Err(EditError::NoSuchNode(id));
        }
        let Some(parent_id) = self.root.parent_of(id) else {
            return Err(EditError::CannotDeleteRoot);
        };

        let split = self.root.split_mut(parent_id).expect("a parent is a split");
        let index = split
            .children
            .iter()
            .position(|child| child.id() == id)
            .expect("parent_of found it");
        split.children.remove(index);
        split.sizes.remove(index);
        normalise(&mut split.sizes);

        if split.children.len() == 1 {
            let survivor = split.children.remove(0);
            let node = self.root.find_mut(parent_id).expect("still there");
            *node = survivor;
        }
        self.flatten();
        Ok(Change::Removed(id))
    }

    /// Every widget that would be destroyed by deleting this node, so the
    /// confirmation dialog can list them.
    pub fn widgets_in(&self, id: NodeId) -> Vec<WidgetKind> {
        self.find(id)
            .map(|node| node.widgets().iter().map(|widget| widget.kind()).collect())
            .unwrap_or_default()
    }

    /// Restore the canonical form everywhere: merge a split into its parent
    /// when they share a direction, and dissolve one-child splits.
    pub fn flatten(&mut self) {
        while flatten_node(&mut self.root) {}
    }

    // ----------------------------------------------------------------- cells

    pub fn set_widget(&mut self, id: NodeId, widget: Widget) -> Result<Change, EditError> {
        // Instance limits are checked against everything except the cell
        // being written, so replacing a widget with itself is always allowed.
        self.check_instances(&widget, Some(id))?;
        let mut widget = widget;
        widget.sanitise();
        let cell = self.root.cell_mut(id).ok_or(EditError::NotACell(id))?;
        cell.widget = Some(widget);
        Ok(Change::Updated(id))
    }

    /// Remove the widget, leaving the cell empty. Structure is untouched.
    pub fn clear_cell(&mut self, id: NodeId) -> Result<Change, EditError> {
        let cell = self.root.cell_mut(id).ok_or(EditError::NotACell(id))?;
        cell.widget = None;
        Ok(Change::Updated(id))
    }

    /// Move a widget from one cell to another, replacing whatever is there.
    pub fn move_widget(&mut self, from: NodeId, to: NodeId) -> Result<Change, EditError> {
        if from == to {
            return Ok(Change::Updated(to));
        }
        let widget = self
            .cell(from)
            .ok_or(EditError::NotACell(from))?
            .widget
            .clone();
        let Some(widget) = widget else {
            return Ok(Change::Updated(to));
        };
        self.root
            .cell_mut(to)
            .ok_or(EditError::NotACell(to))?
            .widget = Some(widget);
        self.root
            .cell_mut(from)
            .ok_or(EditError::NotACell(from))?
            .widget = None;
        Ok(Change::Updated(to))
    }

    /// Exchange the contents of two cells.
    pub fn swap_widgets(&mut self, a: NodeId, b: NodeId) -> Result<Change, EditError> {
        if a == b {
            return Ok(Change::Updated(a));
        }
        let first = self.cell(a).ok_or(EditError::NotACell(a))?.widget.clone();
        let second = self.cell(b).ok_or(EditError::NotACell(b))?.widget.clone();
        self.root.cell_mut(a).expect("checked").widget = second;
        self.root.cell_mut(b).expect("checked").widget = first;
        Ok(Change::Updated(a))
    }

    /// Would placing this widget break a single-instance rule?
    ///
    /// `ignoring` excludes one cell from the count, which is what makes
    /// replacing a widget in place legal.
    pub fn check_instances(
        &self,
        widget: &Widget,
        ignoring: Option<NodeId>,
    ) -> Result<(), EditError> {
        for kind in widget.kind().occupies() {
            if kind.multi_instance() {
                continue;
            }
            let already = self.cells().into_iter().any(|cell| {
                if Some(cell.id) == ignoring {
                    return false;
                }
                cell.widget
                    .as_ref()
                    .is_some_and(|existing| existing.kind().occupies().contains(&kind))
            });
            if already {
                return Err(EditError::AlreadyPlaced(kind.label()));
            }
        }
        Ok(())
    }

    /// Is this widget already in the layout, for the library card's
    /// "Already in Layout" state?
    pub fn already_placed(&self, kind: WidgetKind) -> bool {
        self.check_instances(&Widget::new(kind), None).is_err()
    }

    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn set_cell_properties(
        &mut self,
        id: NodeId,
        padding: f32,
        background: CellBackground,
        border: CellBorder,
        empty_behavior: EmptyBehavior,
    ) -> Result<Change, EditError> {
        let cell = self.root.cell_mut(id).ok_or(EditError::NotACell(id))?;
        cell.padding = padding.clamp(0.0, 64.0);
        cell.background = background;
        cell.border = border;
        cell.empty_behavior = empty_behavior;
        Ok(Change::Updated(id))
    }

    /// Replace the widget in a cell with an edited copy of itself.
    ///
    /// The whole widget, not a property bag: a kind and its configuration
    /// travel together and cannot be separated by a caller.
    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub fn update_widget(&mut self, id: NodeId, widget: Widget) -> Result<Change, EditError> {
        let cell = self.root.cell_mut(id).ok_or(EditError::NotACell(id))?;
        if cell.widget.is_none() {
            return Ok(Change::Updated(id));
        }
        let mut widget = widget;
        widget.sanitise();
        cell.widget = Some(widget);
        Ok(Change::Updated(id))
    }

    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn rename_node(&mut self, id: NodeId, name: Option<String>) -> Result<Change, EditError> {
        let node = self.root.find_mut(id).ok_or(EditError::NoSuchNode(id))?;
        *node.name_mut() = name.filter(|name| !name.trim().is_empty());
        Ok(Change::Updated(id))
    }

    // --------------------------------------------------------------- resizing

    /// Sizes at which a divider snaps, as a fraction of the whole split.
    pub const SNAP_POINTS: [f32; 5] = [0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75];
    const SNAP_TOLERANCE: f32 = 0.02;

    /// Move divider `index` of a split by `delta` (a fraction of the split's
    /// full span). Only the two adjacent children change; the rest keep their
    /// sizes exactly.
    /// How close, as a fraction of the whole canvas, counts as "lined up".
    /// Mild on purpose: it should catch a near miss, not fight a deliberate
    /// choice a couple of percent away.
    const ALIGNMENT_TOLERANCE: f32 = 0.012;
    /// A nominal canvas for geometry: gaps are in points, so a unit square
    /// would be entirely gap.
    const REFERENCE: Frame = Frame {
        x: 0.0,
        y: 0.0,
        width: 1600.0,
        height: 900.0,
    };

    /// The canvas positions of every *other* divider running the same way as
    /// the one in `split_id`, for alignment snapping.
    fn alignment_targets(&self, split_id: NodeId) -> Vec<f32> {
        let Some(direction) = self.split(split_id).map(|split| split.direction) else {
            return Vec::new();
        };
        let (_, dividers) = self.compute(Self::REFERENCE, false);
        dividers
            .into_iter()
            .filter(|divider| divider.split != split_id && divider.direction == direction)
            .map(|divider| match direction {
                // The divider's frame is the gutter itself; its middle is the
                // line the eye reads as the edge.
                Direction::Horizontal => {
                    (divider.frame.x + divider.frame.width / 2.0) / Self::REFERENCE.width
                }
                Direction::Vertical => {
                    (divider.frame.y + divider.frame.height / 2.0) / Self::REFERENCE.height
                }
            })
            .collect()
    }

    /// Move a divider by a delta. Convenience over `resize_divider_to`.
    pub fn resize_divider(
        &mut self,
        split_id: NodeId,
        index: usize,
        delta: f32,
        snap: bool,
    ) -> Result<Change, EditError> {
        let current = self
            .split(split_id)
            .and_then(|split| split.sizes.get(index).copied())
            .ok_or(EditError::NotASplit(split_id))?;
        self.resize_divider_to(split_id, index, current + delta, snap)
    }

    /// Put a divider at an exact position, snapping if asked.
    ///
    /// The caller keeps the *unsnapped* position it is asking for, which is
    /// what lets a drag escape a snap: the pull holds while the pointer is
    /// near, and lets go once it is not.
    pub fn resize_divider_to(
        &mut self,
        split_id: NodeId,
        index: usize,
        before: f32,
        snap: bool,
    ) -> Result<Change, EditError> {
        // Where the other dividers of the same direction sit, in canvas
        // fractions, so this one can line up with them. Gathered before the
        // borrow that mutates.
        let alignments = if snap {
            self.alignment_targets(split_id)
        } else {
            Vec::new()
        };
        let frame = self.frame_of(split_id, Self::REFERENCE, false);

        let split = self
            .root
            .split_mut(split_id)
            .ok_or(EditError::NotASplit(split_id))?;
        if index + 1 >= split.children.len() {
            return Ok(Change::Updated(split_id));
        }
        let minimum = split.min_child.clamp(0.01, 0.4);
        let pair = split.sizes[index] + split.sizes[index + 1];

        let mut new_before = before.clamp(minimum, pair - minimum);

        if snap {
            // Snap on the divider's absolute position within the split, so
            // the thresholds mean the same thing regardless of how many
            // children there are.
            let leading: f32 = split.sizes[..index].iter().sum();
            let position = leading + new_before;
            if let Some(target) = Self::SNAP_POINTS
                .iter()
                .copied()
                .find(|point| (position - point).abs() <= Self::SNAP_TOLERANCE)
            {
                let snapped = target - leading;
                if snapped >= minimum && pair - snapped >= minimum {
                    new_before = snapped;
                }
            }

            // Then alignment: a divider that nearly lines up with one in the
            // row above should line up with it. Columns that are almost but
            // not quite flush are the commonest way a hand-built layout looks
            // wrong, and nudging by eye cannot fix it.
            if let Some(frame) = frame {
                let (origin, extent) = match split.direction {
                    Direction::Horizontal => (
                        frame.x / Self::REFERENCE.width,
                        frame.width / Self::REFERENCE.width,
                    ),
                    Direction::Vertical => (
                        frame.y / Self::REFERENCE.height,
                        frame.height / Self::REFERENCE.height,
                    ),
                };
                if extent > 0.0 {
                    let leading: f32 = split.sizes[..index].iter().sum();
                    let absolute = origin + (leading + new_before) * extent;
                    if let Some(target) = alignments
                        .iter()
                        .copied()
                        .filter(|candidate| {
                            (absolute - candidate).abs() <= Self::ALIGNMENT_TOLERANCE
                        })
                        .min_by(|a, b| (absolute - a).abs().total_cmp(&(absolute - b).abs()))
                    {
                        let snapped = (target - origin) / extent - leading;
                        if snapped >= minimum && pair - snapped >= minimum {
                            new_before = snapped;
                        }
                    }
                }
            }
        }

        split.sizes[index] = new_before;
        split.sizes[index + 1] = pair - new_before;
        Ok(Change::Updated(split_id))
    }

    /// Set one child's size directly, from the properties panel.
    pub fn set_child_size(
        &mut self,
        split_id: NodeId,
        index: usize,
        size: f32,
    ) -> Result<Change, EditError> {
        let split = self
            .root
            .split_mut(split_id)
            .ok_or(EditError::NotASplit(split_id))?;
        if index >= split.sizes.len() {
            return Ok(Change::Updated(split_id));
        }
        let minimum = split.min_child.clamp(0.01, 0.4);
        let clamped = size.clamp(minimum, 1.0 - minimum);
        let others: f32 = split.sizes.iter().sum::<f32>() - split.sizes[index];
        let remaining = 1.0 - clamped;
        if others > 0.0 {
            let scale = remaining / others;
            for (position, value) in split.sizes.iter_mut().enumerate() {
                if position != index {
                    *value *= scale;
                }
            }
        }
        split.sizes[index] = clamped;
        normalise(&mut split.sizes);
        Ok(Change::Updated(split_id))
    }

    /// Distribute a split's space evenly. Double-clicking a divider does this.
    pub fn equalize(&mut self, split_id: NodeId) -> Result<Change, EditError> {
        let split = self
            .root
            .split_mut(split_id)
            .ok_or(EditError::NotASplit(split_id))?;
        let even = 1.0 / split.children.len() as f32;
        split.sizes.iter_mut().for_each(|size| *size = even);
        Ok(Change::Updated(split_id))
    }

    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn reverse_children(&mut self, split_id: NodeId) -> Result<Change, EditError> {
        let split = self
            .root
            .split_mut(split_id)
            .ok_or(EditError::NotASplit(split_id))?;
        split.children.reverse();
        split.sizes.reverse();
        Ok(Change::Updated(split_id))
    }

    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn set_split_properties(
        &mut self,
        split_id: NodeId,
        direction: Direction,
        gap: f32,
        min_child: f32,
    ) -> Result<Change, EditError> {
        // Changing direction can break the canonical form, so flatten after.
        {
            let split = self
                .root
                .split_mut(split_id)
                .ok_or(EditError::NotASplit(split_id))?;
            split.direction = direction;
            split.gap = gap.clamp(0.0, 48.0);
            split.min_child = min_child.clamp(0.01, 0.4);
        }
        self.flatten();
        Ok(Change::Updated(split_id))
    }
}

fn renumber_node(node: &mut Node, next: &mut u32) {
    let id = NodeId(*next);
    *next += 1;
    match node {
        Node::Leaf(cell) => cell.id = id,
        Node::Split(split) => {
            split.id = id;
            for child in &mut split.children {
                renumber_node(child, next);
            }
        }
    }
}

/// One pass of canonicalisation. Returns true when something changed.
fn flatten_node(node: &mut Node) -> bool {
    let Node::Split(split) = node else {
        return false;
    };

    // A split with a single child is not a split.
    if split.children.len() == 1 {
        let survivor = split.children.remove(0);
        *node = survivor;
        return true;
    }

    // Merge same-direction children into this split.
    let mut changed = false;
    let mut index = 0;
    while index < split.children.len() {
        let merge = split.children[index]
            .as_split()
            .is_some_and(|child| child.direction == split.direction);
        if !merge {
            index += 1;
            continue;
        }
        let Node::Split(child) = split.children.remove(index) else {
            unreachable!("checked above")
        };
        let outer = split.sizes.remove(index);
        for (offset, grandchild) in child.children.into_iter().enumerate() {
            split.children.insert(index + offset, grandchild);
            split
                .sizes
                .insert(index + offset, outer * child.sizes[offset]);
        }
        changed = true;
    }
    if changed {
        normalise(&mut split.sizes);
        return true;
    }

    split.children.iter_mut().any(flatten_node)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_with_row_of_three() -> (Layout, Vec<NodeId>) {
        let mut layout = Layout::empty("Test");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout.split_cell(ids[1], Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        (layout, ids)
    }

    #[test]
    fn a_pane_can_be_added_on_either_side() {
        // Dropping on the left edge must put the new pane on the left.
        let mut layout = Layout::empty("Sides");
        let root = layout.root.id();
        layout
            .set_widget(root, Widget::new(WidgetKind::CurrentSlide))
            .unwrap();
        let Ok(Change::Created(fresh)) =
            layout.split_cell_at(root, Direction::Horizontal, Side::Before)
        else {
            panic!("expected a new pane")
        };
        let order: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        assert_eq!(order[0], fresh, "the new pane is first");
        assert_eq!(order[1], root, "the original keeps its widget on the right");
        assert!(layout.cell(root).unwrap().widget.is_some());

        // And on the right when asked for the other side.
        let mut layout = Layout::empty("Sides");
        let root = layout.root.id();
        let Ok(Change::Created(fresh)) =
            layout.split_cell_at(root, Direction::Horizontal, Side::After)
        else {
            panic!("expected a new pane")
        };
        let order: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        assert_eq!(order, vec![root, fresh]);
    }

    #[test]
    fn adding_before_a_pane_inside_a_row_keeps_the_order() {
        let (mut layout, ids) = layout_with_row_of_three();
        let middle = ids[1];
        let Ok(Change::Created(fresh)) =
            layout.split_cell_at(middle, Direction::Horizontal, Side::Before)
        else {
            panic!("expected a new pane")
        };
        let order: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        let at = order.iter().position(|id| *id == fresh).unwrap();
        assert_eq!(
            order[at + 1],
            middle,
            "the new pane sits immediately before the one it was dropped on"
        );
        assert!(layout.is_canonical(), "the row stayed flat");
    }

    #[test]
    fn a_new_layout_is_one_empty_cell() {
        let layout = Layout::empty("Fresh");
        assert_eq!(layout.cells().len(), 1);
        assert!(layout.cells()[0].is_empty());
        assert!(layout.is_canonical());
        assert_eq!(layout.id, LayoutId("fresh".into()));
    }

    #[test]
    fn splitting_the_middle_of_a_row_yields_four_across_not_a_nested_pair() {
        let (mut layout, ids) = layout_with_row_of_three();
        assert_eq!(ids.len(), 3);
        let root_split = layout.root.as_split().unwrap();
        assert_eq!(root_split.children.len(), 3);

        layout.split_cell(ids[1], Direction::Horizontal).unwrap();

        let root_split = layout.root.as_split().unwrap();
        assert_eq!(root_split.children.len(), 4, "flattened into the parent");
        assert!(
            root_split.children.iter().all(|child| !child.is_split()),
            "no nested split appeared"
        );
        assert!(layout.is_canonical());
    }

    #[test]
    fn splitting_perpendicular_nests() {
        let (mut layout, ids) = layout_with_row_of_three();
        layout.split_cell(ids[1], Direction::Vertical).unwrap();

        let root_split = layout.root.as_split().unwrap();
        assert_eq!(root_split.children.len(), 3, "the row is still three wide");
        let middle = &root_split.children[1];
        let nested = middle.as_split().expect("the middle became a split");
        assert_eq!(nested.direction, Direction::Vertical);
        assert_eq!(nested.children.len(), 2);
        assert!(layout.is_canonical());
    }

    #[test]
    fn a_new_divider_halves_the_selected_cell_and_leaves_siblings_alone() {
        let mut layout = Layout::empty("Test");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout.set_child_size(layout.root.id(), 0, 0.7).unwrap();

        let before: Vec<f32> = layout.root.as_split().unwrap().sizes.clone();
        assert!((before[0] - 0.7).abs() < 1e-3);

        layout.split_cell(ids[0], Direction::Horizontal).unwrap();
        let after: Vec<f32> = layout.root.as_split().unwrap().sizes.clone();
        assert_eq!(after.len(), 3);
        assert!(
            (after[0] - 0.35).abs() < 1e-3,
            "the selected cell was halved"
        );
        assert!((after[1] - 0.35).abs() < 1e-3);
        assert!((after[2] - 0.3).abs() < 1e-3, "the sibling kept its size");
    }

    #[test]
    fn deleting_gives_space_to_siblings_in_proportion() {
        let (mut layout, ids) = layout_with_row_of_three();
        layout.set_child_size(layout.root.id(), 0, 0.5).unwrap();
        let sizes = layout.root.as_split().unwrap().sizes.clone();
        let (first, third) = (sizes[0], sizes[2]);

        layout.delete_node(ids[1]).unwrap();

        let after = layout.root.as_split().unwrap();
        assert_eq!(after.children.len(), 2);
        let ratio_before = first / third;
        let ratio_after = after.sizes[0] / after.sizes[1];
        assert!(
            (ratio_before - ratio_after).abs() < 1e-3,
            "relative sizes are preserved: {ratio_before} vs {ratio_after}"
        );
        assert!((after.sizes.iter().sum::<f32>() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn a_split_reduced_to_one_child_dissolves() {
        let mut layout = Layout::empty("Test");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();

        layout.delete_node(ids[1]).unwrap();
        assert!(!layout.root.is_split(), "the survivor replaced the split");
        assert_eq!(layout.cells().len(), 1);
        assert!(layout.is_canonical());
    }

    #[test]
    fn dissolving_a_split_flattens_into_the_grandparent() {
        // A row containing [cell, column[cell, cell]]; delete one member of
        // the column and the survivor must be absorbed into the row.
        let mut layout = Layout::empty("Test");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout.split_cell(ids[1], Direction::Vertical).unwrap();
        let column = layout.root.as_split().unwrap().children[1]
            .as_split()
            .unwrap()
            .clone();
        assert_eq!(column.children.len(), 2);

        layout.delete_node(column.children[0].id()).unwrap();

        let root_split = layout.root.as_split().unwrap();
        assert_eq!(root_split.children.len(), 2);
        assert!(root_split.children.iter().all(|child| !child.is_split()));
        assert!(layout.is_canonical());
    }

    #[test]
    fn the_root_cell_cannot_be_deleted() {
        let mut layout = Layout::empty("Test");
        let root = layout.root.id();
        assert_eq!(layout.delete_node(root), Err(EditError::CannotDeleteRoot));
    }

    #[test]
    fn deleting_reports_the_widgets_it_would_destroy() {
        let (mut layout, ids) = layout_with_row_of_three();
        layout
            .set_widget(ids[1], Widget::new(WidgetKind::SpeakerNotes))
            .unwrap();
        layout.split_cell(ids[1], Direction::Vertical).unwrap();
        let subtree = layout.root.parent_of(ids[1]).unwrap();

        let doomed = layout.widgets_in(subtree);
        assert_eq!(doomed, vec![WidgetKind::SpeakerNotes]);
    }

    #[test]
    fn resizing_moves_only_the_two_adjacent_children() {
        let (mut layout, _) = layout_with_row_of_three();
        let split_id = layout.root.id();
        let before = layout.root.as_split().unwrap().sizes.clone();

        layout.resize_divider(split_id, 0, 0.1, false).unwrap();

        let after = layout.root.as_split().unwrap().sizes.clone();
        assert!((after[0] - (before[0] + 0.1)).abs() < 1e-4);
        assert!((after[1] - (before[1] - 0.1)).abs() < 1e-4);
        assert!(
            (after[2] - before[2]).abs() < 1e-6,
            "the third child is untouched"
        );
        assert!((after.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    /// Two rows, each split in two, so their dividers can be lined up.
    fn two_rows_each_split() -> (Layout, NodeId, NodeId) {
        let mut layout = Layout::empty("Rows");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Vertical).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout.split_cell(ids[0], Direction::Horizontal).unwrap();
        layout.split_cell(ids[1], Direction::Horizontal).unwrap();
        let splits: Vec<NodeId> = layout
            .root
            .as_split()
            .unwrap()
            .children
            .iter()
            .map(|child| child.id())
            .collect();
        (layout, splits[0], splits[1])
    }

    #[test]
    fn a_divider_lines_up_with_the_one_in_the_row_above() {
        let (mut layout, top, bottom) = two_rows_each_split();
        // Put the top row's divider somewhere unremarkable, away from the
        // quarter/third/half points so only alignment can explain a snap.
        layout.resize_divider(top, 0, 0.11, false).unwrap();
        let target = layout.split(top).unwrap().sizes[0];

        // Drag the bottom one to just short of it.
        layout
            .resize_divider(bottom, 0, target - 0.5 - 0.008, true)
            .unwrap();

        let landed = layout.split(bottom).unwrap().sizes[0];
        assert!(
            (landed - target).abs() < 1e-3,
            "expected the bottom divider to line up at {target}, got {landed}"
        );
    }

    #[test]
    fn alignment_snapping_does_not_drag_a_deliberate_choice_into_line() {
        let (mut layout, top, bottom) = two_rows_each_split();
        layout.resize_divider(top, 0, 0.11, false).unwrap();
        let target = layout.split(top).unwrap().sizes[0];

        // Well clear of it, and of the quarter/third/half points: no snap,
        // the position is kept as asked.
        let wanted = target - 0.06;
        layout
            .resize_divider(bottom, 0, wanted - 0.5, true)
            .unwrap();

        let landed = layout.split(bottom).unwrap().sizes[0];
        assert!(
            (landed - wanted).abs() < 1e-3,
            "expected {wanted}, got {landed}"
        );
    }

    #[test]
    fn alignment_snapping_is_off_when_snapping_is_off() {
        let (mut layout, top, bottom) = two_rows_each_split();
        layout.resize_divider(top, 0, 0.11, false).unwrap();
        let target = layout.split(top).unwrap().sizes[0];

        let wanted = target - 0.008;
        layout
            .resize_divider(bottom, 0, wanted - 0.5, false)
            .unwrap();

        let landed = layout.split(bottom).unwrap().sizes[0];
        assert!(
            (landed - wanted).abs() < 1e-3,
            "holding the modifier means exactly where the pointer is"
        );
    }

    #[test]
    fn resizing_respects_the_minimum_child_size() {
        let mut layout = Layout::empty("Test");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let split_id = layout.root.id();

        layout.resize_divider(split_id, 0, -5.0, false).unwrap();
        let sizes = layout.root.as_split().unwrap().sizes.clone();
        assert!(
            sizes[0] >= 0.049,
            "clamped at the minimum, got {}",
            sizes[0]
        );
        assert!((sizes.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn snapping_uses_the_dividers_position_within_the_whole_split() {
        let (mut layout, _) = layout_with_row_of_three();
        let split_id = layout.root.id();
        layout.equalize(split_id).unwrap();

        // Three equal children: the first divider sits at 0.333. Nudge it
        // towards 0.25 and it should snap exactly there.
        layout.resize_divider(split_id, 0, -0.075, true).unwrap();
        let sizes = layout.root.as_split().unwrap().sizes.clone();
        assert!(
            (sizes[0] - 0.25).abs() < 1e-4,
            "snapped to 25%, got {}",
            sizes[0]
        );

        // With snapping off the same nudge lands where it was dragged.
        layout.equalize(split_id).unwrap();
        layout.resize_divider(split_id, 0, -0.075, false).unwrap();
        let sizes = layout.root.as_split().unwrap().sizes.clone();
        assert!((sizes[0] - (1.0 / 3.0 - 0.075)).abs() < 1e-4);
    }

    #[test]
    fn equalising_and_reversing() {
        let (mut layout, _) = layout_with_row_of_three();
        let split_id = layout.root.id();
        layout.set_child_size(split_id, 0, 0.6).unwrap();
        layout.equalize(split_id).unwrap();
        for size in &layout.root.as_split().unwrap().sizes {
            assert!((size - 1.0 / 3.0).abs() < 1e-4);
        }

        layout.set_child_size(split_id, 0, 0.5).unwrap();
        let first_child = layout.root.as_split().unwrap().children[0].id();
        layout.reverse_children(split_id).unwrap();
        let reversed = layout.root.as_split().unwrap();
        assert_eq!(reversed.children.last().unwrap().id(), first_child);
        assert!((reversed.sizes.last().unwrap() - 0.5).abs() < 1e-3);
    }

    #[test]
    fn changing_a_splits_direction_reflattens_the_tree() {
        let mut layout = Layout::empty("Test");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout.split_cell(ids[1], Direction::Vertical).unwrap();
        let nested = layout.root.as_split().unwrap().children[1].id();

        // Turning the nested column into a row makes it mergeable with the
        // row above it.
        layout
            .set_split_properties(nested, Direction::Horizontal, 8.0, 0.05)
            .unwrap();

        assert!(layout.is_canonical());
        let root_split = layout.root.as_split().unwrap();
        assert_eq!(root_split.children.len(), 3);
        assert!(root_split.children.iter().all(|child| !child.is_split()));
    }

    #[test]
    fn single_instance_widgets_are_refused_twice() {
        let (mut layout, ids) = layout_with_row_of_three();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::SpeakerNotes))
            .unwrap();
        assert_eq!(
            layout.set_widget(ids[1], Widget::new(WidgetKind::SpeakerNotes)),
            Err(EditError::AlreadyPlaced("Speaker Notes"))
        );
        assert!(layout.already_placed(WidgetKind::SpeakerNotes));

        // Replacing it in its own cell is fine.
        assert!(layout
            .set_widget(ids[0], Widget::new(WidgetKind::SpeakerNotes))
            .is_ok());
    }

    #[test]
    fn a_second_slider_is_refused_but_a_second_counter_is_not() {
        let (mut layout, ids) = layout_with_row_of_three();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::SlideSlider))
            .unwrap();
        assert!(layout.already_placed(WidgetKind::SlideSlider));
        assert_eq!(
            layout.set_widget(ids[1], Widget::new(WidgetKind::SlideSlider)),
            Err(EditError::AlreadyPlaced("Slide Slider"))
        );
        // A counter is multi-instance, so it is still allowed.
        assert!(layout
            .set_widget(ids[1], Widget::new(WidgetKind::SlideCounter))
            .is_ok());
    }

    #[test]
    fn moving_swapping_and_clearing_widgets() {
        let (mut layout, ids) = layout_with_row_of_three();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::CurrentSlide))
            .unwrap();
        layout
            .set_widget(ids[1], Widget::new(WidgetKind::SpeakerNotes))
            .unwrap();

        layout.swap_widgets(ids[0], ids[1]).unwrap();
        assert_eq!(
            layout.cell(ids[0]).unwrap().widget.as_ref().unwrap().kind(),
            WidgetKind::SpeakerNotes
        );

        layout.move_widget(ids[0], ids[2]).unwrap();
        assert!(layout.cell(ids[0]).unwrap().is_empty());
        assert_eq!(
            layout.cell(ids[2]).unwrap().widget.as_ref().unwrap().kind(),
            WidgetKind::SpeakerNotes
        );

        layout.clear_cell(ids[2]).unwrap();
        assert!(layout.cell(ids[2]).unwrap().is_empty());
        assert_eq!(
            layout.cells().len(),
            3,
            "clearing does not change structure"
        );
    }

    #[test]
    fn renaming_structural_nodes_leaves_widget_names_alone() {
        let (mut layout, ids) = layout_with_row_of_three();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::CurrentSlide))
            .unwrap();
        let split_id = layout.root.id();

        layout
            .rename_node(split_id, Some("Slide Previews".into()))
            .unwrap();
        assert_eq!(
            layout.split(split_id).unwrap().display_name(),
            "Slide Previews"
        );
        assert_eq!(
            layout
                .cell(ids[0])
                .unwrap()
                .widget
                .as_ref()
                .unwrap()
                .label(),
            "Current Slide"
        );

        layout.rename_node(split_id, Some("   ".into())).unwrap();
        assert!(
            layout.split(split_id).unwrap().name.is_none(),
            "a blank name falls back to the generated one"
        );
    }

    #[test]
    fn aspect_ratios_flag_only_substantial_differences() {
        assert!(!AspectRatio::SixteenNine.differs_substantially_from(AspectRatio::SixteenTen));
        assert!(
            AspectRatio::SixteenNine.differs_substantially_from(AspectRatio::Detected {
                width: 1080,
                height: 1920
            })
        );
        assert!(AspectRatio::SixteenNine.differs_substantially_from(AspectRatio::FourThree));
    }

    #[test]
    fn renumbering_makes_ids_unique_and_safe() {
        let (mut layout, _) = layout_with_row_of_three();
        layout.renumber();
        let ids = layout.root.ids();
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len());

        // A freshly allocated id must not collide with an existing one.
        let a_cell = layout.cells()[0].id;
        layout.split_cell(a_cell, Direction::Vertical).unwrap();
        let ids = layout.root.ids();
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len());
    }

    #[test]
    fn hit_testing_finds_the_cell_under_a_point() {
        let (layout, ids) = layout_with_row_of_three();
        let area = Frame::new(0.0, 0.0, 900.0, 300.0);
        assert_eq!(layout.cell_at(10.0, 10.0, area), Some(ids[0]));
        assert_eq!(layout.cell_at(450.0, 150.0, area), Some(ids[1]));
        assert_eq!(layout.cell_at(890.0, 290.0, area), Some(ids[2]));
        assert_eq!(layout.cell_at(-5.0, 10.0, area), None);
    }
}
