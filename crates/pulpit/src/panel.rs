//! The one popup shape: a darkened ground, a centred sheet, and a way out.
//!
//! Every dialog in the application is drawn by [`panel`] so that all of them
//! agree on the three ways out a person tries without being told — the close
//! glyph in the corner, a press on the ground outside, and Escape. The first
//! two are here; the Escape half is wired in the application's key handling,
//! against the same message.
//!
//! Generic over the message type because the presenter window and the layout
//! editor speak different vocabularies and are entitled to the same dialog.

use iced::widget::{button, container, mouse_area, opaque, scrollable, stack};
use iced::{Alignment, Element, Length};

use crate::theme;

/// A panel is the size the rest of the chrome says a dialog is.
const WIDTH: f32 = theme::controls::DIALOG_MAX_WIDTH;
const PADDING: f32 = theme::controls::DIALOG_PADDING;

/// Draw `body` as a modal panel.
///
/// `dismiss` is what all three ways out mean. `None` is for a question that
/// must be answered — restoring a session, or abandoning unsaved work — where
/// a stray press outside must not count as an answer. Such a panel gets no
/// close glyph either, for the same reason: an affordance that is offered is
/// one that will be used.
pub fn panel<'a, Message: Clone + 'a>(
    body: impl Into<Element<'a, Message>>,
    dismiss: Option<Message>,
) -> Element<'a, Message> {
    let mut layers: Vec<Element<'a, Message>> = vec![body.into()];
    if let Some(dismiss) = dismiss.clone() {
        // Top-right, over the panel's own padding rather than in the flow, so
        // a dialog's first line stays its title instead of a row of empty
        // space holding one glyph.
        layers.push(
            container(
                // The same glyph size every other dismissal in the application
                // uses, rather than a number of this dialog frame's own.
                button(theme::icon::icon(
                    theme::Icon::Close,
                    theme::type_scale::BODY,
                ))
                .padding(theme::space::XS)
                .style(theme::ambient::tool_button)
                .on_press(dismiss),
            )
            .width(Length::Fill)
            .align_x(Alignment::End)
            .into(),
        );
    }

    // `opaque` stops a press inside the sheet from reaching the ground behind
    // it. Without it, using a control would also close the panel it is in —
    // and doing it this way needs no no-op message from the caller.
    let sheet = opaque(
        container(scrollable(stack(layers)).style(theme::ambient::scrollbar))
            .padding(PADDING)
            .width(Length::Fill)
            .max_width(WIDTH)
            .style(theme::ambient::dialog),
    );

    let ground = container(sheet)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .padding(theme::space::M)
        .style(theme::ambient::scrim);

    match dismiss {
        Some(dismiss) => mouse_area(ground).on_press(dismiss).into(),
        None => ground.into(),
    }
}
