//! The application's one vertical scrollbar.
//!
//! Content and event handling belong to callers; track and thumb mechanics
//! live here so every scrolling surface looks and behaves the same.

use iced::widget::{scrollable, Scrollable};
use iced::Element;
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

/// The shared track and thumb geometry.
pub fn bar() -> scrollable::Scrollbar {
    scrollable::Scrollbar::new()
        .width(super::tokens::SCROLL_HANDLE_WIDTH)
        .scroller_width(super::tokens::SCROLL_HANDLE_WIDTH)
        .min_scroller_length(super::tokens::SCROLL_HANDLE_MIN_LENGTH)
}

/// A vertically scrolling surface using the shared scrollbar.
pub fn vertical<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message> {
    scrollable(content).direction(scrollable::Direction::Vertical(bar()))
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
}
