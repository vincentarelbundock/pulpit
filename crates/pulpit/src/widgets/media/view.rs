//! Drawing the media transport.
//!
//! Presenter-only by construction: this is a layout widget, and the layout is
//! the presenter screen. Nothing here can reach the audience window, which is
//! the entire reason the controls are drawn on this side instead of inside
//! the page the two views share.

use iced::widget::{button, container, responsive, row, slider, text};
use iced::{Element, Length, Padding};

use crate::theme;
use crate::theme::Icon;
use crate::theme::{target, type_scale};
use crate::widgets::event::{TransportRequest, WidgetEvent};
use crate::widgets::media::model::Transport;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetKind};

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let media = &ctx.context.media;
    let mode = ctx.context.mode;
    let on = ctx.on_event;
    let scale = ctx.scale;
    if widget.kind() != WidgetKind::MediaTransport {
        return crate::widgets::common::view::misdirected(widget.kind());
    }
    let Some(transport) = media.transport.clone() else {
        return idle(scale);
    };
    // In the editor the controls are drawn but do nothing, like every other
    // control there. `interactive` is about the mode; `enabled` is about
    // whether the media itself can answer.
    let live = mode.interactive() && transport.enabled;
    transport_row(transport, live, on, scale)
}

/// A slide with no media at all. The pane still says what it is for, rather
/// than going blank and reading as a rendering fault.
fn idle<Message: 'static>(scale: f32) -> Element<'static, Message> {
    container(
        text("No media on this slide")
            .size(type_scale::BODY * scale)
            .color(theme::ambient::muted()),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center(Length::Fill)
    .into()
}

fn transport_row<Message: Clone + 'static>(
    transport: Transport,
    live: bool,
    on: fn(WidgetEvent) -> Message,
    scale: f32,
) -> Element<'static, Message> {
    responsive(move |area| {
        let height = (area.height * 0.8)
            .clamp(target::MINIMUM, 56.0)
            .min(area.height.max(target::MINIMUM));

        let action = transport.action;
        let mut play = button(
            container(theme::icon::icon(
                action.icon(),
                (height * 0.42).clamp(12.0, 22.0),
            ))
            .center(Length::Fill),
        )
        .width(Length::Fixed(height))
        .height(Length::Fixed(height))
        .padding(0)
        .style(theme::ambient::forward_button);
        if live {
            play = play.on_press(on(WidgetEvent::Transport(match action {
                crate::widgets::media::model::Action::Play => TransportRequest::Play,
                crate::widgets::media::model::Action::Pause => TransportRequest::Pause,
            })));
        }

        let readout = text(transport.readout.clone())
            .size((type_scale::LABEL * scale).clamp(10.0, 20.0))
            .color(if transport.enabled {
                theme::ambient::text()
            } else {
                theme::ambient::muted()
            })
            .wrapping(iced::widget::text::Wrapping::None);

        let mut controls = row![play]
            .spacing(theme::space::M)
            .align_y(iced::Alignment::Center);

        // The scrub bar takes whatever the row does not need. Without a
        // duration there is nothing to scrub, so the readout simply moves
        // left rather than a dead track being drawn.
        if transport.scrubbable {
            let duration = transport.duration.unwrap_or(0.0);
            let mut track = slider(0.0..=duration, transport.position, move |seconds| {
                on(WidgetEvent::Transport(TransportRequest::SeekTo(seconds)))
            })
            .step(0.05_f32)
            .height(height * 0.5)
            .width(Length::Fill);
            if !live {
                // Inert, not absent: the presenter can still see the playhead.
                track = slider(0.0..=duration, transport.position, move |_| {
                    on(WidgetEvent::Ignored)
                })
                .step(0.05_f32)
                .height(height * 0.5)
                .width(Length::Fill);
            }
            controls = controls.push(track);
        } else {
            controls = controls.push(iced::widget::space::horizontal());
        }

        controls = controls.push(readout);

        if transport.mutable {
            let muted = transport.muted;
            let sound_icon = if muted { Icon::Mute } else { Icon::Volume };
            let mut sound = button(
                container(theme::icon::icon(
                    sound_icon,
                    (height * 0.36).clamp(11.0, 18.0),
                ))
                .center(Length::Fill),
            )
            .width(Length::Fixed(height))
            .height(Length::Fixed(height))
            .padding(0)
            .style(theme::ambient::back_button);
            if live {
                sound = sound.on_press(on(WidgetEvent::Transport(TransportRequest::SetMuted(
                    !muted,
                ))));
            }
            controls = controls.push(sound);
        }

        // Project the media across the whole slide area, audience and
        // presenter together. Drawn as pressed-in while active, because the
        // button is the state's most visible record on the presenter side.
        let fullscreen = transport.fullscreen;
        let mut project = button(
            container(theme::icon::icon(
                Icon::Maximize,
                (height * 0.36).clamp(11.0, 18.0),
            ))
            .center(Length::Fill),
        )
        .width(Length::Fixed(height))
        .height(Length::Fixed(height))
        .padding(0)
        .style(if fullscreen {
            theme::ambient::forward_button
        } else {
            theme::ambient::back_button
        });
        if live {
            project = project.on_press(on(WidgetEvent::Transport(
                TransportRequest::SetFullscreen(!fullscreen),
            )));
        }
        controls = controls.push(project);

        container(controls)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::from([0.0, 2.0]))
            .center_y(Length::Fill)
            .into()
    })
    .into()
}
