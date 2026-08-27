//! Small visual primitives every family draws with.

use iced::widget::{button, column, container, responsive, row, space, text};
use iced::{Alignment, Element, Length};

use crate::theme;
use crate::widgets::common::Align;

/// The glyph for what a select band takes, wherever the choice is offered:
/// the lasso that gathers marks up, the picture, and text in a dashed frame.
///
/// One mapping for both toolbars, so the presenter palette and the Reader
/// cannot come to draw the same choice with different pictures.
/// The glyph for one of the highlighter's three marks.
pub fn markup_kind_glyph(kind: pulpit_core::annotation::MarkupKind) -> theme::Icon {
    match kind {
        pulpit_core::annotation::MarkupKind::Highlight => theme::Icon::Highlighter,
        pulpit_core::annotation::MarkupKind::Underline => theme::Icon::Underline,
        pulpit_core::annotation::MarkupKind::StrikeOut => theme::Icon::StrikeOut,
    }
}

pub fn shape_kind_glyph(kind: pulpit_core::annotation::ShapeKind) -> theme::Icon {
    match kind {
        pulpit_core::annotation::ShapeKind::Rectangle => theme::Icon::Rectangle,
        pulpit_core::annotation::ShapeKind::Ellipse => theme::Icon::Ellipse,
        pulpit_core::annotation::ShapeKind::Line => theme::Icon::Line,
        pulpit_core::annotation::ShapeKind::Arrow => theme::Icon::Arrow,
    }
}

pub fn stamp_mark_glyph(mark: pulpit_core::annotation::StampChoice) -> theme::Icon {
    match mark {
        pulpit_core::annotation::StampChoice::Check => theme::Icon::Check,
        // The dismiss glyph, which is the cross this application already
        // draws: two marks that look different would read as two marks.
        pulpit_core::annotation::StampChoice::Cross => theme::Icon::Close,
    }
}

pub fn select_kind_glyph(kind: pulpit_core::annotation::SelectKind) -> theme::Icon {
    match kind {
        pulpit_core::annotation::SelectKind::Marks => theme::Icon::Lasso,
        pulpit_core::annotation::SelectKind::Image => theme::Icon::Picture,
        pulpit_core::annotation::SelectKind::Text => theme::Icon::TextRegion,
    }
}

/// The title-and-close row atop a floating panel.
///
/// An options panel and an overflow menu are different questions — "what
/// does this tool draw" against "what did not fit on the row" — but both are
/// a popover the presenter opens mid-talk and must be able to leave without
/// hunting for the way out. Closing over `on_close` rather than always
/// wiring a press keeps that exit inert instead of absent when the surface
/// it would dismiss is not actually live, so the row does not change shape
/// with the mode.
pub fn panel_header<Message: Clone + 'static>(
    title: impl Into<String>,
    on_close: Option<Message>,
) -> Element<'static, Message> {
    row![
        text(title.into()).size(theme::type_scale::LABEL),
        space::horizontal().width(Length::Fill),
        button(theme::icon::icon(
            theme::Icon::Close,
            theme::type_scale::BODY
        ))
        .padding(2)
        .style(theme::ambient::tool_button)
        .on_press_maybe(on_close),
    ]
    .align_y(Alignment::Center)
    .into()
}

/// A caption above a value, the shape most information widgets take.
///
/// Sized to the pane, like every other reading.
pub fn labelled<Message: 'static>(
    label: &'static str,
    value: String,
    scale: f32,
    align: Align,
) -> Element<'static, Message> {
    responsive(move |area| {
        let size = fitted_size(area, value.chars().count().max(8), 0.42) * scale;
        container(
            column![
                text(label.to_uppercase())
                    .size((size * 0.5).clamp(9.0, 14.0))
                    .color(theme::ambient::muted()),
                text(value.clone()).size(size).color(theme::ambient::text()),
            ]
            .spacing(2)
            .align_x(alignment(align)),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    })
    .into()
}

/// A caption, a dot and a phrase. Status is never colour alone: the dot is a
/// shape as well, and the words say the same thing.
pub fn status_line<Message: 'static>(
    label: &'static str,
    value: &'static str,
    colour: iced::Color,
    scale: f32,
) -> Element<'static, Message> {
    responsive(move |area| {
        let size = fitted_size(area, value.chars().count(), 0.34) * scale;
        let body = column![
            text(label.to_uppercase())
                .size((size * 0.7).clamp(9.0, 14.0))
                .color(theme::ambient::muted()),
            row![
                container(
                    space::horizontal()
                        .width(Length::Fixed(8.0))
                        .height(Length::Fixed(8.0))
                )
                .style(move |_: &iced::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(colour)),
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..iced::Border::default()
                    },
                    ..Default::default()
                }),
                text(value).size(size).color(colour),
            ]
            .spacing(theme::space::S)
            .align_y(Alignment::Center),
        ]
        .spacing(theme::space::XS)
        .align_x(Alignment::Center);

        container(body)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    })
    .into()
}

