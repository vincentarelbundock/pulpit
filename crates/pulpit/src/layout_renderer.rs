//! Drawing a layout tree.
//!
//! This module owns traversal, proportional sizing, cell chrome and empty-cell
//! behaviour — and nothing else. Every arm of the dispatch below hands
//! straight to a widget family; no widget's own composition lives here.
//!
//! One renderer serves three purposes: the live presenter screen, the editor's
//! preview with sample content, and the editor canvas. Having one is what
//! makes "what you designed is what you present" true rather than aspirational.

use iced::widget::{container, opaque, space, Column, Row, Stack};
use iced::{Element, Length, Padding};

use crate::layout::{Direction, Layout, Node};
use crate::theme;
use crate::widgets::context::Context;
use crate::widgets::event::WidgetEvent;
use crate::widgets::registry;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::Widget;

/// Draw a whole layout.
pub fn layout<'a, Message: Clone + 'static>(
    layout: &Layout,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    node(&layout.root, context, compose, on_event, false)
}

/// Put a transient side panel beside a working surface.
///
/// This is the application-owned half of a side panel: it controls geometry,
/// clipping and the reveal animation. The caller supplies only the panel's
/// contents, just as a layout cell supplies the outline widget's contents.
pub fn side_panel<'a, Message: 'a>(
    panel: impl Into<Element<'a, Message>>,
    surface: impl Into<Element<'a, Message>>,
    width: f32,
    reveal: f32,
) -> Element<'a, Message> {
    let reveal = reveal.clamp(0.0, 1.0);
    // The row stands whether or not the panel is showing. Handing the
    // surface back bare would move it to the root of this position in the
    // tree and back again, and the widget state it carries — the page's own
    // scroll position among it — is matched by position.
    mounted_row(vec![
        container(panel_contents(panel, reveal))
            .width(Length::Fixed(revealed(width, reveal)))
            .height(Length::Fill)
            .clip(true)
            .into(),
        surface.into(),
    ])
}

