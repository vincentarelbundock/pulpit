//! Drawing speaker notes.

use iced::widget::{column, container, image, scrollable, text};
use iced::{ContentFit, Element, Length};

use pulpit_render::cache::FrameKind;

use crate::theme;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::Widget;

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let slides = &ctx.context.slides;
    let document = &ctx.context.document;
    let mode = ctx.context.mode;
    let options = widget.notes();
    // Notes follow the *preview* slide, not the committed one: reading ahead
    // is the whole point of the pane.
    let slide = options.source.slide(slides.preview);

    // Text first. A deck that carries its notes as text has said what they
    // are; cropping a region of a page is the fallback for decks that only
    // draw them.
    let written = slides
        .text_notes
        .and_then(|notes| notes.for_slide(slide))
        .map(str::to_string);

    let body: Element<'static, Message> = match written {
        Some(written) => scrollable(
            text(written)
                .size(options.font_size)
                .line_height(options.line_spacing)
                .color(theme::ambient::text()),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::ambient::scrollbar)
        .into(),
        None => match slides
            .frames
            .frame(slide, FrameKind::Notes, slides.preview_width)
        {
            Some(handle) => image(handle)
                .content_fit(ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None if mode.is_sample() => text(document.sample_notes.to_string())
                .size(options.font_size)
                .line_height(options.line_spacing)
                .color(theme::ambient::text())
                .into(),
            None => text(
                "No notes for this slide. Choose a notes mapping in the presenter window if your \
                 deck carries them.",
            )
            .size(options.font_size * 0.85)
            .line_height(options.line_spacing)
            .color(theme::ambient::muted())
            .into(),
        },
    };

    column![
        text("Speaker Notes")
            .size(theme::type_scale::LABEL)
            .color(theme::ambient::muted()),
        container(body).width(Length::Fill).height(Length::Fill),
    ]
    .spacing(theme::space::XS)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
