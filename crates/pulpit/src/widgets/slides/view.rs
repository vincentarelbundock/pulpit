//! Drawing slide previews.

use iced::widget::{column, container, image, mouse_area, responsive, row, text};
use iced::{ContentFit, Element, Length, Size};

use pulpit_render::cache::FrameKind;

use crate::theme;
use crate::widgets::context::{Mode, SlideData};
use crate::widgets::event::WidgetEvent;
use crate::widgets::slides::model::{Relative, SlideFit};
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetKind};

/// One slide pane, or the three-across strip.
pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let slides = &ctx.context.slides;
    let mode = ctx.context.mode;
    let on = ctx.on_event;
    match widget.kind() {
        WidgetKind::CurrentSlide => panel(widget, slides, mode, Relative::Current, on),
        WidgetKind::PreviousSlide => panel(widget, slides, mode, Relative::Previous, on),
        WidgetKind::NextSlide => panel(widget, slides, mode, Relative::Next, on),
        WidgetKind::PreviousCurrentNext => strip(widget, slides, mode, on),
        other => crate::widgets::common::view::misdirected(other),
    }
}

/// The current slide is substantially larger and sits in the middle: where
/// did I come from, what is showing, what is next.
fn strip<Message: Clone + 'static>(
    widget: &Widget,
    slides: &SlideData<'_>,
    mode: Mode,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    row![
        container(panel(widget, slides, mode, Relative::Previous, on))
            .width(Length::FillPortion(27)),
        container(panel(widget, slides, mode, Relative::Current, on))
            .width(Length::FillPortion(46)),
        container(panel(widget, slides, mode, Relative::Next, on)).width(Length::FillPortion(27)),
    ]
    .spacing(theme::space::S)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn panel<Message: Clone + 'static>(
    widget: &Widget,
    slides: &SlideData<'_>,
    mode: Mode,
    which: Relative,
    on: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let current = which == Relative::Current;
    let (slide, exists) = which.index(slides.current, slides.count);
    let available = exists || mode.is_sample();

    // Never `ContentFit::Fill`: a slide is a fixed-aspect document, and
    // stretching it shows the audience something the author did not draw.
    let fit = match widget.slide().fit {
        SlideFit::Fit | SlideFit::Stretch => ContentFit::Contain,
        SlideFit::Fill => ContentFit::Cover,
    };

    let width = if current {
        slides.preview_width * 2
    } else {
        slides.preview_width
    };
    let handle = slides.frames.frame(slide, FrameKind::Slide, width);
    let build_picture =
        move |handle: Option<iced::widget::image::Handle>| -> Element<'static, Message> {
            match handle {
                Some(handle) => image(handle)
                    .content_fit(fit)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                None => container(
                    text(if available {
                        format!("Slide {}", slide + 1)
                    } else {
                        "—".to_string()
                    })
                    .size(theme::type_scale::HEADING)
                    .color(theme::ambient::muted()),
                )
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into(),
            }
        };

    // The live current-slide panel reports pointer positions in slide
    // coordinates and presses, so the application can hit-test the page's
    // link annotations and its overlays. `responsive` supplies the panel size
    // the letterbox mapping needs; everything else stays a plain picture.
    let picture: Element<'static, Message> = if current && mode.interactive() {
        let aspect = slides.aspect;
        let has_links = slides.has_links;
        let overlays = slides.overlays.clone();
        let crop = slides.crop;
        // Reference-counted handles: the closure owns them, the strokes are
        // never copied.
        let annotations = std::sync::Arc::clone(slides.annotations);
        let marks_cache = std::rc::Rc::clone(&slides.marks_cache);
        let annotation_style = slides.annotation_style;
        let rendered_text = std::sync::Arc::clone(slides.rendered_text);
        // The presenter always sees their own marks; the audience window
        // decides for itself (see `crate::view::audience`).
        let armed = annotations.is_armed();
        let highlights = slides.link_highlights.clone();
        responsive(move |size| {
            let composited = annotate(
                composite(
                    build_picture(handle.clone()),
                    &overlays,
                    size,
                    aspect,
                    fit,
                    crop,
                ),
                &annotations,
                &marks_cache,
                annotation_style,
                aspect,
                fit,
                crop,
                &rendered_text,
            );
            // The outline goes on last so it is never hidden by a video or
            // an overlay drawn on top of the link it belongs to.
            let composited = highlight_links(composited, &highlights, size, aspect, fit, crop);
            let area = mouse_area(composited)
                .on_move(move |point| {
                    let (x, y) = to_content(point, size, aspect, fit);
                    on(WidgetEvent::SlideCursor { x, y })
                })
                .on_press(on(WidgetEvent::SlidePressed));
            // An armed tool takes the pointer away from the links, and the
            // cursor says so before the presenter finds out by pressing. The
            // highlighter wears the I-beam rather than the crosshair, because
            // it sweeps the page's own text rather than drawing where the hand
            // goes — the same cursor it has in document mode, for the same
            // reason (§8.1).
            if armed {
                let cursor = match annotations.tool {
                    Some(pulpit_core::annotation::AnnotationTool::Highlighter) => {
                        iced::mouse::Interaction::Text
                    }
                    _ => iced::mouse::Interaction::Crosshair,
                };
                area.interaction(cursor).into()
            } else if has_links {
                area.interaction(iced::mouse::Interaction::Pointer).into()
            } else {
                area.into()
            }
        })
        .into()
    } else if current && !(slides.overlays.is_empty() && slides.annotations.is_empty()) {
        // The preview panel composites too, but takes no input.
        let overlays = slides.overlays.clone();
        let annotations = std::sync::Arc::clone(slides.annotations);
        let marks_cache = std::rc::Rc::clone(&slides.marks_cache);
        let annotation_style = slides.annotation_style;
        let rendered_text = std::sync::Arc::clone(slides.rendered_text);
        let (aspect, crop) = (slides.aspect, slides.crop);
        responsive(move |size| {
            annotate(
                composite(
                    build_picture(handle.clone()),
                    &overlays,
                    size,
                    aspect,
                    fit,
                    crop,
                ),
                &annotations,
                &marks_cache,
                annotation_style,
                aspect,
                fit,
                crop,
                &rendered_text,
            )
        })
        .into()
    } else {
        build_picture(handle)
    };

    // No inner padding: the slide gets the whole cell. The canvas behind it
    // is black in every scheme, so the letterbox bars read as the projector's
    // own margins rather than as app chrome.
    let framed = container(picture)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::ambient::slide_letterbox);

    // The live slide carries no caption: it is the one panel that needs no
    // label, and the space is better spent on the slide itself.
    if current {
        return framed.into();
    }

    let caption = text(format!("{} · {}", which.caption(), slides.label(slide)))
        .size(theme::type_scale::LABEL)
        .color(theme::ambient::muted());

    column![caption, framed]
        .spacing(theme::space::XS)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Draw the overlays over the page, each at its own page rectangle.
