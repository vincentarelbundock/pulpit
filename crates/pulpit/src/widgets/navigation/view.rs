//! Drawing the navigation controls.
//!
//! In preview and editing the same controls are drawn *inert*: buttons take
//! no press, and the slider reports what the pointer does as
//! `WidgetEvent::Ignored` rather than as a move. Inert is about what a
//! control *does*, never about whether it can be interacted with — Iced
//! decides that, and a widget that assumes otherwise panics the first time
//! someone drags it.

use iced::widget::{button, column, container, responsive, row, slider, text, Row};
use iced::{Element, Length, Padding, Size};

use crate::theme;
use crate::theme::target;
use crate::theme::Icon;
use crate::widgets::common::view::fitted_size;
use crate::widgets::context::{Mode, SlideData};
use crate::widgets::event::WidgetEvent;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetKind};

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let slides = &ctx.context.slides;
    let mode = ctx.context.mode;
    let on = ctx.on_event;
    let scale = ctx.scale;
    match widget.kind() {
        WidgetKind::SlideButtons => {
            let options = widget.buttons();
            responsive(move |area| {
                // On its own the pair fills whatever pane it is given.
                let height = (area.height * 0.9 * scale)
                    .clamp(target::MINIMUM, 220.0)
                    .min(area.height.max(target::MINIMUM));
                buttons(options, area.width, height, mode, on)
            })
            .into()
        }
        WidgetKind::SlideSlider => scrubber(slides, mode, on),
        WidgetKind::SlideCounter => counter(slides, scale, mode, on),
        WidgetKind::PauseResume => {
            action("Pause / Resume", WidgetEvent::ToggleTimer, mode, on, false)
        }
        WidgetKind::EndPresentation => action(
            "End Presentation",
            WidgetEvent::EndPresentation,
            mode,
            on,
            true,
        ),
        other => crate::widgets::common::view::misdirected(other),
    }
}

/// Back and forward: equal halves of one control, labels measured to fit the
/// button they sit in so they can never wrap onto a second line.
fn buttons<Message: Clone + 'static>(
    options: crate::widgets::navigation::model::ButtonsOptions,
    width: f32,
    height: f32,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let halves = f32::from(u8::from(options.back) + u8::from(options.forward)).max(1.0);
    let button_width = ((width - theme::space::S) / halves - 20.0).max(1.0);
    let longest = if options.labels { "Forward" } else { "" };
    let label_size = fitted_size(
        Size::new(button_width, height),
        longest.chars().count(),
        0.42,
    )
    .min(44.0);
    // Below this the words are unreadable anyway, so the buttons keep their
    // arrows and drop the words rather than wrapping.
    let show_words = options.labels && label_size >= 12.0;
    let label_size = label_size.max(11.0);

    let mut row = Row::new().spacing(theme::space::S).width(Length::Fill);
    if options.back {
        let label = text("Back")
            .size(label_size)
            .wrapping(iced::widget::text::Wrapping::None);
        let content = if show_words {
            row![theme::icon::icon(Icon::ChevronLeft, label_size), label].spacing(theme::space::XS)
        } else {
            row![theme::icon::icon(Icon::ChevronLeft, label_size)]
        };
        let mut back = button(container(content).center(Length::Fill))
            .height(Length::Fixed(height))
            .padding(Padding::from([0.0, 8.0]))
            .width(Length::FillPortion(1))
            .style(theme::controls::filled_tonal(theme::ambient::palette()));
        if mode.interactive() {
            back = back.on_press(on(WidgetEvent::Previous));
        }
        row = row.push(back);
    }
    if options.forward {
        let label = text("Forward")
            .size(label_size)
            .wrapping(iced::widget::text::Wrapping::None);
        let content = if show_words {
            row![label, theme::icon::icon(Icon::ChevronRight, label_size)].spacing(theme::space::XS)
        } else {
            row![theme::icon::icon(Icon::ChevronRight, label_size)]
        };
        let mut forward = button(container(content).center(Length::Fill))
            .height(Length::Fixed(height))
            .padding(Padding::from([0.0, 8.0]))
            .width(Length::FillPortion(1))
            .style(theme::controls::filled_tonal(theme::ambient::palette()));
        if mode.interactive() {
            forward = forward.on_press(on(WidgetEvent::Next));
        }
        row = row.push(forward);
    }
    row.into()
}

