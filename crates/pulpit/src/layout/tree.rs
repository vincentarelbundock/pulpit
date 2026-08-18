//! The recursive layout tree, its canonical form, and the operations the
//! editor performs on it.
//!
//! Two node types only: a **split** with two or more children in one
//! direction, and a **leaf** which is one visible cell. The tree is kept
//! canonical — a split never directly contains a child split of the same
//! direction — which is what makes divider behaviour predictable and gives
//! "split the middle of a three-across row" the four-across result a user
//! expects instead of a nested pair.

use serde::{Deserialize, Serialize};

use crate::widgets::{Widget, WidgetKind};

/// Stable identifier for a node within one layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// Children side by side; dividers are vertical. "Split left and right".
    #[default]
    Horizontal,
    /// Children stacked; dividers are horizontal. "Split top and bottom".
    Vertical,
}

impl Direction {
    /// The wording the interface uses, paired with a directional icon.
    pub fn action_label(self) -> &'static str {
        match self {
            Direction::Horizontal => "Split left and right",
            Direction::Vertical => "Split top and bottom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Direction::Horizontal => "Left and right",
            Direction::Vertical => "Top and bottom",
        }
    }

    pub fn perpendicular(self) -> Direction {
        match self {
            Direction::Horizontal => Direction::Vertical,
            Direction::Vertical => Direction::Horizontal,
        }
    }
}

/// What an empty cell does at presentation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmptyBehavior {
    /// Render as an empty dark panel.
    #[default]
    ShowBlankPanel,
    /// Hide the cell and give its space to its siblings.
    Collapse,
}

impl EmptyBehavior {
    pub const ALL: [EmptyBehavior; 2] = [EmptyBehavior::ShowBlankPanel, EmptyBehavior::Collapse];

    pub fn label(self) -> &'static str {
        match self {
            EmptyBehavior::ShowBlankPanel => "Show blank panel",
            EmptyBehavior::Collapse => "Collapse",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellBackground {
    /// The application background shows through.
    #[default]
    None,
    /// A slightly raised dark panel.
    Panel,
    /// A light canvas, used to separate slide content from presenter tools.
    Canvas,
}

impl CellBackground {
    pub const ALL: [CellBackground; 3] = [
        CellBackground::None,
        CellBackground::Panel,
        CellBackground::Canvas,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CellBackground::None => "None",
            CellBackground::Panel => "Panel",
            CellBackground::Canvas => "Light canvas",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellBorder {
    #[default]
    None,
    Subtle,
    Accent,
}

/// How one axis of a cell participates in its parent's split.
///
/// `Fill` keeps the authored split proportion. `Hug` takes only the widget's
/// declared minimum extent plus the cell padding, leaving the remaining space
/// to the fill cells beside it. The setting lives on the cell so structural
/// edits cannot separate it from the widget it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellExtent {
    #[default]
    Fill,
    Hug,
}

/// Independent sizing for the two axes of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CellSizing {
    #[serde(default)]
    pub horizontal: CellExtent,
    #[serde(default)]
    pub vertical: CellExtent,
}

impl CellSizing {
    fn is_fill(&self) -> bool {
        self.horizontal == CellExtent::Fill && self.vertical == CellExtent::Fill
    }
}

impl CellBorder {
    pub const ALL: [CellBorder; 3] = [CellBorder::None, CellBorder::Subtle, CellBorder::Accent];

    pub fn label(self) -> &'static str {
        match self {
            CellBorder::None => "None",
            CellBorder::Subtle => "Subtle",
            CellBorder::Accent => "Accent",
        }
    }
}

/// A placed widget whose id (§ [`crate::widgets::registry::WidgetId`]) is not
/// known to this build.
///
/// Kept instead of dropped or refused: a layout saved by a newer pulpit, or
/// one hand-edited, must still open. The id, its raw configuration and its
/// raw style are preserved untouched, so saving the layout again reproduces
/// the entry exactly rather than losing it or guessing at a shape this build
/// cannot validate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnavailableWidget {
    pub widget_id: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub style: serde_json::Value,
}

