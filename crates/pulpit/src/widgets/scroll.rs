//! The application's one vertical scrollbar.
//!
//! Content and event handling belong to callers; track and thumb mechanics
//! live here so every scrolling surface looks and behaves the same.

use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{self, Widget};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::widget::{scrollable, Scrollable};
use iced::{Element, Event, Length, Rectangle, Size};
use std::ops::Range;

pub const OVERSCAN_ROWS: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct VirtualWindow {
    pub rows: Range<usize>,
    pub before: f32,
    pub after: f32,
}

/// The finite window to build for a fixed-height list. Off-screen rows remain
/// as exact spacers, so the native scrollbar continues to describe the whole
/// list and every panel shares one virtualization rule.
pub fn virtual_window(count: usize, row_height: f32, offset: f32, viewport: f32) -> VirtualWindow {
    if count == 0 || row_height <= 0.0 || viewport <= 0.0 {
        return VirtualWindow {
            rows: 0..0,
            before: 0.0,
            after: count as f32 * row_height.max(0.0),
        };
    }
    let first_visible = (offset.max(0.0) / row_height).floor() as usize;
    let visible = (viewport / row_height).ceil() as usize;
    let start = first_visible.saturating_sub(OVERSCAN_ROWS).min(count);
    let end = (first_visible + visible + OVERSCAN_ROWS + 1).min(count);
    VirtualWindow {
        rows: start..end,
        before: start as f32 * row_height,
        after: count.saturating_sub(end) as f32 * row_height,
    }
}

