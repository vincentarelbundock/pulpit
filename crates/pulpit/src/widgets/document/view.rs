//! Drawing the reader.
//!
//! Five widgets, one document. The page surface is the only one with any real
//! geometry in it, and even there the arithmetic belongs to
//! [`super::model::Column`]: this file turns already-placed pages into
//! elements, so what is on screen is decided by something that can be tested
//! without a window.

use iced::widget::{
    button, column, container, image, mouse_area, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Padding};

use pulpit_core::annotation::AnnotationTool;
use pulpit_core::page::PageIndex;
use pulpit_render::document::CompatibilityLevel;

use crate::theme;
use crate::widgets::context::{Mode, ReaderData};
use crate::widgets::event::ReadCommand;
use crate::widgets::{Widget, WidgetEvent, WidgetKind};

use super::model::{OutlineView, Zoom};

/// Hand one reader widget its part of the document.
pub fn view<Message: Clone + 'static>(
    widget: &Widget,
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    match widget.kind() {
        WidgetKind::DocumentPage => page_surface(reader, mode, on_event),
        WidgetKind::DocumentNav => navigation(reader, mode, on_event),
        WidgetKind::DocumentOutline => outline(reader, mode, on_event),
        WidgetKind::FormFields => fields(reader, mode, on_event),
        WidgetKind::AnnotationTools => tools(widget, reader, mode, on_event),
        // The dispatcher only routes this family's kinds here; an empty cell
        // is what any other widget would already have drawn.
        _ => blank(),
    }
}

fn blank<Message: 'static>() -> Element<'static, Message> {
    space::horizontal()
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// A line of muted text, centred — what a widget shows when the document has
/// nothing for it.
fn nothing<Message: 'static>(message: &str) -> Element<'static, Message> {
    container(
        text(message.to_string())
            .size(theme::type_scale::LABEL)
            .color(theme::ambient::muted()),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .into()
}

/// The page surface: the pages in the window, stacked on their mount.
///
/// Only the visible pages are built. Everything else in a thousand-page
/// document costs a rectangle in [`super::model::Column`] and nothing here,
/// which is what makes continuous scroll over a long document affordable.
fn page_surface<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    if !reader.open {
        return nothing("No document open.");
    }
    if reader.page_count == 0 {
        return nothing("This document has no pages.");
    }

    let mut sheets = column![].spacing(super::model::PAGE_GAP);
    // A leading spacer stands for every page above the window, so the pages
    // that *are* built land where the column says they do rather than at the
    // top of the cell.
    if let Some(first) = reader.visible.first() {
        if first.placed.top > 0.0 {
            sheets = sheets.push(space::vertical().height(Length::Fixed(first.placed.top)));
        }
    }
    let armed = reader.controls.tool.is_some();
    for page in &reader.visible {
        sheets = sheets.push(sheet(page, mode, armed, on_event));
    }

    let scroller = scrollable(
        container(sheets)
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .padding(Padding::from(super::model::PAGE_GAP)),
    )
    .width(Length::Fill)
    .height(Length::Fill);

    // In the editor the surface is a representation, so it takes no events.
    if !mode.interactive() {
        return scroller.into();
    }
    let _ = on_event;
    scroller.into()
}

/// One page, drawn at the size the column gave it.
///
/// A page with no frame yet is still drawn, at its full size, as a blank
/// sheet: the alternative is a column that changes height as frames arrive,
/// which moves the text under the reader's eye.
fn sheet<Message: Clone + 'static>(
    page: &crate::widgets::context::ReaderPage,
    mode: Mode,
    armed: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let inner: Element<'static, Message> = match &page.frame {
        Some(handle) => image(handle.clone())
            .width(Length::Fixed(page.placed.width))
            .height(Length::Fixed(page.placed.height))
            .into(),
        None => space::horizontal()
            .width(Length::Fixed(page.placed.width))
            .height(Length::Fixed(page.placed.height))
            .into(),
    };
    let sheet = container(inner)
        .width(Length::Fixed(page.placed.width))
        .height(Length::Fixed(page.placed.height))
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::WHITE)),
            ..container::Style::default()
        });

    if !mode.interactive() {
        return sheet.into();
    }

    // Pointer positions leave this widget in canonical page points and in no
    // other unit (A4). The conversion is per sheet because each sheet knows
    // its own page's size and the size it was drawn at, and a document of
    // mixed page sizes has no single scale to convert by.
    let index = page.placed.page;
    let (drawn_width, drawn_height) = (page.placed.width, page.placed.height);
    let (canonical_width, canonical_height) = page.canonical;
    let to_page = move |point: iced::Point| -> (f32, f32) {
        let x = if drawn_width > 0.0 {
            point.x * canonical_width / drawn_width
        } else {
            0.0
        };
        let y = if drawn_height > 0.0 {
            point.y * canonical_height / drawn_height
        } else {
            0.0
        };
        (x, y)
    };

    let area = mouse_area(sheet)
        .on_move(move |point| {
            let (x, y) = to_page(point);
            on_event(WidgetEvent::Read(ReadCommand::PageCursor {
                page: index,
                x,
                y,
            }))
        })
        .on_press(on_event(WidgetEvent::Read(ReadCommand::PagePressed)))
        .on_release(on_event(WidgetEvent::Read(ReadCommand::PageReleased)))
        // The pointer leaving the sheet mid-stroke ends the gesture rather
        // than leaving it open: a stroke that resumed when the pointer came
        // back would join two marks the user made separately.
        .on_exit(on_event(WidgetEvent::Read(ReadCommand::PageCancelled)));

    // An armed tool takes the pointer away from the document's own links and
    // fields, and the cursor says so before the reader finds out by pressing.
    if armed {
        area.interaction(iced::mouse::Interaction::Crosshair).into()
    } else {
        area.into()
    }
}

