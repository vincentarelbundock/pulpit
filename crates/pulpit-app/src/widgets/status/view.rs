//! Drawing the title, the section, and the audience status lines.

use iced::widget::{column, container, responsive, row, space, text};
use iced::{Alignment, Element, Length};

use crate::theme;
use crate::widgets::common::view::{fitted_size, labelled, status_line};
use crate::widgets::context::{AudienceData, DocumentData, SlideData};
use crate::widgets::status::model::{audience_reading, connection_reading, StatusIntent};
use crate::widgets::{Widget, WidgetKind};

pub fn view<Message: 'static>(
    widget: &Widget,
    document: &DocumentData<'_>,
    slides: &SlideData<'_>,
    audience: &AudienceData,
    scale: f32,
    accent: iced::Color,
) -> Element<'static, Message> {
    match widget.kind() {
        WidgetKind::PresentationTitle => title(document, slides, scale),
        WidgetKind::CurrentSection => labelled(
            "Section",
            document.section.clone().unwrap_or_else(|| "—".to_string()),
            scale,
            accent,
            widget.style.alignment,
        ),
        WidgetKind::AudienceScreenStatus => {
            // Nothing is said about the ordinary case. Presenting into a
            // window on one screen is a normal way to work; blanking is what
            // is worth interrupting for.
            match audience_reading(audience.blank, audience.fullscreen) {
                Some(reading) => {
                    status_line("Audience", reading.text, colour(reading.intent), scale)
                }
                None => space::horizontal().into(),
            }
        }
        WidgetKind::ConnectionStatus => {
            let reading = connection_reading(audience.connected);
            status_line("Connection", reading.text, colour(reading.intent), scale)
        }
        other => crate::widgets::common::view::misdirected(other),
    }
}

fn colour(intent: StatusIntent) -> iced::Color {
    match intent {
        StatusIntent::Good => theme::ambient::accent(),
        StatusIntent::Warning => theme::ambient::alert(),
    }
}

fn title<Message: 'static>(
    document: &DocumentData<'_>,
    slides: &SlideData<'_>,
    scale: f32,
) -> Element<'static, Message> {
    let name = document.title.clone();
    let count = slides.count;
    responsive(move |area| {
        // The title fills its strip: it is the first thing anyone looks at on
        // a shared machine, and a fixed size wastes a wide header.
        let size = fitted_size(area, name.chars().count().max(12), 0.5) * scale;
        // A slim accent line beside the title, as in the controller design.
        row![
            container(
                space::horizontal()
                    .width(Length::Fixed(3.0))
                    .height(Length::Fill)
            )
            .style(theme::ambient::accent_rule)
            .height(Length::Fill),
            column![
                text(name.clone()).size(size).color(theme::ambient::text()),
                text(if count == 0 {
                    "No document".to_string()
                } else {
                    format!("{count} slides")
                })
                .size((size * 0.5).clamp(10.0, 18.0))
                .color(theme::ambient::muted()),
            ]
            .spacing(2),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .height(Length::Fill)
        .into()
    })
    .into()
}