/// One visible cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub id: NodeId,
    /// Structural name, renameable without touching widget names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<Widget>,
    /// Set instead of `widget` when the saved widget id does not resolve to
    /// a kind this build knows. Mutually exclusive with `widget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<UnavailableWidget>,
    #[serde(default)]
    pub padding: f32,
    #[serde(default)]
    pub background: CellBackground,
    /// Kept in the file format for compatibility with existing layouts.
    /// Passive perimeter borders are no longer rendered; split gutters draw
    /// a single muted separator between neighbouring cells instead.
    #[serde(default)]
    pub border: CellBorder,
    #[serde(default)]
    pub empty_behavior: EmptyBehavior,
    /// Whether this cell fills its proportional track or hugs the widget on
    /// either axis. Omitted from older and ordinary proportional layouts.
    #[serde(default, skip_serializing_if = "CellSizing::is_fill")]
    pub sizing: CellSizing,
}

impl Cell {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            name: None,
            widget: None,
            unavailable: None,
            padding: 8.0,
            background: CellBackground::None,
            border: CellBorder::None,
            empty_behavior: EmptyBehavior::ShowBlankPanel,
            sizing: CellSizing::default(),
        }
    }

    pub fn with_widget(id: NodeId, widget: Widget) -> Self {
        Self {
            widget: Some(widget),
            ..Self::new(id)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.widget.is_none() && self.unavailable.is_none()
    }

    /// What the tree panel calls this cell.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| match (&self.widget, &self.unavailable) {
                (Some(widget), _) => widget.label().to_string(),
                (None, Some(unavailable)) => format!("Unknown widget ({})", unavailable.widget_id),
                (None, None) => "Empty cell".to_string(),
            })
    }
}

/// A split with two or more children in one direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub direction: Direction,
    pub children: Vec<Node>,
    /// Relative sizes, one per child, summing to 1.
    pub sizes: Vec<f32>,
    /// Gutter between children, in layout points.
    #[serde(default = "default_gap")]
    pub gap: f32,
    /// Smallest fraction any child may be shrunk to by a drag.
    #[serde(default = "default_min_child")]
    pub min_child: f32,
}

fn default_gap() -> f32 {
    crate::widgets::tokens::SPLIT_GAP
}

fn default_min_child() -> f32 {
    0.05
}

