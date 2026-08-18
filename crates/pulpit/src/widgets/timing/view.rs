//! Drawing the timer, the clock, and the two together.
//!
//! Both readings size themselves to the pane they are given: a timer in a
//! large cell should be readable from the back of the room, and the same
//! timer in a strip along the bottom should not be clipped. The presenter's
//! scale property remains a multiplier on top of the fitted size, so a
//! deliberate "smaller than it could be" is still possible.

use iced::widget::{button, column, container, responsive, row, text};
use iced::{Element, Length, Size};

use crate::theme;
use crate::widgets::common::view::{fitted_size, format_clock};
use crate::widgets::common::Variant;
use crate::widgets::context::Mode;
use crate::widgets::event::{AlarmCommand, TimerCommand};
use crate::widgets::timing::model::TimerControls;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetEvent, WidgetKind};

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let timing = &ctx.context.timing;
    let alarms = ctx.context.alarms;
    let timer_controls = ctx.context.timer_controls;
    let mode = ctx.context.mode;
    let on = ctx.on_event;
    let scale = ctx.scale;
    // Everything the closure needs, copied out: it outlives this call.
    let kind = widget.kind();
    let style = widget.style;
    let timer_options = widget.timer();
    let clock_options = widget.clock();
    let timing = *timing;
    let timer_controls = timer_controls.clone();
    // The next cue and whether one is going off is all the clock needs; the
    // list itself belongs to the popup.
    let next = alarms.next(timing.seconds_of_day).cloned();
    let ringing = alarms.ringing.clone();
    let snoozed = alarms.snoozed.clone();
    let live = matches!(mode, Mode::Live);

    responsive(move |area| match kind {
        WidgetKind::Timer => {
            let reading =
                timer_options.reading(timing.elapsed, timing.target, timer_controls.count_down);
            let value = format_clock(reading.seconds, true);
            let colour = if reading.overtime || reading.warning {
                theme::ambient::alert()
            } else {
                theme::ambient::text()
            };
            // The reading gives up a strip to the mode line, the way the clock
            // does to its alarms, so the two widgets sit at the same height
            // beside one another.
            let block = reading_block(
                value,
                colour,
                Size::new(area.width, (area.height - FOOTER_ROW).max(1.0)),
                style.variant,
                scale,
            );
            column![
                block,
                timer_row(&timer_controls, reading.overtime, live, on)
            ]
            .spacing(2)
            .align_x(iced::Alignment::Center)
            .into()
        }
        WidgetKind::Clock => {
            // A cue that has gone off colours the whole reading: a clock the
            // presenter is already looking at is the loudest place to say so,
            // and it stays coloured until dismissed rather than flashing past.
            let colour = if ringing.is_some() {
                theme::ambient::alert()
            } else {
                theme::ambient::text()
            };
            let clock = reading_block(
                clock_options.format(timing.seconds_of_day),
                colour,
                // The alarm line takes a slice of the pane, so the reading is
                // fitted to what is left and cannot grow into it.
                if clock_options.show_alarms {
                    Size::new(area.width, (area.height - FOOTER_ROW).max(1.0))
                } else {
                    area
                },
                style.variant,
                scale,
            );
            if !clock_options.show_alarms {
                return clock;
            }
            column![
                clock,
                alarm_row(
                    &clock_options,
                    next.clone(),
                    ringing.clone(),
                    snoozed.clone(),
                    live,
                    on
                )
            ]
            .spacing(2)
            .align_x(iced::Alignment::Center)
            .into()
        }
        other => crate::widgets::common::view::misdirected(other),
    })
    .into()
}

/// Height reserved under a reading for the line of controls beneath it.
///
/// Enough for text at [`FOOTER_TEXT`] with the padding around a button: the
/// line is read and pressed mid-talk, so it is sized to be legible from
/// standing rather than squeezed to leave the digits another point or two.
const FOOTER_ROW: f32 = 30.0;

/// The size of the text in that line.
const FOOTER_TEXT: f32 = theme::type_scale::BODY;

/// The line under the timer: which way it is running, and the way in to
/// change that. It is the clock's alarm line for the other half of the pair.
fn timer_row<Message: Clone + 'static>(
    controls: &TimerControls,
    overtime: bool,
    live: bool,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let caption = if overtime {
        "overtime".to_string()
    } else {
        controls.caption()
    };
    let colour = if overtime {
        theme::ambient::alert()
    } else {
        theme::ambient::muted()
    };
    // Running past the end is answered here, exactly as a cue going off is
    // answered on the clock: the same two words in the same place, because
    // "you are out of time" is the same news whichever half of the pair says
    // it.
    if overtime && !controls.overtime_dismissed && controls.target_seconds.is_some() {
        let answer = |label: String, command: TimerCommand| {
            let mut control = button(text(label).size(FOOTER_TEXT))
                .padding(theme::space::XS)
                .style(theme::ambient::tool_button);
            if live {
                control = control.on_press(on(WidgetEvent::Timer(command)));
            }
            control
        };
        return container(
            row![
                text("overtime").size(FOOTER_TEXT).color(colour),
                answer(
                    format!("+{}m", controls.snooze_minutes),
                    TimerCommand::Snooze
                ),
                answer("Dismiss".to_string(), TimerCommand::Dismiss),
            ]
            .spacing(theme::space::S)
            .align_y(iced::Alignment::Center),
        )
        .center_x(Length::Fill)
        .into();
    }

    let mut control = button(settings_label(caption, colour, live))
        .padding(theme::space::XS)
        .style(theme::ambient::tool_button);
    if live {
        control = control.on_press(on(WidgetEvent::Timer(TimerCommand::Open(true))));
    }
    container(control).center_x(Length::Fill).into()
}