///
/// The page stays underneath at all times: an overlay that has produced no
/// frame yet simply is not in this list, so what shows through is the PDF
/// the author drew rather than a hole.
pub fn composite<'a, Message: Clone + 'a>(
    page: Element<'a, Message>,
    overlays: &[crate::widgets::context::SlideOverlay],
    panel: Size,
    aspect: f32,
    fit: ContentFit,
    crop: pulpit_core::notes::Region,
) -> Element<'a, Message> {
    use iced::widget::stack;

    // The page is *always* wrapped, even with nothing to lay over it.
    //
    // Widget state in iced is positional: returning the bare page when the
    // list is empty and a `stack` when it is not makes the picture change
    // depth in the tree, which resets the image widget's state and costs a
    // frame. The lists here fill in asynchronously — link annotations and
    // overlay declarations are fetched per page and arrive after the
    // navigation that asked for them — so the shape would change a fraction
    // of a second *after* each slide appeared. That is the presenter's live
    // panel blinking shortly after a page turn, and the neighbour panels,
    // which have no overlays, links or ink, never doing it.
    let mut layers = stack![page];
    for overlay in overlays {
        let Some(rectangle) = crate::media::place(panel, aspect, fit, crop, overlay.region) else {
            continue;
        };
        if rectangle.width <= 0.0 || rectangle.height <= 0.0 {
            continue;
        }
        // An overlay scrolled out of a zoom is skipped rather than clamped
        // back into view, which would misrepresent where it sits on the page.
        if rectangle.x + rectangle.width <= 0.0
            || rectangle.y + rectangle.height <= 0.0
            || rectangle.x >= panel.width
            || rectangle.y >= panel.height
        {
            continue;
        }
        // Interactive content gets a hairline edge so the presenter can see
        // what will take a click before they aim at it. The audience list is
        // built with `interactive` false, so this never reaches them.
        let surface = container(
            image(overlay.handle.clone())
                .content_fit(ContentFit::Fill)
                .width(Length::Fixed(rectangle.width))
                .height(Length::Fixed(rectangle.height)),
        )
        .style({
            // Copied out, so the style closure borrows nothing from the list.
            let interactive = overlay.interactive;
            move |_: &iced::Theme| {
                if interactive {
                    container::Style {
                        border: iced::Border {
                            color: theme::ambient::accent(),
                            width: 1.0,
                            radius: 2.0.into(),
                        },
                        ..container::Style::default()
                    }
                } else {
                    container::Style::default()
                }
            }
        });
        let placed = container(surface)
            .padding(iced::Padding {
                top: rectangle.y.max(0.0),
                left: rectangle.x.max(0.0),
                right: 0.0,
                bottom: 0.0,
            })
            .width(Length::Fill)
            .height(Length::Fill);
        layers = layers.push(placed);
    }
    layers.into()
}