pub fn alignment(align: Align) -> Alignment {
    match align {
        Align::Start => Alignment::Start,
        Align::Center => Alignment::Center,
        Align::End => Alignment::End,
    }
}

/// `H:MM:SS`, or `M:SS` under an hour.
pub fn format_clock(seconds: i64, allow_short: bool) -> String {
    let seconds = seconds.max(0);
    let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 || !allow_short {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_is_short_under_an_hour_and_long_over_it() {
        assert_eq!(format_clock(0, true), "0:00");
        assert_eq!(format_clock(65, true), "1:05");
        assert_eq!(format_clock(3600, true), "1:00:00");
        assert_eq!(format_clock(65, false), "0:01:05");
        assert_eq!(format_clock(-5, true), "0:00", "never negative");
    }
}

/// The largest text size at which `characters` glyphs still fit the box.
///
/// Widgets are placed in cells whose size the presenter chooses by dragging a
/// divider; a fixed font size means a timer that is tiny in a large pane and
/// clipped in a small one. This is the arithmetic that makes a reading fill
/// the space it was given.
///
/// `height_fraction` is how much of the height the glyphs may take — the rest
/// is the caption above and the breathing room around it. The width estimate
/// assumes the digits and letters of a proportional sans face, which run
/// about 0.58 em wide; being slightly conservative is right, because text
/// that overflows its cell is worse than text a little smaller than it could
/// have been.
pub fn fitted_size(area: iced::Size, characters: usize, height_fraction: f32) -> f32 {
    const EM_WIDTH: f32 = 0.58;
    let by_height = area.height * height_fraction;
    let by_width = if characters == 0 {
        by_height
    } else {
        area.width / (characters as f32 * EM_WIDTH)
    };
    by_height.min(by_width).max(1.0)
}

#[cfg(test)]
mod fit_tests {
    use super::*;
    use iced::Size;

    #[test]
    fn a_reading_grows_with_its_pane() {
        let small = fitted_size(Size::new(200.0, 80.0), 5, 0.6);
        let large = fitted_size(Size::new(400.0, 160.0), 5, 0.6);
        assert!(large > small, "{large} should exceed {small}");
    }

    #[test]
    fn a_wide_short_pane_is_limited_by_its_height() {
        let size = fitted_size(Size::new(2000.0, 60.0), 5, 0.6);
        assert!((size - 36.0).abs() < 0.01, "got {size}");
    }

    #[test]
    fn a_tall_narrow_pane_is_limited_by_its_width() {
        // Eight characters across 200pt: about 43pt each way is impossible,
        // so the width decides.
        let size = fitted_size(Size::new(200.0, 2000.0), 8, 0.6);
        assert!(size < 50.0, "got {size}");
        assert!(size > 30.0, "got {size}");
    }

    #[test]
    fn a_longer_reading_is_set_smaller_in_the_same_box() {
        // Tall enough that the width is what decides.
        let area = Size::new(300.0, 200.0);
        let short = fitted_size(area, 4, 0.6);
        let long = fitted_size(area, 8, 0.6);
        assert!(long < short, "an eight-character clock must shrink to fit");
    }

    #[test]
    fn nothing_ever_asks_for_a_size_of_zero() {
        assert!(fitted_size(Size::new(0.0, 0.0), 5, 0.6) >= 1.0);
    }
}

/// What a family draws when it is handed a widget that is not its own.
///
/// This cannot happen while `Family::of` and the family dispatch agree, and a
/// test builds every kind to keep them agreeing. It is drawn rather than
/// panicked because the one place it could ever appear is on stage, and a
/// pane reading "Clock (not drawn)" is survivable in a way that a crashed
/// presenter screen is not.
pub fn misdirected<Message: 'static>(
    kind: crate::widgets::WidgetKind,
) -> Element<'static, Message> {
    debug_assert!(false, "{kind:?} was handed to the wrong family");
    container(
        text(format!("{} (not drawn)", kind.label()))
            .size(theme::type_scale::LABEL)
            .color(theme::ambient::muted()),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}