/// Float a transient side panel over a working surface.
///
/// A narrow document window cannot spare a permanent rail without turning
/// the page into a sliver. The panel still owns a dependable width, while the
/// page keeps the whole viewport underneath it. `opaque` prevents presses in
/// the drawer from leaking through to links and form fields on the page.
pub fn overlay_side_panel<'a, Message: 'a>(
    panel: impl Into<Element<'a, Message>>,
    surface: impl Into<Element<'a, Message>>,
    width: f32,
    reveal: f32,
) -> Element<'a, Message> {
    let reveal = reveal.clamp(0.0, 1.0);
    // Mounted whether or not the drawer is showing, for the reason
    // `side_panel` gives: the surface must not change position in the tree.
    // `Stack::from_vec` rather than the macro, which drops a zero-width
    // layer on the floor and would take the surface's position with it.
    Stack::from_vec(vec![
        surface.into(),
        opaque(
            container(panel_contents(panel, reveal))
                .width(Length::Fixed(revealed(width, reveal)))
                .height(Length::Fill)
                .clip(true),
        ),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// A transient panel's contents, or nothing at all while it is closed. Its
/// container stays; what it holds does not have to.
fn panel_contents<'a, Message: 'a>(
    panel: impl Into<Element<'a, Message>>,
    reveal: f32,
) -> Element<'a, Message> {
    if reveal <= f32::EPSILON {
        space::horizontal().width(Length::Fixed(0.0)).into()
    } else {
        panel.into()
    }
}

fn node<'a, Message: Clone + 'static>(
    node: &Node,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
    inside_outline_rail: bool,
) -> Element<'a, Message> {
    match node {
        Node::Leaf(cell) => {
            let content: Element<'a, Message> = match (&cell.widget, &cell.unavailable) {
                (Some(widget), _)
                    if widget.kind() == crate::widgets::WidgetKind::DocumentOutline
                        && context.search_open =>
                {
                    self::widget_kind(
                        crate::widgets::WidgetKind::Search,
                        widget,
                        context,
                        compose,
                        on_event,
                    )
                }
                (Some(widget), _) => self::widget(widget, context, compose, on_event),
                (None, Some(unavailable)) => unavailable_panel(unavailable),
                (None, None) => blank_panel(),
            };
            // Every widget sits in the middle of its cell, both ways.
            // Anything that wants the whole cell still takes it: filling
            // content is unaffected by an alignment it never uses.
            container(content)
                .padding(Padding::from(cell.padding))
                .align_x(iced::Alignment::Center)
                .align_y(iced::Alignment::Center)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::ambient::cell_style(cell.background, false, false))
                .into()
        }
        Node::Split(split) => {
            // Only structural emptiness removes a child. A pane the reveal
            // has closed keeps its place in the split and takes no space:
            // dropping it shifts every sibling after it one position, and
            // widget state is matched by position, so the page surface would
            // be handed the state of whatever used to sit beside it — a
            // fresh scrollable at the top of the document.
            let visible: Vec<usize> = (0..split.children.len())
                .filter(|index| {
                    !(context.mode.collapse_empty() && is_collapsed(&split.children[*index]))
                })
                .collect();
            if visible.is_empty() {
                return space::horizontal().into();
            }

            // Flexible cells become fill portions, so authored proportions
            // still divide all remaining space. A hug cell takes only its
            // widget's functional minimum plus padding; the pure geometry
            // calculation uses the same `Node::hug_extent` rule.
            let portion = |index: usize| {
                (split.sizes[index]
                    * pane_reveal(&split.children[index], context, inside_outline_rail)
                    * 1000.0)
                    .max(1.0) as u16
            };

            match split.direction {
                Direction::Horizontal => {
                    let mut content = Vec::with_capacity(visible.len() * 2);
                    let mut previous_closed = false;
                    for (position, index) in visible.into_iter().enumerate() {
                        let reveal =
                            pane_reveal(&split.children[index], context, inside_outline_rail);
                        let closed = reveal <= f32::EPSILON;
                        if position > 0 {
                            content.push(split_separator(
                                split.direction,
                                split.gap,
                                !closed && !previous_closed,
                            ));
                        }
                        previous_closed = closed;
                        let width = if closed {
                            Length::Fixed(0.0)
                        } else {
                            split.children[index]
                                .hug_extent(split.direction)
                                .map_or_else(
                                    || Length::FillPortion(portion(index)),
                                    |extent| Length::Fixed(extent * reveal),
                                )
                        };
                        content.push(
                            container(pane(
                                closed,
                                &split.children[index],
                                context,
                                compose,
                                on_event,
                                inside_outline_rail,
                            ))
                            .width(width)
                            .height(Length::Fill)
                            .clip(true)
                            .into(),
                        );
                    }
                    mounted_row(content)
                }
                Direction::Vertical => {
                    let mut content = Vec::with_capacity(visible.len() * 2);
                    let mut previous_closed = false;
                    for (position, index) in visible.into_iter().enumerate() {
                        let reveal =
                            pane_reveal(&split.children[index], context, inside_outline_rail);
                        let closed = reveal <= f32::EPSILON;
                        if position > 0 {
                            content.push(split_separator(
                                split.direction,
                                split.gap,
                                !closed && !previous_closed,
                            ));
                        }
                        previous_closed = closed;
                        let height = if closed {
                            Length::Fixed(0.0)
                        } else {
                            split.children[index]
                                .hug_extent(split.direction)
                                .map_or_else(
                                    || Length::FillPortion(portion(index)),
                                    |extent| Length::Fixed(extent * reveal),
                                )
                        };
                        content.push(
                            container(pane(
                                closed,
                                &split.children[index],
                                context,
                                compose,
                                on_event,
                                inside_outline_rail,
                            ))
                            .width(Length::Fill)
                            .height(height)
                            .clip(true)
                            .into(),
                        );
                    }
                    mounted_column(content)
                }
            }
        }
    }
}