/// The finite window for a list whose rows have content-driven heights.
pub fn variable_window(heights: &[f32], offset: f32, viewport: f32) -> VirtualWindow {
    let total: f32 = heights.iter().sum();
    if heights.is_empty() || viewport <= 0.0 {
        return VirtualWindow {
            rows: 0..0,
            before: 0.0,
            after: total,
        };
    }
    let offset = offset.max(0.0);
    let mut top = 0.0;
    let first = heights
        .iter()
        .position(|height| {
            top += height.max(0.0);
            top > offset
        })
        .unwrap_or(heights.len());
    let mut bottom = top;
    let mut last = first;
    while last < heights.len() && bottom < offset + viewport {
        bottom += heights[last].max(0.0);
        last += 1;
    }
    let start = first.saturating_sub(OVERSCAN_ROWS);
    let end = (last + OVERSCAN_ROWS).min(heights.len());
    let before = heights[..start].iter().sum();
    let after = heights[end..].iter().sum();
    VirtualWindow {
        rows: start..end,
        before,
        after,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealDirection {
    Up,
    Down,
    Nearest,
}

/// Reveal a row only when it has left the viewport. Directional movement
/// places it at the edge being travelled toward; an external jump uses the
/// nearest edge and avoids needless motion.
pub fn reveal_offset(
    index: usize,
    row_height: f32,
    offset: f32,
    viewport: f32,
    count: usize,
    direction: RevealDirection,
) -> f32 {
    if count == 0 || row_height <= 0.0 || viewport <= 0.0 {
        return 0.0;
    }
    let row_top = index.min(count - 1) as f32 * row_height;
    let row_bottom = row_top + row_height;
    let target = if row_top < offset {
        row_top
    } else if row_bottom > offset + viewport {
        row_bottom - viewport
    } else {
        return offset;
    };
    let directional = match direction {
        RevealDirection::Up => row_top,
        RevealDirection::Down => row_bottom - viewport,
        RevealDirection::Nearest => target,
    };
    directional.clamp(0.0, (count as f32 * row_height - viewport).max(0.0))
}

pub fn reveal_variable_offset(
    index: usize,
    heights: &[f32],
    offset: f32,
    viewport: f32,
    direction: RevealDirection,
) -> f32 {
    if heights.is_empty() || viewport <= 0.0 {
        return 0.0;
    }
    let index = index.min(heights.len() - 1);
    let row_top: f32 = heights[..index].iter().sum();
    let row_bottom = row_top + heights[index];
    let nearest = if row_top < offset {
        row_top
    } else if row_bottom > offset + viewport {
        row_bottom - viewport
    } else {
        return offset;
    };
    let target = match direction {
        RevealDirection::Up => row_top,
        RevealDirection::Down => row_bottom - viewport,
        RevealDirection::Nearest => nearest,
    };
    let total: f32 = heights.iter().sum();
    target.clamp(0.0, (total - viewport).max(0.0))
}

/// The shared track and thumb geometry.
pub fn bar() -> scrollable::Scrollbar {
    // Width zero: iced still scrolls on the wheel and by keyboard, but draws
    // no rail and claims no hit area, so the lane is free for `thumbed` to
    // draw in and to grab in.
    scrollable::Scrollbar::hidden()
}

/// A vertically scrolling surface using the shared scrollbar.
pub fn vertical<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message> {
    scrollable(content)
        .direction(scrollable::Direction::Vertical(bar()))
        .style(crate::theme::ambient::scrollbar)
}

/// Where the thumb sits in its track, and how long it is.
///
/// This is the whole of what the vendored `iced_widget` fork used to change
/// inside iced, lifted out to where it can be read and tested. iced's own
/// scrollbar makes the thumb exactly `track x ratio` with a hard floor of two
/// pixels, which on a long document is a sliver nobody can grab. Ours has a
/// floor worth aiming at — and once a floor is worth aiming at, the offset
/// has to be mapped through the *shortened* track rather than the full one,
/// or the thumb hangs past the bottom by however much the floor added.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    /// Distance from the top of the track to the top of the thumb.
    pub top: f32,
    pub length: f32,
}

/// The thumb for a surface, or `None` when everything already fits.
pub fn thumb(offset: f32, viewport: f32, content: f32) -> Option<Thumb> {
    if viewport <= 0.0 || content <= viewport {
        return None;
    }
    let length = (viewport * (viewport / content))
        .max(super::tokens::SCROLL_HANDLE_MIN_LENGTH)
        .min(viewport);
    let scroll_range = content - viewport;
    let track_range = (viewport - length).max(0.0);
    let travelled = offset.clamp(0.0, scroll_range) / scroll_range;
    Some(Thumb {
        top: travelled * track_range,
        length,
    })
}

/// The content offset that puts the thumb's top at `top`.
///
/// The inverse of [`thumb`], so dragging lands exactly where releasing and
/// re-reading the offset would put it.
pub fn offset_for_thumb_top(top: f32, viewport: f32, content: f32) -> f32 {
    let Some(thumb) = thumb(0.0, viewport, content) else {
        return 0.0;
    };
    let track_range = (viewport - thumb.length).max(0.0);
    if track_range <= 0.0 {
        return 0.0;
    }
    let scroll_range = content - viewport;
    (top.clamp(0.0, track_range) / track_range) * scroll_range
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_list_builds_only_a_window_and_exact_spacers() {
        let window = virtual_window(1_000, 32.0, 3200.0, 320.0);
        assert!(window.rows.start > 0);
        assert!(window.rows.end < 1_000);
        assert_eq!(window.before, window.rows.start as f32 * 32.0);
        assert_eq!(window.after, (1_000 - window.rows.end) as f32 * 32.0);
        assert!(window.rows.len() <= 10 + OVERSCAN_ROWS * 2 + 1);
    }

    #[test]
    fn reveal_moves_only_after_the_selection_leaves_the_viewport() {
        assert_eq!(
            reveal_offset(4, 32.0, 96.0, 160.0, 100, RevealDirection::Down),
            96.0
        );
        assert_eq!(
            reveal_offset(9, 32.0, 96.0, 160.0, 100, RevealDirection::Down),
            160.0
        );
        assert_eq!(
            reveal_offset(1, 32.0, 96.0, 160.0, 100, RevealDirection::Up),
            32.0
        );
    }

    #[test]
    fn variable_rows_keep_exact_spacers_and_reveal_tall_entries() {
        let heights = [20.0, 40.0, 20.0, 60.0, 20.0];
        let window = variable_window(&heights, 60.0, 40.0);
        assert_eq!(
            window.before + heights[window.rows.clone()].iter().sum::<f32>() + window.after,
            160.0
        );
        assert_eq!(
            reveal_variable_offset(3, &heights, 0.0, 80.0, RevealDirection::Down),
            60.0
        );
    }

    #[test]
    fn a_surface_that_fits_has_no_thumb_at_all() {
        assert_eq!(thumb(0.0, 400.0, 400.0), None);
        assert_eq!(thumb(0.0, 400.0, 120.0), None);
    }

    #[test]
    fn a_very_long_document_still_gets_a_thumb_worth_grabbing() {
        // iced's own rule is `track * ratio` with a two-point floor, which
        // here would be under a pixel. The floor is the whole reason this
        // arithmetic is ours.
        let thumb = thumb(0.0, 800.0, 400_000.0).expect("a scrollable surface has a thumb");
        assert_eq!(thumb.length, super::super::tokens::SCROLL_HANDLE_MIN_LENGTH);
    }

    #[test]
    fn the_thumb_reaches_the_bottom_of_its_track_and_no_further() {
        // The bug the floor would otherwise introduce: map the offset through
        // the full track rather than the shortened one and the thumb hangs
        // past the end by however much the floor added.
        let viewport = 800.0;
        let content = 400_000.0;
        let bottom = thumb(content - viewport, viewport, content).expect("a thumb");
        assert!((bottom.top + bottom.length - viewport).abs() < 0.01);

        let top = thumb(0.0, viewport, content).expect("a thumb");
        assert_eq!(top.top, 0.0);
    }

    #[test]
    fn dragging_to_a_position_reports_the_offset_that_puts_it_there() {
        let viewport = 600.0;
        let content = 9_000.0;
        for offset in [0.0, 1_200.0, 4_500.0, content - viewport] {
            let thumb = thumb(offset, viewport, content).expect("a thumb");
            let round_tripped = offset_for_thumb_top(thumb.top, viewport, content);
            assert!(
                (round_tripped - offset).abs() < 0.01,
                "{offset} came back as {round_tripped}"
            );
        }
    }

    #[test]
    fn a_drag_past_either_end_stops_at_the_end() {
        let viewport = 600.0;
        let content = 9_000.0;
        assert_eq!(offset_for_thumb_top(-500.0, viewport, content), 0.0);
        assert_eq!(
            offset_for_thumb_top(10_000.0, viewport, content),
            content - viewport
        );
    }

    #[test]
    fn a_surface_with_nothing_to_scroll_reports_no_offset() {
        assert_eq!(offset_for_thumb_top(120.0, 400.0, 400.0), 0.0);
    }
}

/// Draw pulpit's own thumb over a scrolling surface.
///
/// iced's scrollbar is hidden underneath (see [`bar`]) because its thumb
/// length is not ours to set and its hit area follows its own geometry: a
/// thumb drawn at one length and grabbable at another is worse than either.
/// So the lane is ours entirely — drawn here, and dragged here.
///
/// `offset` is the surface's current scroll position, which every caller
/// already tracks to virtualize its rows. Viewport and content heights are
/// read from the laid-out child, so callers do not have to report those too.
pub fn thumbed<'a, Message: 'a>(
    surface: impl Into<Element<'a, Message>>,
    offset: f32,
    on_drag: impl Fn(f32) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(Thumbed {
        surface: surface.into(),
        offset,
        on_drag: Box::new(on_drag),
    })
}

struct Thumbed<'a, Message> {
    surface: Element<'a, Message>,
    offset: f32,
    on_drag: Box<dyn Fn(f32) -> Message + 'a>,
}