/// Stack the presenter's marks over whatever has been composited so far.
///
/// Above the media overlays on purpose: a mark is about what is on screen,
/// including a video, and one hidden behind the thing it was pointing at
/// would be worse than useless.
#[allow(clippy::too_many_arguments)]
fn annotate<'a, Message: Clone + 'a>(
    page: Element<'a, Message>,
    annotations: &std::sync::Arc<pulpit_core::annotation::Annotations>,
    cache: &std::rc::Rc<iced::widget::canvas::Cache>,
    style: pulpit_core::annotation::AnnotationStyle,
    aspect: f32,
    fit: ContentFit,
    crop: pulpit_core::notes::Region,
    rendered_text: &std::sync::Arc<
        std::collections::HashMap<u64, crate::typst_annotation::RenderedText>,
    >,
) -> Element<'a, Message> {
    use iced::widget::stack;

    // Always a stack, empty or not: see the note in `composite`. Ink appears
    // and disappears under the presenter's hand, and the picture underneath
    // must not be rebuilt each time it does.
    match crate::widgets::annotations::view::marks(
        annotations,
        cache,
        style,
        aspect,
        fit,
        crop,
        true,
        rendered_text,
    ) {
        Some(marks) => stack![page, marks].into(),
        None => stack![page].into(),
    }
}

/// Outline the links the presenter is pointing at or has focused.
///
/// Presenter-only, and drawn from the same `place` geometry the press uses,
/// so the rectangle shown is exactly the rectangle that will be followed.
fn highlight_links<'a, Message: Clone + 'a>(
    page: Element<'a, Message>,
    highlights: &[crate::widgets::context::LinkHighlight],
    panel: Size,
    aspect: f32,
    fit: ContentFit,
    crop: pulpit_core::notes::Region,
) -> Element<'a, Message> {
    use crate::widgets::context::HighlightReason;
    use iced::widget::stack;

    // Always a stack, empty or not: see the note in `composite`. Highlights
    // come and go with the pointer, and the picture must not be rebuilt when
    // the presenter merely moves the mouse across a link.
    let mut layers = stack![page];
    for highlight in highlights {
        let Some(rectangle) = crate::media::place(panel, aspect, fit, crop, highlight.rect) else {
            continue;
        };
        if rectangle.width <= 0.0 || rectangle.height <= 0.0 {
            continue;
        }
        let focused = highlight.reason == HighlightReason::Focused;
        let outline = container(iced::widget::space::horizontal())
            .width(Length::Fixed(rectangle.width))
            .height(Length::Fixed(rectangle.height))
            .style(move |_| container::Style {
                border: iced::Border {
                    color: theme::ambient::accent(),
                    // The keyboard needs a stronger mark than the pointer:
                    // the pointer is already visible on its own.
                    width: if focused { 2.5 } else { 1.5 },
                    radius: 2.0.into(),
                },
                ..container::Style::default()
            });
        layers = layers.push(
            container(outline)
                .padding(iced::Padding {
                    top: rectangle.y.max(0.0),
                    left: rectangle.x.max(0.0),
                    right: 0.0,
                    bottom: 0.0,
                })
                .width(Length::Fill)
                .height(Length::Fill),
        );
    }
    layers.into()
}

