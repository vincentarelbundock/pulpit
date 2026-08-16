//! Save-time and import-time validation.
//!
//! Validation inspects **configuration**, not merely presence: a Slide
//! Navigation widget with its forward button and slider both hidden does not
//! satisfy the forward-navigation requirement.
//!
//! Most findings are warnings. A presenter may deliberately build a layout
//! with no notes; the application explains the consequence and still saves.
//! Only structural corruption and instance-limit violations block.

use crate::layout::model::Layout;
use crate::layout::tree::{instance_counts, Frame, NodeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Saving and importing are refused.
    Blocking,
    /// Explained, then allowed.
    Warning,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Blocking => "Blocked",
            Severity::Warning => "Warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub severity: Severity,
    /// What is wrong.
    pub message: String,
    /// What it means in practice, so the user can decide.
    pub consequence: String,
    /// The node to select when the user clicks the issue.
    pub node: Option<NodeId>,
}

impl Issue {
    fn warning(message: impl Into<String>, consequence: impl Into<String>) -> Issue {
        Issue {
            severity: Severity::Warning,
            message: message.into(),
            consequence: consequence.into(),
            node: None,
        }
    }

    fn blocking(message: impl Into<String>, consequence: impl Into<String>) -> Issue {
        Issue {
            severity: Severity::Blocking,
            message: message.into(),
            consequence: consequence.into(),
            node: None,
        }
    }

    fn at(mut self, node: NodeId) -> Issue {
        self.node = Some(node);
        self
    }
}

/// Validate a layout as it would be drawn into `area`.
///
/// `area` matters: "a widget does not fit its cell" is a question about
/// geometry, and geometry depends on the screen the layout is checked at.
pub fn validate(layout: &Layout, area: Frame) -> Vec<Issue> {
    let mut issues = Vec::new();

    // ---- structural, blocking -------------------------------------------
    if !layout.is_canonical() {
        issues.push(Issue::blocking(
            "The layout tree is not canonical",
            "Splits must have two or more children and may not nest a split of the same \
             direction. This file cannot be used.",
        ));
    }
    for (kind, count) in instance_counts(&layout.root) {
        if !kind.multi_instance() && count > 1 {
            issues.push(Issue::blocking(
                format!("{} appears {count} times", kind.label()),
                format!(
                    "{} may only appear once, including inside compound widgets.",
                    kind.label()
                ),
            ));
        }
    }

    // ---- operability, warnings ------------------------------------------
    let widgets = layout.widgets();

    // The three warnings below are about *presenting*: a screen with no slide
    // on it and no way to advance is a presenter screen that cannot present.
    // A document layout has neither and needs neither — a reader turns pages,
    // not slides — so asking a Reader for a current-slide widget would be
    // asking it to be something else (§2, §2.1).
    let presenting = crate::layout::builtin::LayoutMode::of(layout)
        == crate::layout::builtin::LayoutMode::Presentation;

    if presenting && !widgets.iter().any(|widget| widget.shows_current_slide()) {
        issues.push(Issue::warning(
            "No current-slide widget",
            "You will not be able to see what the audience is seeing.",
        ));
    }

    if presenting
        && !widgets
            .iter()
            .any(|widget| widget.provides_forward_navigation())
    {
        issues.push(Issue::warning(
            "No forward-navigation control",
            "No visible forward button or slider: you will have to advance with the keyboard \
             or a remote.",
        ));
    }

    if presenting
        && !widgets
            .iter()
            .any(|widget| widget.provides_backward_navigation())
    {
        issues.push(Issue::warning(
            "No backward-navigation control",
            "Going back will only be possible from the keyboard or a remote.",
        ));
    }

    // ---- geometry --------------------------------------------------------
    let (placements, _) = layout.compute(area, true);
    let frame_of = |id: NodeId| {
        placements
            .iter()
            .find(|placement| placement.id == id)
            .map(|placement| placement.frame)
    };

    let mut empty_cells = 0;
    for cell in layout.cells() {
        let Some(frame) = frame_of(cell.id) else {
            continue;
        };
        let inner_width = (frame.width - cell.padding * 2.0).max(0.0);
        let inner_height = (frame.height - cell.padding * 2.0).max(0.0);

        let Some(widget) = &cell.widget else {
            empty_cells += 1;
            continue;
        };

        let (minimum_width, minimum_height) = widget.minimum_size();
        if inner_width + 0.5 < minimum_width || inner_height + 0.5 < minimum_height {
            issues.push(
                Issue::warning(
                    format!("{} does not fit its cell", widget.label()),
                    format!(
                        "The cell is {:.0}×{:.0}pt but this widget needs at least {:.0}×{:.0}pt. \
                         It will be cramped or clipped.",
                        inner_width, inner_height, minimum_width, minimum_height
                    ),
                )
                .at(cell.id),
            );
        }

        // Whatever the family itself has to say. The validator attaches the
        // cell; it does not know what makes a timer or a notes pane unusable.
        for complaint in widget.validate((inner_width, inner_height)) {
            issues.push(Issue::warning(complaint.message, complaint.consequence).at(cell.id));
        }
    }

    if empty_cells > 0 {
        issues.push(Issue::warning(
            format!(
                "The layout contains {empty_cells} empty {}",
                if empty_cells == 1 { "cell" } else { "cells" }
            ),
            "Empty cells render as blank panels unless you set them to collapse.",
        ));
    }

    issues
}

