//! The application's one vertical scrollbar.
//!
//! Content and event handling belong to callers; track and thumb mechanics
//! live here so every scrolling surface looks and behaves the same.

use iced::widget::{scrollable, Scrollable};
use iced::Element;

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