/// The navigation band: where you are, and how big the page is.
fn navigation<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let live = mode.interactive() && reader.open;
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };

    let step = |icon: theme::Icon, command: ReadCommand, enabled: bool| {
        let control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
            .padding(Padding::from([4.0, 8.0]));
        if live && enabled {
            control.on_press(send(command))
        } else {
            control
        }
    };

    let entry = {
        let value = reader
            .page_entry
            .clone()
            .unwrap_or_else(|| reader.counter());
        let field = text_input("", &value)
            .size(theme::type_scale::LABEL)
            .width(Length::Fixed(96.0));
        if live {
            field
                .on_input(move |typed| send(ReadCommand::TypePage(typed)))
                .on_submit(send(ReadCommand::CommitPage))
        } else {
            field
        }
    };

    let previous = reader.controls.page.get() > 0;
    let next = reader.controls.page.get() + 1 < reader.page_count;

    let zoom_label = text(reader.controls.zoom.label(reader.scale))
        .size(theme::type_scale::LABEL)
        .color(theme::ambient::muted());

    let band = row![
        step(
            theme::Icon::ChevronLeft,
            ReadCommand::GoToPage(PageIndex(reader.controls.page.get().saturating_sub(1))),
            previous
        ),
        entry,
        step(
            theme::Icon::ChevronRight,
            ReadCommand::GoToPage(PageIndex(reader.controls.page.get() + 1)),
            next
        ),
        space::horizontal().width(Length::Fixed(theme::space::M)),
        step(theme::Icon::ZoomOut, ReadCommand::ZoomOut, true),
        zoom_label,
        step(theme::Icon::ZoomIn, ReadCommand::ZoomIn, true),
        step(
            theme::Icon::FitWidth,
            ReadCommand::SetZoom(Zoom::FitWidth),
            true
        ),
        step(
            theme::Icon::FitPage,
            ReadCommand::SetZoom(Zoom::FitPage),
            true
        ),
    ]
    .spacing(theme::space::XS)
    .align_y(Alignment::Center);

    // The compatibility level and the dirty mark are the two things a reader
    // should not have to go looking for: one says what pulpit can honour in
    // this file (§3.4), the other that there is something unsaved in it.
    let mut status = row![].spacing(theme::space::XS).align_y(Alignment::Center);
    if reader.open && reader.level != CompatibilityLevel::Native {
        status = status.push(
            text(reader.level.label().to_string())
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::muted()),
        );
    }
    if reader.dirty {
        status = status.push(
            text("Unsaved changes".to_string())
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::accent()),
        );
    }

    container(
        row![band, space::horizontal().width(Length::Fill), status]
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .align_y(Alignment::Center)
    .into()
}

