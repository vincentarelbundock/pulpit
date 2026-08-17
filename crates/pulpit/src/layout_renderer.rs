//! Drawing a layout tree.
//!
//! This module owns traversal, proportional sizing, cell chrome and empty-cell
//! behaviour — and nothing else. Every arm of the dispatch below hands
//! straight to a widget family; no widget's own composition lives here.
//!
//! One renderer serves three purposes: the live presenter screen, the editor's
//! preview with sample content, and the editor canvas. Having one is what
//! makes "what you designed is what you present" true rather than aspirational.

use iced::widget::{container, space, Column, Row};
use iced::{Element, Length, Padding};
#[cfg(test)]
use pulpit_render::cache::FrameKind;

use crate::layout::{Direction, Layout, Node};
use crate::theme;
use crate::widgets::context::Context;
use crate::widgets::event::WidgetEvent;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{annotations, navigation, notes, slides, status, timing, Family, Widget};

/// Draw a whole layout.
pub fn layout<'a, Message: Clone + 'static>(
    layout: &Layout,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    node(&layout.root, context, compose, on_event)
}

fn node<'a, Message: Clone + 'static>(
    node: &Node,
    context: &Context<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    match node {
        Node::Leaf(cell) => {
            let content: Element<'a, Message> = match &cell.widget {
                Some(widget) => self::widget(widget, context, compose, on_event),
                None => blank_panel(),
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
                })
                .collect();
            if visible.is_empty() {
                return space::horizontal().into();
            }

            // Proportions become fill portions, so the layout scales to any
            // window without letterboxing or reflow.
            let portion = |index: usize| (split.sizes[index] * 1000.0).max(1.0) as u16;

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
                            ))
                            .width(Length::FillPortion(portion(index)))
                            .height(Length::Fill),
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
                            ))
                            .width(Length::Fill)
                            .height(Length::FillPortion(portion(index))),
                        );
                    }
                    content.width(Length::Fill).height(Length::Fill).into()
                }
            }
        }
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

    match widget.kind().family() {
        Family::Slides => slides::view::view(&ctx, widget),
        Family::Annotations => annotations::view::view(&ctx, widget),
        Family::Notes => notes::view::view(&ctx, widget),
        Family::Media => crate::widgets::media::view::view(&ctx, widget),
        Family::Timing => timing::view::view(&ctx, widget),
        Family::Navigation => navigation::view::view(&ctx, widget),
        Family::Document => crate::widgets::document::view::view(&ctx, widget),
        Family::Search => crate::widgets::search::view::view(&ctx, widget),
        Family::Chrome => crate::widgets::chrome::view::view(&ctx, widget),
        Family::Status => status::view::view(&ctx, widget),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::context::{AudienceData, DocumentData, SlideData, TimingData};
    use crate::widgets::{sample, Mode, WidgetKind};
    use iced::widget::image::Handle;
    use pulpit_core::Blank;

    /// No pictures: what matters here is that every widget can be built.
    struct NoFrames;

    impl crate::widgets::context::FrameSource for NoFrames {
        fn frame(&self, _slide: usize, _kind: FrameKind, _max_width: u32) -> Option<Handle> {
            None
        }
    }

    fn context(mode: Mode) -> Context<'static> {
        Context {
            mode,
            search: crate::widgets::context::SearchData {
                state: &sample::SEARCH,
            },
            slides: SlideData {
                current: sample::SLIDE,
                preview: sample::SLIDE,
                count: sample::SLIDE_COUNT,
                frames: &NoFrames,
                preview_width: 640,
                aspect: 16.0 / 9.0,
                text_notes: None,
                has_links: false,
                link_highlights: Vec::new(),
                overlays: Vec::new(),
                crop: pulpit_core::notes::Region::FULL,
                annotations: &sample::ANNOTATIONS,
                rendered_text: {
                    static EMPTY: std::sync::LazyLock<
                        std::sync::Arc<
                            std::collections::HashMap<u64, crate::typst_annotation::RenderedText>,
                        >,
                    > = std::sync::LazyLock::new(|| {
                        std::sync::Arc::new(std::collections::HashMap::new())
                    });
                    &EMPTY
                },
                marks_cache: std::rc::Rc::new(iced::widget::canvas::Cache::new()),
                annotation_controls: crate::widgets::AnnotationControls::default(),
                annotation_style: pulpit_core::annotation::AnnotationStyle::default(),
            },
            alarms: &sample::ALARMS,
            timer_controls: &sample::TIMER,
            timing: TimingData {
                elapsed: std::time::Duration::from_secs(12 * 60),
                target: Some(std::time::Duration::from_secs(40 * 60)),
                running: true,
                seconds_of_day: 13 * 3600,
            },
            document: DocumentData {
                title: sample::TITLE.to_string(),
                section: Some("Reconnection".to_string()),
                sample_notes: sample::NOTES,
            },
            reader: crate::widgets::sample::closed_reader(),
            audience: AudienceData {
                blank: Blank::Off,
                connected: true,
                fullscreen: true,
                started: true,
                menu_open: false,
            },
            media: crate::widgets::context::MediaData {
                transport: sample::transport(),
            },
        }
    }

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
}
