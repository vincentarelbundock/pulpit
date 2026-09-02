//! Drawing the menu button and the audience lifecycle controls.
//!
//! Both are drawn inert in the editor, like every other family: the controls
//! are there to be positioned and sized, and pressing one in the editor must
//! not open a projector window.

use iced::widget::{button, container, row, text, Row};
use iced::{Alignment, Element, Length};

use crate::theme;
use crate::theme::type_scale;
use crate::widgets::context::Mode;
use crate::widgets::event::{ChromeCommand, WidgetEvent};
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetKind};

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let audience = &ctx.context.audience;
    let mode = ctx.context.mode;
    let on = ctx.on_event;
    match widget.kind() {
        WidgetKind::MainMenu => menu_button(audience.menu_open, mode, on),
        WidgetKind::AudienceControls => lifecycle(audience.started, mode, on),
        other => crate::widgets::common::view::misdirected(other),
    }
}

/// One always-available entry point, and the way back out of it.
fn menu_button<Message: Clone + 'static>(
    open: bool,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let palette = theme::ambient::palette();
    let glyph = if open {
        theme::Icon::Close
    } else {
        theme::Icon::Menu
    };
    // When the menu lives in a layout band, it is one toolbar button among
    // the document controls beside it. Give it their glyph size and inset so
    // it does not make the whole row taller or look stranded in a large box.
    let mut control = button(theme::icon::icon(glyph, theme::type_scale::HEADING))
        .padding(theme::controls::TOOLBAR_BUTTON)
        .style(theme::controls::selectable(palette, open));
    if mode.interactive() {
        control = control.on_press(on(WidgetEvent::Chrome(ChromeCommand::ToggleMenu)));
    }
    container(control)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// A conventional split Start button: the broad side performs the default,
/// while the arrow exposes deliberate placement variants. Stop stands beside
/// it rather than replacing it, so neither ever moves under the hand.
fn lifecycle<Message: Clone + 'static>(
    started: bool,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    container(
        lifecycle_row(started, mode.interactive(), on)
            .spacing(theme::space::XS)
            .align_y(Alignment::Center),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}

/// Start, its placement arrow, and Stop — the buttons themselves, without the
/// container that positions them.
///
/// The presenter toolbar draws the same three controls when the layout has
/// not placed this widget, and pads them into a strip instead of centring
/// them in a cell. Only that wrapper differs, so only that wrapper is
/// written twice.
pub(crate) fn lifecycle_row<Message: Clone + 'static>(
    started: bool,
    live: bool,
    on: fn(WidgetEvent) -> Message,
) -> Row<'static, Message> {
    const CONTROL_WIDTH: f32 = 112.0;
    const START_LABEL_WIDTH: f32 = CONTROL_WIDTH - theme::controls::BUTTON_HEIGHT;
    // Larger than the usual label so the words carry the button, while still
    // leaving a margin of empty space inside it.
    const LIFECYCLE_LABEL: f32 = 16.0;
    let palette = theme::ambient::palette();

    // Once the audience is running the arrow does nothing, so the control
    // becomes one undivided button and its label sits in the middle of it
    // rather than in the middle of the narrower left half.
    let mut start = button(
        text(if started { "Started" } else { "Start" })
            .size(LIFECYCLE_LABEL)
            .center(),
    )
    .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
    .width(Length::Fixed(if started {
        CONTROL_WIDTH
    } else {
        START_LABEL_WIDTH
    }))
    .style(move |base, status| {
        if started {
            theme::controls::filled_tonal(palette)(base, status)
        } else {
            theme::controls::split_left(palette)(base, status)
        }
    });
    if live {
        start = start.on_press(on(WidgetEvent::Chrome(ChromeCommand::StartAudience)));
    }

    let mut stop = button(text("Stop").size(LIFECYCLE_LABEL).center())
        .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
        .width(Length::Fixed(CONTROL_WIDTH))
        .style(theme::controls::filled_tonal(palette));
    if live {
        stop = stop.on_press(on(WidgetEvent::Chrome(ChromeCommand::StopAudience)));
    }

    let start_dropdown = if started {
        row![start]
    } else {
        let mut arrow = button(theme::icon::icon(
            theme::Icon::ChevronDown,
            type_scale::BODY,
        ))
        .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
        .width(Length::Fixed(theme::controls::BUTTON_HEIGHT))
        .style(theme::controls::split_right(palette));
        if live {
            arrow = arrow.on_press(on(WidgetEvent::Chrome(ChromeCommand::ToggleStartMenu)));
        }
        row![start, arrow]
    }
    .spacing(0);

    row![start_dropdown, stop]
}