/// The outline rail: bookmarks, or the pages themselves.
fn outline<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let live = mode.interactive() && reader.open;
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };

    let view = reader.controls.outline;
    let toggle = {
        let control = button(text(view.other().label().to_string()).size(theme::type_scale::LABEL))
            .padding(Padding::from([4.0, 8.0]));
        if live {
            control.on_press(send(ReadCommand::SetOutlineView(view.other())))
        } else {
            control
        }
    };

    let body: Element<'static, Message> = match view {
        OutlineView::Bookmarks if reader.outline.is_empty() => {
            // A document with no bookmarks is not a fault, and the rail says
            // so rather than sitting empty and looking broken.
            nothing("This document has no bookmarks.")
        }
        OutlineView::Bookmarks => {
            let mut list = column![].spacing(2.0);
            for entry in reader.outline {
                let indent = (entry.depth.min(6) as f32) * 12.0;
                let label = text(entry.title.clone())
                    .size(theme::type_scale::LABEL)
                    .width(Length::Fill);
                let control = button(
                    row![space::horizontal().width(Length::Fixed(indent)), label]
                        .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding(Padding::from([3.0, 6.0]));
                list = list.push(if live {
                    control.on_press(send(ReadCommand::GoToPage(entry.page)))
                } else {
                    control
                });
            }
            scrollable(list)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        OutlineView::Thumbnails => {
            let mut list = column![].spacing(theme::space::XS);
            for index in 0..reader.page_count {
                let page = PageIndex(index);
                let selected = page == reader.controls.page;
                let label = text(format!("{page}"))
                    .size(theme::type_scale::LABEL)
                    .color(if selected {
                        theme::ambient::accent()
                    } else {
                        theme::ambient::muted()
                    });
                let control = button(label)
                    .width(Length::Fill)
                    .padding(Padding::from([3.0, 6.0]));
                list = list.push(if live {
                    control.on_press(send(ReadCommand::GoToPage(page)))
                } else {
                    control
                });
            }
            scrollable(list)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    };

    column![toggle, body]
        .spacing(theme::space::XS)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The field inspector: every field, and what it holds.
fn fields<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    if !reader.open {
        return nothing("No document open.");
    }
    if reader.fields.is_empty() {
        // Most PDFs have no AcroForm, and the inspector says which case this
        // is rather than looking like it failed to load. A document that *has*
        // a form and listed no fields is a third case, and calling it "no form
        // fields" would be a plain untruth.
        return nothing(
            match (reader.has_form, reader.level.allows_form_filling()) {
                (false, _) => "This document has no form fields.",
                (true, true) => "This document's fields are not listed in this build.",
                (true, false) => "This document's form cannot be filled.",
            },
        );
    }

    // A navigation and progress list, not a second editor (§8.6). Values are
    // edited in place on the page, by PDFium's own form-fill environment, so
    // there is exactly one editing surface and no way for two of them to
    // disagree about what a field holds.
    let live = mode.interactive();
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };

    let filled = reader
        .fields
        .iter()
        .filter(|field| field.kind.is_fillable() && !field.value.trim().is_empty())
        .count();
    let fillable = reader
        .fields
        .iter()
        .filter(|field| field.is_editable())
        .count();
    let progress = text(format!("{filled} of {fillable} filled"))
        .size(theme::type_scale::LABEL)
        .color(theme::ambient::muted());

    let mut list = column![].spacing(theme::space::XS);
    for field in reader.fields {
        let done = field.kind.is_fillable() && !field.value.trim().is_empty();
        let name = text(field.name.clone()).size(theme::type_scale::LABEL);
        // What it holds, shown as a reading rather than as an input: one line,
        // muted, and never something to type into.
        let value = text(if field.value.trim().is_empty() {
            field.kind.label().to_string()
        } else {
            field.value.clone()
        })
        .size(theme::type_scale::CAPTION)
        .color(theme::ambient::muted());

        let mark: Element<'static, Message> = if done {
            theme::icon::tinted(
                theme::Icon::Check,
                theme::type_scale::LABEL,
                theme::ambient::accent(),
            )
        } else {
            space::horizontal()
                .width(Length::Fixed(theme::type_scale::LABEL))
                .into()
        };

        // Pressing a row takes the reader to the field on the page, where the
        // engine's own editor is; it does not open one here.
        let target = field.pages().first().copied();
        let row = button(
            row![mark, column![name, value].spacing(1.0)]
                .spacing(theme::space::XS)
                .align_y(Alignment::Start),
        )
        .width(Length::Fill)
        .padding(Padding::from([3.0, 6.0]));
        list = list.push(match (live, target) {
            (true, Some(page)) => row.on_press(send(ReadCommand::GoToPage(page))),
            _ => row,
        });
    }

    column![
        progress,
        scrollable(list).width(Length::Fill).height(Length::Fill)
    ]
    .spacing(theme::space::XS)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The annotation toolbar: what a press does to the page.
