//! What a family's `view` entry point is handed.
//!
//! [`Context`](crate::widgets::context::Context) is the domain snapshot: one
//! facet per concern, borrowed from the application. This wraps it with the
//! few things the layout renderer used to compute once and thread through by
//! hand — the message constructor, the composing editor's content, and the
//! ambient scale — so every family's entry point takes the same
//! four words rather than its own hand-picked slice of them.

use crate::widgets::context::Context;
use crate::widgets::event::WidgetEvent;

/// Everything a widget family's `view` entry point may draw from.
///
/// Individual views still reach into `context` for the facet they need
/// (`ctx.context.slides`, `ctx.context.mode`, …); nothing here duplicates
/// what [`Context`] already carries.
// Two lifetimes, not one: `context` is borrowed only for the call, while
// `'a` is what the composing editor's content — and so the drawn element —
// is allowed to outlive it by. Collapsing them into one would force every
// call site to keep its `Context` alive as long as the `Element` it draws,
// which the document family's own signature never required.
pub struct WidgetViewContext<'ctx, 'a, Message> {
    pub context: &'ctx Context<'ctx>,
    /// The mark being typed into, when one is open on the page (§8.5).
    /// `None` outside the document family's page surface.
    pub compose: Option<&'a iced::widget::text_editor::Content>,
    /// How a family turns a widget interaction into the application's
    /// message type.
    pub on_event: fn(WidgetEvent) -> Message,
    /// The widget's content scale, already clamped to
    /// [`crate::widgets::common::SCALE_RANGE`].
    pub scale: f32,
}

impl<'ctx, 'a, Message> WidgetViewContext<'ctx, 'a, Message> {
    pub fn new(
        context: &'ctx Context<'ctx>,
        compose: Option<&'a iced::widget::text_editor::Content>,
        on_event: fn(WidgetEvent) -> Message,
        scale: f32,
    ) -> Self {
        Self {
            context,
            compose,
            on_event,
            scale,
        }
    }
}