impl Split {
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            format!(
                "{} split · {} cells",
                self.direction.label(),
                self.children.len()
            )
        })
    }

    /// Number of dividers: one fewer than the children.
    pub fn divider_count(&self) -> usize {
        self.children.len().saturating_sub(1)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Node {
    Split(Split),
    Leaf(Cell),
}

impl Node {
    pub fn id(&self) -> NodeId {
        match self {
            Node::Split(split) => split.id,
            Node::Leaf(cell) => cell.id,
        }
    }

    pub fn is_split(&self) -> bool {
        matches!(self, Node::Split(_))
    }

    pub fn as_split(&self) -> Option<&Split> {
        match self {
            Node::Split(split) => Some(split),
            Node::Leaf(_) => None,
        }
    }

    pub fn as_cell(&self) -> Option<&Cell> {
        match self {
            Node::Leaf(cell) => Some(cell),
            Node::Split(_) => None,
        }
    }

    pub fn display_name(&self) -> String {
        match self {
            Node::Split(split) => split.display_name(),
            Node::Leaf(cell) => cell.display_name(),
        }
    }

    pub fn name_mut(&mut self) -> &mut Option<String> {
        match self {
            Node::Split(split) => &mut split.name,
            Node::Leaf(cell) => &mut cell.name,
        }
    }

    /// The fixed extent this subtree requests on `direction`, when every leaf
    /// in that axis is content-sized. A perpendicular split hugs the largest
    /// child; a parallel split hugs their sum and its gutters.
    pub fn hug_extent(&self, direction: Direction) -> Option<f32> {
        match self {
            Node::Leaf(cell) => {
                let extent = match direction {
                    Direction::Horizontal => cell.sizing.horizontal,
                    Direction::Vertical => cell.sizing.vertical,
                };
                if extent != CellExtent::Hug {
                    return None;
                }
                let (width, height) = cell.widget.as_ref()?.minimum_size();
                let content = match direction {
                    Direction::Horizontal => width,
                    Direction::Vertical => height,
                };
                Some(content + cell.padding * 2.0)
            }
            Node::Split(split) => {
                let extents: Option<Vec<f32>> = split
                    .children
                    .iter()
                    .map(|child| child.hug_extent(direction))
                    .collect();
                let extents = extents?;
                if split.direction == direction {
                    Some(
                        extents.iter().sum::<f32>()
                            + split.gap * extents.len().saturating_sub(1) as f32,
                    )
                } else {
                    extents.into_iter().reduce(f32::max)
                }
            }
        }
    }

    /// Every cell in this subtree, in reading order.
    pub fn cells(&self) -> Vec<&Cell> {
        let mut out = Vec::new();
        self.collect_cells(&mut out);
        out
    }

    fn collect_cells<'a>(&'a self, out: &mut Vec<&'a Cell>) {
        match self {
            Node::Leaf(cell) => out.push(cell),
            Node::Split(split) => {
                for child in &split.children {
                    child.collect_cells(out);
                }
            }
        }
    }

    /// Every widget in this subtree.
    pub fn widgets(&self) -> Vec<&Widget> {
        self.cells()
            .into_iter()
            .filter_map(|cell| cell.widget.as_ref())
            .collect()
    }

    pub fn find(&self, id: NodeId) -> Option<&Node> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Node::Leaf(_) => None,
            Node::Split(split) => split.children.iter().find_map(|child| child.find(id)),
        }
    }

    pub fn find_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Node::Leaf(_) => None,
            Node::Split(split) => split
                .children
                .iter_mut()
                .find_map(|child| child.find_mut(id)),
        }
    }

    pub fn cell_mut(&mut self, id: NodeId) -> Option<&mut Cell> {
        match self.find_mut(id)? {
            Node::Leaf(cell) => Some(cell),
            Node::Split(_) => None,
        }
    }

    pub fn split_mut(&mut self, id: NodeId) -> Option<&mut Split> {
        match self.find_mut(id)? {
            Node::Split(split) => Some(split),
            Node::Leaf(_) => None,
        }
    }

    /// The id of the split that directly contains `id`, if any.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        match self {
            Node::Leaf(_) => None,
            Node::Split(split) => {
                if split.children.iter().any(|child| child.id() == id) {
                    return Some(split.id);
                }
                split.children.iter().find_map(|child| child.parent_of(id))
            }
        }
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.find(id).is_some()
    }

    /// Depth-first list of every node id.
    pub fn ids(&self) -> Vec<NodeId> {
        let mut out = vec![self.id()];
        if let Node::Split(split) = self {
            for child in &split.children {
                out.extend(child.ids());
            }
        }
        out
    }

    /// True when this subtree obeys the canonical rule.
    pub fn is_canonical(&self) -> bool {
        match self {
            Node::Leaf(_) => true,
            Node::Split(split) => {
                if split.children.len() < 2 {
                    return false;
                }
                if split.sizes.len() != split.children.len() {
                    return false;
                }
                if (split.sizes.iter().sum::<f32>() - 1.0).abs() > 1e-3 {
                    return false;
                }
                if split.sizes.iter().any(|size| *size <= 0.0) {
                    return false;
                }
                split.children.iter().all(|child| {
                    let same_direction = child
                        .as_split()
                        .is_some_and(|inner| inner.direction == split.direction);
                    !same_direction && child.is_canonical()
                })
            }
        }
    }
}

/// A rectangle in layout points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Frame {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// Where one node ends up when the layout is drawn into `area`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub id: NodeId,
    pub frame: Frame,
}

/// A divider between two adjacent children of a split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Divider {
    pub split: NodeId,
    /// Index of the child *before* the divider.
    pub index: usize,
    pub direction: Direction,
    pub frame: Frame,
}

/// Compute where everything lands inside `area`.
///
/// `collapse_empty` applies the cell property of the same meaning: at
/// presentation time a collapsed empty cell gives its space to its siblings;
/// in the editor it stays visible so it can still be selected.
pub fn compute(root: &Node, area: Frame, collapse_empty: bool) -> (Vec<Placement>, Vec<Divider>) {
    let mut placements = Vec::new();
    let mut dividers = Vec::new();
    layout_node(root, area, collapse_empty, &mut placements, &mut dividers);
    (placements, dividers)
}

fn is_collapsed(node: &Node, collapse_empty: bool) -> bool {
    if !collapse_empty {
        return false;
    }
    match node {
        Node::Leaf(cell) => cell.is_empty() && cell.empty_behavior == EmptyBehavior::Collapse,
        Node::Split(split) => split
            .children
            .iter()
            .all(|child| is_collapsed(child, collapse_empty)),
    }
}