/// The slider, with the position beside it.
///
/// The numbers are always on the right and always present: a scrubber whose
/// reading disappears in a narrow pane leaves the presenter dragging blind.
/// Instead the pair share the width — the reading takes what "12 / 40" needs
/// at a size proportional to the pane, and the track takes the rest.
fn scrubber<Message: Clone + 'static>(
    slides: &SlideData<'_>,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    // The reading follows the *preview*, which is what the slider moves: the
    // number under your finger has to change as you drag, not when you let go.
    let label = slides.label(slides.preview);
    let count = slides.count;
    let preview = slides.preview;

    responsive(move |area| {
        let characters = label.chars().count().max(3);
        // A quarter of the pane at most: past that the numbers would be
        // reading matter rather than a readout, and the track is the control.
        let reading_width = ((characters as f32 * 11.0).max(46.0)).min(area.width * 0.28);
        let size = fitted_size(
            Size::new(reading_width, area.height),
            characters,
            SLIDER_READING_HEIGHT,
        )
        .clamp(10.0, 30.0);

        row![
            container(scrubber_from(count, preview, mode, on))
                .width(Length::Fill)
                .center_y(Length::Fill),
            button(
                container(
                    text(label.clone())
                        .size(size)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .color(theme::ambient::text()),
                )
                .width(Length::Fill)
                .align_x(iced::alignment::Horizontal::Right)
                .center_y(Length::Fill),
            )
            .width(Length::Fixed(reading_width))
            .height(Length::Fill)
            .padding(0)
            .style(theme::ambient::tool_button)
            .on_press_maybe(mode.interactive().then(|| on(WidgetEvent::ShowOverview))),
        ]
        .spacing(theme::space::S)
        .align_y(iced::Alignment::Center)
        .into()
    })
    .into()
}

/// How much of the pane's height the position reading may take.
const SLIDER_READING_HEIGHT: f32 = 0.55;

/// The same slider, from values copied out of the facet — which is what a
/// `responsive` closure needs, since it outlives this call.
fn scrubber_from<Message: Clone + 'static>(
    count: usize,
    preview: usize,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let maximum = count.saturating_sub(1).max(1) as u32;
    // This is a controlled slider. During a scrub the audience stays on
    // `current`, while `preview` follows the pointer until release commits
    // it. Binding the thumb to `current` made Iced snap it back after every
    // movement, which looked as though dragging the handle did not work.
    let value = (preview as u32).min(maximum);
    let control = slider(0..=maximum, value, move |slide| {
        on(scrub_event(mode, slide as usize))
    })
    .width(Length::Fill)
    // Iced's default track is sixteen pixels tall, which is under half the
    // minimum pointer target and far too thin to hit with a finger while
    // standing at a lectern.
    .height(target::MINIMUM)
    .step(1u32);

    if mode.interactive() {
        control.on_release(on(WidgetEvent::CommitScrub)).into()
    } else {
        control.into()
    }
}

/// What a movement of the slider means in this mode.
///
/// An inert slider is inert in the *drawing*, not to the pointer: Iced sends
/// it a message the moment the handle moves, whatever mode it was built in.
/// Mapping that message away with `unreachable!` panicked the first time
/// anyone dragged the sample slider in the editor. A control that cannot act
/// still needs something to say, so it says nothing, out loud.
fn scrub_event(mode: Mode, slide: usize) -> WidgetEvent {
    if mode.interactive() {
        WidgetEvent::ScrubTo(slide)
    } else {
        WidgetEvent::Ignored
    }
}

fn counter<Message: Clone + 'static>(
    slides: &SlideData<'_>,
    scale: f32,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let label = slides.label(slides.current);
    responsive(move |area| {
        // "12 / 40" fills its pane like any other reading.
        let size = fitted_size(area, label.chars().count(), 0.5) * scale;
        button(
            column![
                text("SLIDE")
                    .size((size * 0.34).clamp(9.0, 14.0))
                    .color(theme::ambient::muted()),
                text(label.clone())
                    .font(theme::font::READOUT)
                    .size(size)
                    .color(theme::ambient::text()),
            ]
            .spacing(2)
            .align_x(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(0)
        .style(theme::ambient::tool_button)
        .on_press_maybe(mode.interactive().then(|| on(WidgetEvent::ShowOverview)))
        .into()
    })
    .into()
}

fn action<Message: Clone + 'static>(
    label: &'static str,
    event: WidgetEvent,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
    alert: bool,
) -> Element<'static, Message> {
    let mut control = button(
        text(label)
            .size(theme::type_scale::BODY)
            .center()
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding(theme::space::M)
    .style(if alert {
        theme::ambient::alert_button
    } else {
        theme::ambient::tool_button
    });
    if mode.interactive() {
        control = control.on_press(on(event));
    }
    container(control)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dragging a slider in the editor used to panic.
    ///
    /// The inert slider was built by mapping its message away with
    /// `unreachable!`, which is true of the drawing and false of the
    /// pointer.
    #[test]
    fn an_inert_slider_has_something_harmless_to_say() {
        assert_eq!(scrub_event(Mode::Live, 7), WidgetEvent::ScrubTo(7));
        for mode in [Mode::Preview, Mode::Editing] {
            assert_eq!(
                scrub_event(mode, 7),
                WidgetEvent::Ignored,
                "{mode:?}: dragging an inert slider must produce a message, and
                 that message must do nothing"
            );
        }
    }

    #[test]
    fn every_mode_builds_a_slider() {
        for mode in [Mode::Live, Mode::Preview, Mode::Editing] {
            let _: Element<'static, WidgetEvent> = scrubber_from(10, 3, mode, |event| event);
        }
    }
}