/// Where the pointer took hold of the thumb, measured from the thumb's top.
///
/// Keeping the grab point rather than re-centring on the cursor is what makes
/// a drag feel attached to the thumb instead of snapping it under the pointer.
#[derive(Debug, Default, Clone, Copy)]
struct Grab {
    held_at: Option<f32>,
    /// Whether the pointer was over the lane when the thumb was last drawn.
    ///
    /// The thumb changes colour on hover, and nothing else in the surface
    /// necessarily asks for a repaint when the pointer merely crosses into
    /// the lane — so without this the new colour waits for whatever redraw
    /// happens next.
    hovered: bool,
}

impl<Message> Thumbed<'_, Message> {
    /// The lane the thumb lives in: the right edge of the surface.
    fn lane(bounds: Rectangle) -> Rectangle {
        let width = super::tokens::SCROLL_LANE_WIDTH.min(bounds.width);
        Rectangle {
            x: bounds.x + bounds.width - width,
            width,
            ..bounds
        }
    }

    /// The drawn thumb, in window coordinates.
    fn thumb_bounds(&self, layout: Layout<'_>) -> Option<Rectangle> {
        let bounds = layout.bounds();
        let content = layout.children().next()?.bounds().height;
        let thumb = thumb(self.offset, bounds.height, content)?;
        let lane = Self::lane(bounds);
        let width = super::tokens::SCROLL_HANDLE_WIDTH.min(lane.width);
        Some(Rectangle {
            x: lane.x + (lane.width - width) / 2.0,
            y: bounds.y + thumb.top,
            width,
            height: thumb.length,
        })
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Thumbed<'_, Message> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<Grab>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(Grab::default())
    }

    fn children(&self) -> Vec<widget::Tree> {
        vec![widget::Tree::new(&self.surface)]
    }

    fn diff(&self, tree: &mut widget::Tree) {
        tree.diff_children(&[self.surface.as_widget()]);
    }

    fn size(&self) -> Size<Length> {
        self.surface.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.surface.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.surface
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let content = layout.children().next().map(|node| node.bounds().height);
        let thumb_bounds = self.thumb_bounds(layout);

        // Taken before the child sees it, and only inside the lane: a press
        // anywhere else is the content's, including a press in the lane when
        // there is nothing to scroll and so no thumb drawn.
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let (Some(position), Some(thumb_bounds)) = (cursor.position(), thumb_bounds) {
                    if Self::lane(bounds).contains(position) {
                        let held_at = if thumb_bounds.contains(position) {
                            position.y - thumb_bounds.y
                        } else {
                            // A press in the empty track jumps the thumb to
                            // the pointer and carries on as a drag, which is
                            // what a scrollbar that can be grabbed anywhere
                            // is expected to do.
                            thumb_bounds.height / 2.0
                        };
                        tree.state.downcast_mut::<Grab>().held_at = Some(held_at);
                        if let Some(content) = content {
                            shell.publish((self.on_drag)(offset_for_thumb_top(
                                position.y - bounds.y - held_at,
                                bounds.height,
                                content,
                            )));
                        }
                        shell.capture_event();
                        return;
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hovering = cursor
                    .position()
                    .is_some_and(|position| Self::lane(bounds).contains(position))
                    && thumb_bounds.is_some();
                let grab = tree.state.downcast_mut::<Grab>();
                if grab.hovered != hovering {
                    grab.hovered = hovering;
                    shell.request_redraw();
                }
                let held_at = grab.held_at;
                if let (Some(held_at), Some(position), Some(content)) =
                    (held_at, cursor.position(), content)
                {
                    shell.publish((self.on_drag)(offset_for_thumb_top(
                        position.y - bounds.y - held_at,
                        bounds.height,
                        content,
                    )));
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let grab = tree.state.downcast_mut::<Grab>();
                if grab.held_at.take().is_some() {
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }

        self.surface.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let over_lane = cursor
            .position()
            .is_some_and(|position| Self::lane(layout.bounds()).contains(position))
            && self.thumb_bounds(layout).is_some();
        if tree.state.downcast_ref::<Grab>().held_at.is_some() || over_lane {
            return mouse::Interaction::Pointer;
        }
        self.surface.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.surface.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );

        let Some(thumb_bounds) = self.thumb_bounds(layout) else {
            return;
        };
        // The same three states iced's own rail described, so the thumb reads
        // as the one pulpit always had: quiet at rest, clearer under the
        // pointer, accented while it is being dragged.
        let palette = crate::theme::ambient::palette();
        let dragged = tree.state.downcast_ref::<Grab>().held_at.is_some();
        let hovered = cursor
            .position()
            .is_some_and(|position| Self::lane(layout.bounds()).contains(position));
        let colour = if dragged {
            palette.accent
        } else if hovered {
            palette.strong_border()
        } else {
            palette.border()
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: thumb_bounds,
                border: iced::Border {
                    radius: crate::theme::radius::SMALL.into(),
                    ..iced::Border::default()
                },
                ..renderer::Quad::default()
            },
            colour,
        );
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut widget::Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.surface.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