/// A row that keeps every child it is given, at the position it was given in.
///
/// `Row::push` quietly discards a child whose width or height is a fixed
/// zero, which is exactly what a closed pane and its retired gutter are. That
/// would undo the whole point of keeping them: the panes after them would
/// shift, and iced matches widget state to widgets by position. `from_vec`
/// does not inspect its children, so the width and height it does not infer
/// are set here instead.
fn mounted_row<'a, Message: 'a>(children: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    Row::from_vec(children)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The vertical half of [`mounted_row`], with the same reason behind it.
fn mounted_column<'a, Message: 'a>(children: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    Column::from_vec(children)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// One child of a split, or a placeholder standing in its position while it
/// is closed.
///
/// A closed pane holds its place so its siblings keep theirs, but its
/// contents are not built: nothing inside a rail nobody can see is worth
/// laying out, and its own state is expected to start again when it opens,
/// exactly as it did when the pane was dropped altogether.
fn pane<'a, Message: Clone + 'static>(
    closed: bool,
    child: &Node,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
    inside_outline_rail: bool,
) -> Element<'a, Message> {
    if closed {
        return space::horizontal().width(Length::Fixed(0.0)).into();
    }
    self::node(
        child,
        context,
        compose,
        on_event,
        inside_outline_rail || is_outline_rail(child),
    )
}

/// The outline is a layout pane, not content folded inside one. Scaling its
/// portion gives the released width (or height in a custom layout) back to
/// its siblings throughout the disclosure animation.
fn pane_reveal(node: &Node, context: &Context<'_>, inside_outline_rail: bool) -> f32 {
    if !inside_outline_rail && is_outline_rail(node) {
        revealed(
            1.0,
            context.reader.outline_reveal.max(context.search_reveal),
        )
    } else {
        1.0
    }
}

/// The one reveal calculation used by both layout-owned and transient rails.
fn revealed(full_extent: f32, reveal: f32) -> f32 {
    full_extent * reveal.clamp(0.0, 1.0)
}

/// A rail is the largest subtree that contains the outline but not the page.
/// In the built-in Reader this includes Search, so collapsing means the whole
/// sidebar disappears rather than leaving its lower half behind.
fn is_outline_rail(node: &Node) -> bool {
    contains(node, crate::widgets::WidgetKind::DocumentOutline)
        && !contains(node, crate::widgets::WidgetKind::DocumentPage)
}

fn contains(node: &Node, kind: crate::widgets::WidgetKind) -> bool {
    match node {
        Node::Leaf(cell) => cell
            .widget
            .as_ref()
            .is_some_and(|widget| widget.kind() == kind),
        Node::Split(split) => split.children.iter().any(|child| contains(child, kind)),
    }
}

/// One muted line between adjacent cells, centred in the space reserved for
/// their gutter. There is deliberately no corresponding line at the outside
/// edge of a layout.
fn split_separator<Message: 'static>(
    direction: Direction,
    gap: f32,
    drawn: bool,
) -> Element<'static, Message> {
    // A separator beside a closed pane keeps its position in the split and
    // gives up its gutter: there is no boundary left to mark.
    if !drawn {
        return space::horizontal().width(Length::Fixed(0.0)).into();
    }
    let width = crate::widgets::tokens::CELL_SEPARATOR_WIDTH;
    let gutter = gap.max(width);
    let line = container(space::horizontal())
        .width(match direction {
            Direction::Horizontal => Length::Fixed(width),
            Direction::Vertical => Length::Fill,
        })
        .height(match direction {
            Direction::Horizontal => Length::Fill,
            Direction::Vertical => Length::Fixed(width),
        })
        .style(theme::ambient::separator);

    container(line)
        .width(match direction {
            Direction::Horizontal => Length::Fixed(gutter),
            Direction::Vertical => Length::Fill,
        })
        .height(match direction {
            Direction::Horizontal => Length::Fill,
            Direction::Vertical => Length::Fixed(gutter),
        })
        .align_x(iced::Alignment::Center)
        .align_y(iced::Alignment::Center)
        .into()
}

fn is_collapsed(node: &Node) -> bool {
    match node {
        Node::Leaf(cell) => {
            cell.is_empty() && cell.empty_behavior == crate::layout::EmptyBehavior::Collapse
        }
        Node::Split(split) => split.children.iter().all(is_collapsed),
    }
}

fn blank_panel<Message: 'static>() -> Element<'static, Message> {
    space::horizontal()
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// A cell whose saved widget id this build does not know. Static and inert:
/// it names what was there rather than pretending to be a working widget or
/// silently discarding it.
fn unavailable_panel<'a, Message: 'static>(
    unavailable: &crate::layout::UnavailableWidget,
) -> Element<'a, Message> {
    use iced::widget::text;

    container(
        text(format!("Unknown widget\n{}", unavailable.widget_id))
            .size(theme::tokens::type_scale::CAPTION)
            .align_x(iced::Alignment::Center),
    )
    .padding(crate::theme::space::S)
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(iced::Alignment::Center)
    .align_y(iced::Alignment::Center)
    .style(theme::ambient::notice)
    .into()
}