fn layout_node(
    node: &Node,
    area: Frame,
    collapse_empty: bool,
    placements: &mut Vec<Placement>,
    dividers: &mut Vec<Divider>,
) {
    placements.push(Placement {
        id: node.id(),
        frame: area,
    });
    let Node::Split(split) = node else { return };

    let visible: Vec<usize> = (0..split.children.len())
        .filter(|index| !is_collapsed(&split.children[*index], collapse_empty))
        .collect();
    if visible.is_empty() {
        return;
    }

    let gaps = split.gap * visible.len().saturating_sub(1) as f32;
    let span = match split.direction {
        Direction::Horizontal => area.width,
        Direction::Vertical => area.height,
    };
    let usable = (span - gaps).max(0.0);
    let fixed: Vec<Option<f32>> = visible
        .iter()
        .map(|index| split.children[*index].hug_extent(split.direction))
        .collect();
    let fixed_total: f32 = fixed.iter().flatten().sum();
    let flexible_total: f32 = visible
        .iter()
        .zip(fixed.iter())
        .filter_map(|(index, extent)| extent.is_none().then_some(split.sizes[*index]))
        .sum();
    let flexible_span = (usable - fixed_total).max(0.0);

    let mut offset = match split.direction {
        Direction::Horizontal => area.x,
        Direction::Vertical => area.y,
    };

    for (position, index) in visible.iter().enumerate() {
        let length = fixed[position].unwrap_or_else(|| {
            if flexible_total > 0.0 {
                flexible_span * split.sizes[*index] / flexible_total
            } else {
                0.0
            }
        });
        let child_area = match split.direction {
            Direction::Horizontal => Frame::new(offset, area.y, length, area.height),
            Direction::Vertical => Frame::new(area.x, offset, area.width, length),
        };
        layout_node(
            &split.children[*index],
            child_area,
            collapse_empty,
            placements,
            dividers,
        );
        offset += length;

        if position + 1 < visible.len() {
            let frame = match split.direction {
                Direction::Horizontal => Frame::new(offset, area.y, split.gap, area.height),
                Direction::Vertical => Frame::new(area.x, offset, area.width, split.gap),
            };
            dividers.push(Divider {
                split: split.id,
                index: *index,
                direction: split.direction,
                frame,
            });
            offset += split.gap;
        }
    }
}

/// Normalise a size vector so it sums to 1 with no non-positive entries.
pub fn normalise(sizes: &mut [f32]) {
    for size in sizes.iter_mut() {
        if !size.is_finite() || *size <= 0.0 {
            *size = 0.001;
        }
    }
    let total: f32 = sizes.iter().sum();
    if total <= 0.0 {
        let even = 1.0 / sizes.len() as f32;
        sizes.iter_mut().for_each(|size| *size = even);
        return;
    }
    sizes.iter_mut().for_each(|size| *size /= total);
}