/// A reading that is also a way in to its settings, and says so.
///
/// The gear is the whole of the difference between a caption and a control: a
/// line of text that opens a panel when pressed is indistinguishable from a
/// line of text that does nothing, and a presenter mid-talk will not go
/// hunting. Inert in the editor, where nothing opens, so the designer is not
/// promised a panel it will not get.
fn settings_label<'a, Message: 'a>(
    caption: String,
    colour: iced::Color,
    live: bool,
) -> Element<'a, Message> {
    let label = text(caption).size(FOOTER_TEXT).color(colour);
    if !live {
        return label.into();
    }
    row![
        label,
        theme::icon::tinted(theme::Icon::Gear, FOOTER_TEXT * 0.85, colour)
    ]
    .spacing(theme::space::XS)
    .align_y(iced::Alignment::Center)
    .into()
}

/// The line under the clock: what is coming, and the way in to the popup.
///
/// One press target, always in the same place whether or not a cue is set, so
/// that finding it is muscle memory rather than a search. It is the *only*
/// press target on the clock: making the whole reading clickable would turn
/// every stray press during a talk into an opened dialog.
fn alarm_row<Message: Clone + 'static>(
    options: &crate::widgets::ClockOptions,
    next: Option<crate::widgets::Alarm>,
    ringing: Option<crate::widgets::Alarm>,
    snoozed: Option<crate::widgets::Alarm>,
    live: bool,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    // A cue that is going off is answered here rather than in a dialog: the
    // presenter is looking at the clock already, and a dialog over the slide
    // would take the notes away at the moment they are most needed.
    if let Some(alarm) = &ringing {
        let name = match &alarm.label {
            Some(label) => format!("{} · {label}", options.format_alarm(alarm.at)),
            None => options.format_alarm(alarm.at),
        };
        let answer = |label: &'static str, command: AlarmCommand| {
            let mut control = button(text(label).size(FOOTER_TEXT))
                .padding(theme::space::XS)
                .style(theme::ambient::tool_button);
            if live {
                control = control.on_press(on(WidgetEvent::Alarm(command)));
            }
            control
        };
        return container(
            row![
                text(format!("● {name}"))
                    .size(FOOTER_TEXT)
                    .color(theme::ambient::alert()),
                answer("Snooze", AlarmCommand::Snooze),
                answer("Dismiss", AlarmCommand::Dismiss),
            ]
            .spacing(theme::space::S)
            .align_y(iced::Alignment::Center),
        )
        .center_x(Length::Fill)
        .into();
    }

    // Otherwise the line names what is coming, and is the way in to the list.
    if let Some(alarm) = &snoozed {
        let caption = format!("snoozed · {}", options.format_alarm(alarm.at));
        let mut control = button(settings_label(caption, theme::ambient::muted(), live))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button);
        if live {
            control = control.on_press(on(WidgetEvent::Alarm(AlarmCommand::Open(true))));
        }
        return container(control).center_x(Length::Fill).into();
    }

    let (caption, command, colour) = match (&ringing, &next) {
        (None, Some(alarm)) => (
            match &alarm.label {
                Some(label) => format!("next {} · {label}", options.format_alarm(alarm.at)),
                None => format!("next {}", options.format_alarm(alarm.at)),
            },
            AlarmCommand::Open(true),
            theme::ambient::muted(),
        ),
        _ => (
            "Alarms".to_string(),
            AlarmCommand::Open(true),
            theme::ambient::muted(),
        ),
    };

    // Inert in the editor: the designer draws the same controls without them
    // doing anything, so a layout can be arranged without setting cues.
    let mut control = button(settings_label(caption, colour, live))
        .padding(theme::space::XS)
        .style(theme::ambient::tool_button);
    if live {
        control = control.on_press(on(WidgetEvent::Alarm(command)));
    }
    container(control).center_x(Length::Fill).into()
}

/// A large reading, centred in the space it was given.
///
/// The block fills its share of the pane and centres in it both ways, so a
/// clock in a wide strip sits in the middle of that strip rather than in its
/// top-left corner. There is no caption over it: "CLOCK" over a clock face
/// says nothing the digits do not, and the line *under* each reading already
/// names what it is doing — so the space goes to the digits instead.
fn reading_block<Message: 'static>(
    value: String,
    colour: iced::Color,
    area: Size,
    variant: Variant,
    scale: f32,
) -> Element<'static, Message> {
    // Scale is a preference, not a licence to overflow: a reading may be
    // made smaller than it could be, never larger than fits.
    let fitted = fitted_size(area, value.chars().count(), height_fraction(variant));
    let ceiling = fitted_size(area, value.chars().count(), CEILING_FRACTION);
    let size = (fitted * scale).min(ceiling);

    container(
        text(value)
            .font(theme::font::READOUT)
            .size(size)
            // A tight line box, because this line is digits. The default
            // leaves room over and under for ascenders and descenders that a
            // clock face does not have, and that room was the empty band
            // around the reading: the pane was full while the numbers were
            // not.
            .line_height(iced::widget::text::LineHeight::Relative(1.0))
            .color(colour),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}

/// How much of the pane's height the reading may take.
///
/// This is what the variants now mean: not three fixed sizes, but how boldly
/// the widget fills whatever it was given. With a tight line box these are
/// close to the share of the pane the digits themselves occupy.
fn height_fraction(variant: Variant) -> f32 {
    match variant {
        Variant::Compact => 0.5,
        Variant::Standard => 0.72,
        Variant::Prominent => 0.9,
    }
}

/// The most of its pane a reading may take, whatever scale was asked for.
///
/// Just short of the whole thing: a reading that touched both edges would look
/// like one that had been clipped, even where it had not.
const CEILING_FRACTION: f32 = 0.95;