fn tools<Message: Clone + 'static>(
    _widget: &Widget,
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let live = mode.interactive() && reader.annotatable();
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };

    let mut bar = row![].spacing(theme::space::XS).align_y(Alignment::Center);

    // Nothing armed: the pointer belongs to the document's own links and form
    // fields, which is what a reader wants most of the time.
    let hand = {
        let selected = reader.controls.tool.is_none();
        let control = button(theme::icon::icon(
            theme::Icon::Hand,
            theme::type_scale::HEADING,
        ))
        .padding(Padding::from([4.0, 8.0]))
        .style(if selected {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
        if live {
            control.on_press(send(ReadCommand::Arm(None)))
        } else {
            control
        }
    };
    bar = bar.push(hand);

    for tool in AnnotationTool::DOCUMENT {
        let selected = reader.controls.tool == Some(tool);
        let icon = match tool {
            AnnotationTool::Select => theme::Icon::Select,
            AnnotationTool::Ink => theme::Icon::Pen,
            AnnotationTool::Highlighter => theme::Icon::Highlighter,
            AnnotationTool::Text => theme::Icon::Type,
            AnnotationTool::Note => theme::Icon::StickyNote,
            AnnotationTool::Stamp => theme::Icon::Stamp,
            AnnotationTool::Eraser => theme::Icon::Eraser,
            // Not in `DOCUMENT`; a presenter effect has nothing to act on in
            // a document, and the toolbar does not offer it.
            AnnotationTool::Pointer | AnnotationTool::Spotlight => theme::Icon::Pointer,
        };
        let control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
            .padding(Padding::from([4.0, 8.0]))
            .style(if selected {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            });
        bar = bar.push(if live {
            control.on_press(send(ReadCommand::Arm(Some(tool))))
        } else {
            control
        });
    }

    bar = bar.push(space::horizontal().width(Length::Fixed(theme::space::M)));

    let history = |icon: theme::Icon, command: ReadCommand, enabled: bool| {
        let control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
            .padding(Padding::from([4.0, 8.0]));
        if live && enabled {
            control.on_press(send(command))
        } else {
            control
        }
    };
    bar = bar.push(history(
        theme::Icon::Undo,
        ReadCommand::Undo,
        reader.can_undo,
    ));
    bar = bar.push(history(
        theme::Icon::Redo,
        ReadCommand::Redo,
        reader.can_redo,
    ));
    // Save As rather than Save: the source is never overwritten (A6), and a
    // control labelled "Save" would be promising otherwise.
    bar = bar.push(history(theme::Icon::Save, ReadCommand::SaveAs, reader.open));

    container(bar)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}