/// Do these issues prevent saving or importing?
pub fn is_blocked(issues: &[Issue]) -> bool {
    issues
        .iter()
        .any(|issue| issue.severity == Severity::Blocking)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::builtin::presenter_default;
    use crate::layout::model::Layout;
    use crate::layout::tree::Direction;
    use crate::widgets::{Widget, WidgetKind, WidgetPatch};

    fn area() -> Frame {
        Frame::new(0.0, 0.0, 1600.0, 900.0)
    }

    fn messages(issues: &[Issue]) -> Vec<String> {
        issues.iter().map(|issue| issue.message.clone()).collect()
    }

    #[test]
    fn a_built_in_layout_is_clean() {
        let issues = validate(&presenter_default(), area());
        assert!(issues.is_empty(), "{issues:?}");
        assert!(!is_blocked(&issues));
    }

    #[test]
    fn an_empty_layout_reports_the_essentials_but_does_not_block() {
        let layout = Layout::empty("Bare");
        let issues = validate(&layout, area());
        let messages = messages(&issues);
        assert!(messages
            .iter()
            .any(|m| m.contains("No current-slide widget")));
        assert!(messages
            .iter()
            .any(|m| m.contains("No forward-navigation control")));
        assert!(messages.iter().any(|m| m.contains("empty cell")));
        assert!(!is_blocked(&issues), "a sparse layout is still saveable");
        for issue in &issues {
            assert!(!issue.consequence.is_empty(), "every issue explains itself");
        }
    }

    #[test]
    fn navigation_hidden_by_configuration_fails_the_requirement() {
        let mut layout = Layout::empty("Nav");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::CurrentSlide))
            .unwrap();

        let mut navigation = Widget::new(WidgetKind::SlideButtons);
        navigation
            .apply(WidgetPatch::Buttons(
                crate::widgets::navigation::model::ButtonsPatch::Forward(false),
            ))
            .unwrap();
        layout.set_widget(ids[1], navigation).unwrap();

        let messages = messages(&validate(&layout, area()));
        assert!(
            messages
                .iter()
                .any(|m| m.contains("No forward-navigation control")),
            "presence is not enough: {messages:?}"
        );
    }

    #[test]
    fn a_compound_widget_satisfies_its_constituents_requirements() {
        let mut layout = Layout::empty("Compound");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Vertical).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::PreviousCurrentNext))
            .unwrap();
        layout
            .set_widget(ids[1], Widget::new(WidgetKind::SlideSlider))
            .unwrap();

        let messages = messages(&validate(&layout, area()));
        assert!(
            !messages.iter().any(|m| m.contains("current-slide")),
            "the strip provides the current slide: {messages:?}"
        );
        assert!(!messages.iter().any(|m| m.contains("forward-navigation")));
    }

    #[test]
    fn cramped_widgets_are_reported_against_the_screen_they_are_checked_at() {
        let mut layout = Layout::empty("Cramped");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::SpeakerNotes))
            .unwrap();
        layout
            .set_widget(ids[1], Widget::new(WidgetKind::CurrentSlide))
            .unwrap();

        let roomy = validate(&layout, Frame::new(0.0, 0.0, 1600.0, 900.0));
        assert!(
            !messages(&roomy).iter().any(|m| m.contains("too small")),
            "{roomy:?}"
        );

        let tiny = validate(&layout, Frame::new(0.0, 0.0, 420.0, 200.0));
        let messages = messages(&tiny);
        assert!(messages
            .iter()
            .any(|m| m.contains("Speaker notes are too small")));
        assert!(messages.iter().any(|m| m.contains("does not fit its cell")));
        assert!(tiny.iter().all(|issue| issue.severity == Severity::Warning));
        assert!(
            tiny.iter().any(|issue| issue.node.is_some()),
            "geometry issues point at a cell so the user can jump to it"
        );
    }

    #[test]
    fn an_unreadable_timer_is_reported() {
        let mut layout = Layout::empty("Timer");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Vertical).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::CurrentSlide))
            .unwrap();
        layout
            .set_widget(ids[1], Widget::new(WidgetKind::Timer))
            .unwrap();
        layout.set_child_size(layout.root.id(), 1, 0.05).unwrap();

        let messages = messages(&validate(&layout, area()));
        assert!(
            messages
                .iter()
                .any(|m| m.contains("Timer text may be unreadable")),
            "{messages:?}"
        );
    }

    #[test]
    fn instance_limit_violations_block() {
        // Build the violation directly: the editing API refuses to create it,
        // which is exactly why import needs this check.
        let mut layout = Layout::empty("Broken");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::SpeakerNotes))
            .unwrap();
        layout.root.cell_mut(ids[1]).unwrap().widget = Some(Widget::new(WidgetKind::SpeakerNotes));

        let issues = validate(&layout, area());
        assert!(is_blocked(&issues));
        assert!(messages(&issues)
            .iter()
            .any(|m| m.contains("Speaker Notes appears 2 times")));
    }

    #[test]
    fn a_non_canonical_tree_blocks() {
        let mut layout = Layout::empty("Broken");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        // Force a same-direction nested split past the editing API.
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        let inner = crate::layout::tree::Split {
            id: NodeId(90),
            name: None,
            direction: Direction::Horizontal,
            children: vec![
                crate::layout::tree::Node::Leaf(crate::layout::tree::Cell::new(NodeId(91))),
                crate::layout::tree::Node::Leaf(crate::layout::tree::Cell::new(NodeId(92))),
            ],
            sizes: vec![0.5, 0.5],
            gap: 8.0,
            min_child: 0.05,
        };
        *layout.root.find_mut(ids[0]).unwrap() = crate::layout::tree::Node::Split(inner);

        let issues = validate(&layout, area());
        assert!(is_blocked(&issues));
        assert!(messages(&issues)
            .iter()
            .any(|m| m.contains("not canonical")));
    }

    #[test]
    fn collapsed_empty_cells_do_not_squeeze_their_siblings_in_validation() {
        let mut layout = Layout::empty("Collapse");
        let root = layout.root.id();
        layout.split_cell(root, Direction::Horizontal).unwrap();
        let ids: Vec<NodeId> = layout.cells().iter().map(|cell| cell.id).collect();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::SpeakerNotes))
            .unwrap();
        layout
            .set_cell_properties(
                ids[1],
                0.0,
                crate::layout::tree::CellBackground::None,
                crate::layout::tree::CellBorder::None,
                crate::layout::tree::EmptyBehavior::Collapse,
            )
            .unwrap();

        // The notes panel gets the whole width at presentation time, so it is
        // not reported as cramped even though half the canvas is "empty".
        let issues = validate(&layout, Frame::new(0.0, 0.0, 560.0, 400.0));
        assert!(
            !messages(&issues)
                .iter()
                .any(|m| m.contains("Speaker notes are too small")),
            "{issues:?}"
        );
    }
}