/// Count how many times each widget kind is *occupied*, compounds expanded.
pub fn instance_counts(root: &Node) -> std::collections::BTreeMap<WidgetKind, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for widget in root.widgets() {
        for kind in widget.kind().occupies() {
            *counts.entry(kind).or_insert(0) += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: u32) -> Node {
        Node::Leaf(Cell::new(NodeId(id)))
    }

    fn split(id: u32, direction: Direction, children: Vec<Node>) -> Node {
        let count = children.len();
        Node::Split(Split {
            id: NodeId(id),
            name: None,
            direction,
            children,
            sizes: vec![1.0 / count as f32; count],
            gap: 8.0,
            min_child: 0.05,
        })
    }

    #[test]
    fn geometry_divides_proportionally_and_accounts_for_gaps() {
        let root = Node::Split(Split {
            id: NodeId(0),
            name: None,
            direction: Direction::Horizontal,
            children: vec![leaf(1), leaf(2), leaf(3)],
            sizes: vec![0.27, 0.46, 0.27],
            gap: 10.0,
            min_child: 0.05,
        });
        let area = Frame::new(0.0, 0.0, 1020.0, 500.0);
        let (placements, dividers) = compute(&root, area, false);

        // 1020 minus two 10pt gaps leaves 1000 for the children.
        let width_of = |id: u32| {
            placements
                .iter()
                .find(|placement| placement.id == NodeId(id))
                .unwrap()
                .frame
                .width
        };
        assert!((width_of(1) - 270.0).abs() < 0.01);
        assert!((width_of(2) - 460.0).abs() < 0.01);
        assert!((width_of(3) - 270.0).abs() < 0.01);
        assert_eq!(dividers.len(), 2);
        assert_eq!(dividers[0].direction, Direction::Horizontal);

        // Children tile the area without overlapping.
        let first = placements.iter().find(|p| p.id == NodeId(1)).unwrap().frame;
        let second = placements.iter().find(|p| p.id == NodeId(2)).unwrap().frame;
        assert!((first.x + first.width + 10.0 - second.x).abs() < 0.01);
    }

    #[test]
    fn collapsed_empty_cells_give_their_space_away_at_presentation_time() {
        let mut collapsing = Cell::new(NodeId(2));
        collapsing.empty_behavior = EmptyBehavior::Collapse;
        let root = Node::Split(Split {
            id: NodeId(0),
            name: None,
            direction: Direction::Horizontal,
            children: vec![leaf(1), Node::Leaf(collapsing), leaf(3)],
            sizes: vec![0.4, 0.2, 0.4],
            gap: 0.0,
            min_child: 0.05,
        });
        let area = Frame::new(0.0, 0.0, 800.0, 100.0);

        let (editor, _) = compute(&root, area, false);
        assert!(
            editor
                .iter()
                .any(|p| p.id == NodeId(2) && p.frame.width > 0.0),
            "in the editor the cell stays selectable"
        );

        let (presentation, dividers) = compute(&root, area, true);
        let width_of = |id: u32| {
            presentation
                .iter()
                .find(|p| p.id == NodeId(id))
                .unwrap()
                .frame
                .width
        };
        assert!(
            (width_of(1) - 400.0).abs() < 0.01,
            "space is redistributed in proportion"
        );
        assert!((width_of(3) - 400.0).abs() < 0.01);
        assert_eq!(
            dividers.len(),
            1,
            "the collapsed cell takes its divider with it"
        );
    }

    #[test]
    fn canonical_form_rejects_a_same_direction_child() {
        let nested_same = split(
            0,
            Direction::Horizontal,
            vec![
                leaf(1),
                split(2, Direction::Horizontal, vec![leaf(3), leaf(4)]),
            ],
        );
        assert!(!nested_same.is_canonical());

        let nested_perpendicular = split(
            0,
            Direction::Horizontal,
            vec![
                leaf(1),
                split(2, Direction::Vertical, vec![leaf(3), leaf(4)]),
            ],
        );
        assert!(nested_perpendicular.is_canonical());
    }

    #[test]
    fn canonical_form_rejects_degenerate_splits() {
        let single_child = Node::Split(Split {
            id: NodeId(0),
            name: None,
            direction: Direction::Horizontal,
            children: vec![leaf(1)],
            sizes: vec![1.0],
            gap: 8.0,
            min_child: 0.05,
        });
        assert!(!single_child.is_canonical());

        let bad_sizes = Node::Split(Split {
            id: NodeId(0),
            name: None,
            direction: Direction::Horizontal,
            children: vec![leaf(1), leaf(2)],
            sizes: vec![0.9, 0.9],
            gap: 8.0,
            min_child: 0.05,
        });
        assert!(!bad_sizes.is_canonical());
    }

    #[test]
    fn traversal_finds_nodes_parents_and_widgets() {
        let mut cell = Cell::new(NodeId(3));
        cell.widget = Some(Widget::new(WidgetKind::CurrentSlide));
        let root = split(
            0,
            Direction::Vertical,
            vec![
                leaf(1),
                split(2, Direction::Horizontal, vec![Node::Leaf(cell), leaf(4)]),
            ],
        );

        assert!(root.find(NodeId(4)).is_some());
        assert_eq!(root.parent_of(NodeId(4)), Some(NodeId(2)));
        assert_eq!(root.parent_of(NodeId(0)), None);
        assert_eq!(root.cells().len(), 3);
        assert_eq!(root.widgets().len(), 1);
        assert_eq!(root.ids().len(), 5);
    }

    #[test]
    fn instance_counting_expands_compounds() {
        let mut strip = Cell::new(NodeId(1));
        strip.widget = Some(Widget::new(WidgetKind::PreviousCurrentNext));
        let mut single = Cell::new(NodeId(2));
        single.widget = Some(Widget::new(WidgetKind::CurrentSlide));
        let root = split(
            0,
            Direction::Horizontal,
            vec![Node::Leaf(strip), Node::Leaf(single)],
        );

        let counts = instance_counts(&root);
        assert_eq!(counts[&WidgetKind::CurrentSlide], 2);
        assert_eq!(counts[&WidgetKind::PreviousSlide], 1);
        assert_eq!(counts[&WidgetKind::PreviousCurrentNext], 1);
    }

    #[test]
    fn normalising_handles_nonsense() {
        let mut sizes = vec![2.0, 2.0];
        normalise(&mut sizes);
        assert_eq!(sizes, vec![0.5, 0.5]);

        let mut broken = vec![f32::NAN, -1.0];
        normalise(&mut broken);
        assert!((broken.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }
}
