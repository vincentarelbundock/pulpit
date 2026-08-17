//! `pulpit.status.blank` — a static, decorative spacer cell.
//!
//! The living template for adding a widget: no new service, no
//! configuration, no capability. Everything it needs is already in scope —
//! [`crate::theme`]'s panel background — so the only work here is a `view`
//! function. See the doc comment at the top of `widgets/registry.rs` for
//! the full list of files a new kind touches; this one adds a module.

use iced::widget::container;
use iced::{Element, Length};

use crate::theme;

/// A themed panel with nothing in it, for balancing a layout on purpose.
pub fn view<'a, Message: 'static>() -> Element<'a, Message> {
    container(iced::widget::space::horizontal())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::ambient::surface)
        .into()
}
