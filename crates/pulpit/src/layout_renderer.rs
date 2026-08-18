//! Drawing a layout tree.
//!
//! This module owns traversal, proportional sizing, cell chrome and empty-cell
//! behaviour — and nothing else. Every arm of the dispatch below hands
//! straight to a widget family; no widget's own composition lives here.
//!
//! One renderer serves three purposes: the live presenter screen, the editor's
//! preview with sample content, and the editor canvas. Having one is what
//! makes "what you designed is what you present" true rather than aspirational.

use iced::widget::{container, row, space, Column, Row};
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
    if reveal <= f32::EPSILON {
        return surface.into();
    }

    row![
        container(panel)
            .width(Length::Fixed(revealed(width, reveal)))
            .height(Length::Fill)
            .clip(true),
        surface.into(),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
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
            let visible: Vec<usize> = (0..split.children.len())
                .filter(|index| {
                    !(context.mode.collapse_empty() && is_collapsed(&split.children[*index]))
                        && pane_reveal(&split.children[*index], context, inside_outline_rail)
                            > f32::EPSILON
                })
                .collect();
            if visible.is_empty() {
                return space::horizontal().into();
            }

            // Proportions become fill portions, so the layout scales to any
            // window without letterboxing or reflow.
            let portion = |index: usize| {
                (split.sizes[index]
                    * pane_reveal(&split.children[index], context, inside_outline_rail)
                    * 1000.0)
                    .max(1.0) as u16
            };

            match split.direction {
                Direction::Horizontal => {
                    let mut content = Row::new();
                    for (position, index) in visible.into_iter().enumerate() {
                        if position > 0 {
                            content = content.push(split_separator(split.direction, split.gap));
                        }
                        content = content.push(
                            container(self::node(
                                &split.children[index],
                                context,
                                compose,
                                on_event,
                                inside_outline_rail || is_outline_rail(&split.children[index]),
                            ))
                            .width(Length::FillPortion(portion(index)))
                            .height(Length::Fill)
                            .clip(true),
                        );
                    }
                    content.width(Length::Fill).height(Length::Fill).into()
                }
                Direction::Vertical => {
                    let mut content = Column::new();
                    for (position, index) in visible.into_iter().enumerate() {
                        if position > 0 {
                            content = content.push(split_separator(split.direction, split.gap));
                        }
                        content = content.push(
                            container(self::node(
                                &split.children[index],
                                context,
                                compose,
                                on_event,
                                inside_outline_rail || is_outline_rail(&split.children[index]),
                            ))
                            .width(Length::Fill)
                            .height(Length::FillPortion(portion(index)))
                            .clip(true),
                        );
                    }
                    content.width(Length::Fill).height(Length::Fill).into()
                }
            }
        }
    }
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
fn split_separator<Message: 'static>(direction: Direction, gap: f32) -> Element<'static, Message> {
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
    let accent = theme::ambient::accent();
    let scale = widget.style.scale.clamp(
        crate::widgets::common::SCALE_RANGE.0,
        crate::widgets::common::SCALE_RANGE.1,
    );
    let ctx = WidgetViewContext::new(context, compose, on_event, accent, scale);

    registry::dispatch(&ctx, widget)
}

fn widget_kind<'a, Message: Clone + 'static>(
    kind: crate::widgets::WidgetKind,
    host: &Widget,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    let accent = theme::ambient::accent();
    let scale = host.style.scale.clamp(
        crate::widgets::common::SCALE_RANGE.0,
        crate::widgets::common::SCALE_RANGE.1,
    );
    let ctx = WidgetViewContext::new(context, compose, on_event, accent, scale);
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
}