/// Hand one widget to the family that implements it.
///
/// Each arm passes only the facets that family uses, which is what keeps a
/// renderer's dependencies visible in its signature.
pub fn widget<'a, Message: Clone + 'static>(
    widget: &Widget,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    let scale = widget.style.scale.clamp(
        crate::widgets::common::SCALE_RANGE.0,
        crate::widgets::common::SCALE_RANGE.1,
    );
    let ctx = WidgetViewContext::new(context, compose, on_event, scale);

    registry::dispatch(&ctx, widget)
}

fn widget_kind<'a, Message: Clone + 'static>(
    kind: crate::widgets::WidgetKind,
    host: &Widget,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    let scale = host.style.scale.clamp(
        crate::widgets::common::SCALE_RANGE.0,
        crate::widgets::common::SCALE_RANGE.1,
    );
    let ctx = WidgetViewContext::new(context, compose, on_event, scale);
    registry::dispatch_kind(kind, &ctx, host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::sample::context;
    use crate::widgets::{Mode, WidgetKind};

    #[test]
    fn every_widget_can_be_drawn_in_every_mode() {
        // The dispatcher is exhaustive at compile time; this proves each arm
        // actually builds an element rather than panicking on a facet it was
        // not given.
        for mode in [Mode::Live, Mode::Preview, Mode::Editing] {
            let context = context(mode);
            for kind in WidgetKind::ALL {
                let widget = Widget::new(kind);
                let _: iced::Element<'_, ()> = self::widget(&widget, &context, None, |_| ());
            }
        }
    }

    #[test]
    fn a_whole_layout_draws_in_every_mode() {
        for mode in [Mode::Live, Mode::Preview, Mode::Editing] {
            let context = context(mode);
            for built_in in crate::layout::builtin::built_in_layouts() {
                let _: iced::Element<'_, ()> = layout(&built_in, &context, None, |_| ());
            }
        }
    }

    #[test]
    fn the_reader_collapses_its_outline_rail() {
        let layout =
            crate::layout::builtin::reader_default(crate::layout::AspectRatio::SixteenNine);
        let root = layout.root.as_split().expect("the reader root is split");
        let body = root.children[1]
            .as_split()
            .expect("the reader body is split");
        let rail = &body.children[0];
        assert!(is_outline_rail(rail));
        assert!(contains(rail, WidgetKind::DocumentOutline));
        assert!(!contains(rail, WidgetKind::Search));

        let mut context = context(Mode::Live);
        context.reader.outline_reveal = 0.0;
        assert_eq!(pane_reveal(rail, &context, false), 0.0);

        context.search_reveal = 1.0;
        assert_eq!(
            pane_reveal(rail, &context, false),
            1.0,
            "search reuses and reveals the outline rail"
        );
    }

    /// Iced matches widget state to widgets by position among their
    /// siblings. A rail that leaves the split when it closes therefore hands
    /// the page surface a stranger's state on the way back in, which is what
    /// used to drop the reader at the top of the document every time the
    /// bookmarks or the search panel were opened and closed again.
    #[test]
    fn closing_the_outline_rail_leaves_its_siblings_where_they_are() {
        use iced::advanced::widget::Tree;

        let layout =
            crate::layout::builtin::reader_default(crate::layout::AspectRatio::SixteenNine);
        let body = layout
            .root
            .as_split()
            .expect("the reader root is split")
            .children[1]
            .clone();

        let shape = |reveal: f32| {
            let mut context = context(Mode::Live);
            context.reader.outline_reveal = reveal;
            context.search_reveal = 0.0;
            let element: iced::Element<'_, ()> = self::node(&body, &context, None, |_| (), false);
            let tree = Tree::new(&element);
            // The row's own children: every pane and every gutter between
            // them, however little of the window they are taking.
            tree.children.len()
        };

        assert_eq!(
            shape(0.0),
            shape(1.0),
            "a closed rail keeps its place in the split"
        );
    }
}