/// Map a pointer position inside the panel onto normalised slide-content
/// coordinates, undoing the aspect-fit letterbox (or the crop, for
/// `ContentFit::Cover`). Values outside `0.0..=1.0` mean the pointer is over
/// the letterbox bars, not the slide.
fn to_content(point: iced::Point, panel: Size, aspect: f32, fit: ContentFit) -> (f32, f32) {
    if panel.width <= 0.0 || panel.height <= 0.0 || aspect <= 0.0 {
        return (-1.0, -1.0);
    }
    let panel_aspect = panel.width / panel.height;
    let wide = panel_aspect > aspect;
    // Contain letterboxes on the wide axis; Cover crops on the narrow one.
    let (drawn_width, drawn_height) = match fit {
        ContentFit::Cover => {
            if wide {
                (panel.width, panel.width / aspect)
            } else {
                (panel.height * aspect, panel.height)
            }
        }
        _ => {
            if wide {
                (panel.height * aspect, panel.height)
            } else {
                (panel.width, panel.width / aspect)
            }
        }
    };
    let offset_x = (panel.width - drawn_width) / 2.0;
    let offset_y = (panel.height - drawn_height) / 2.0;
    (
        (point.x - offset_x) / drawn_width,
        (point.y - offset_y) / drawn_height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_mapping_recovers_slide_coordinates() {
        // A 16:9 slide contained in a 200×200 panel: bars top and bottom,
        // drawn area 200×112.5 starting at y = 43.75.
        let panel = Size::new(200.0, 200.0);
        let aspect = 16.0 / 9.0;

        let (x, y) = to_content(
            iced::Point::new(100.0, 100.0),
            panel,
            aspect,
            ContentFit::Contain,
        );
        assert!((x - 0.5).abs() < 1e-4 && (y - 0.5).abs() < 1e-4);

        let (_, y) = to_content(
            iced::Point::new(100.0, 10.0),
            panel,
            aspect,
            ContentFit::Contain,
        );
        assert!(y < 0.0, "a point on the letterbox bar is outside the slide");

        let (x, y) = to_content(
            iced::Point::new(0.0, 43.75),
            panel,
            aspect,
            ContentFit::Contain,
        );
        assert!(
            x.abs() < 1e-4 && y.abs() < 1e-4,
            "top-left corner of the drawn slide"
        );
    }

    #[test]
    fn cover_mapping_accounts_for_the_crop() {
        // A 16:9 slide covering a 200×200 panel is cropped left and right:
        // drawn width 355.5…, starting off-screen to the left.
        let panel = Size::new(200.0, 200.0);
        let aspect = 16.0 / 9.0;
        let (x, y) = to_content(
            iced::Point::new(100.0, 100.0),
            panel,
            aspect,
            ContentFit::Cover,
        );
        assert!((x - 0.5).abs() < 1e-4 && (y - 0.5).abs() < 1e-4);
        let (x, _) = to_content(
            iced::Point::new(0.0, 100.0),
            panel,
            aspect,
            ContentFit::Cover,
        );
        assert!(
            x > 0.0 && x < 0.5,
            "the panel edge is inside the cropped slide"
        );
    }
}
