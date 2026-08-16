//! Drawing the reader.
//!
//! Five widgets, one document. The page surface is the only one with any real
//! geometry in it, and even there the arithmetic belongs to
//! [`super::model::Column`]: this file turns already-placed pages into
//! elements, so what is on screen is decided by something that can be tested
//! without a window.

use iced::widget::{
    button, column, container, image, mouse_area, row, scrollable, slider, space, text,
    text_editor, text_input, tooltip, Column, Row,
};
use iced::{Alignment, Element, Length, Padding};

use pulpit_core::annotation::{AnnotationTool, InkColor};
use pulpit_core::page::PageIndex;

use crate::theme;
use crate::widgets::context::{Mode, ReaderData};
use crate::widgets::event::ReadCommand;
use crate::widgets::{Widget, WidgetEvent, WidgetKind};

use super::model::{OutlineView, PageSpread, Zoom};

/// Hand one reader widget its part of the document.
pub fn view<'a, Message: Clone + 'static>(
    widget: &Widget,
    reader: &ReaderData<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    match widget.kind() {
        WidgetKind::DocumentPage => page_surface(reader, compose, mode, on_event),
        WidgetKind::DocumentNav => navigation(reader, mode, on_event),
        WidgetKind::DocumentOutline => outline(reader, mode, on_event),
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

/// A control and the word for what it does, shown while the pointer rests on
/// it.
///
/// The reader's two bands are icons, and an icon is only obvious to the person
/// who chose it. The hint is what makes them legible without spending the row
/// on labels the reader stops seeing after the first day.
fn hint<'a, Message: 'a>(
    control: impl Into<Element<'a, Message>>,
    label: &str,
) -> Element<'a, Message> {
    tooltip(
        control,
        container(text(label.to_string()).size(theme::type_scale::CAPTION))
            .padding(6.0)
            .style(theme::ambient::dialog),
        tooltip::Position::Bottom,
    )
    .into()
}

/// The page surface: the pages in the window, stacked on their mount.
///
/// Only the visible pages are built. Everything else in a thousand-page
/// document costs a rectangle in [`super::model::Column`] and nothing here,
/// which is what makes continuous scroll over a long document affordable.
fn page_surface<'a, Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    compose: Option<&'a iced::widget::text_editor::Content>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    if !reader.open {
        return nothing("No document open.");
    }
    if reader.page_count == 0 {
        return nothing("This document has no pages.");
    }

    // No spacing: every gap between two sheets is pushed explicitly, because
    // the content built here has to be exactly as tall as
    // [`super::model::Column`] says the document is and has to put each page
    // exactly where the column puts it. A scroll offset that means one thing
    // to the model and another to the widget is a document that stops
    // scrolling part way down.
    let mut sheets = column![].spacing(0.0);
    // A leading spacer stands for every page above the window, so the pages
    // that *are* built land where the column says they do rather than at the
    // top of the cell.
    if let Some(first) = reader.visible.first() {
        if first.placed.top > 0.0 {
            sheets = sheets.push(space::vertical().height(Length::Fixed(first.placed.top)));
        }
    }
    let pointer = Pointer {
        armed: reader.controls.tool,
        marqueeing: reader.controls.crop.takes_the_pointer(),
        panning: reader.panning,
    };
    // Pages that share a top are one row, which in a two-page spread is a
    // pair of facing sheets: the column decided that, and this only has to
    // draw what it decided.
    let mut rows: Vec<Vec<&crate::widgets::context::ReaderPage>> = Vec::new();
    for page in reader.visible.iter() {
        match rows.last_mut() {
            Some(row) if (row[0].placed.top - page.placed.top).abs() < f32::EPSILON.max(0.01) => {
                row.push(page)
            }
            _ => rows.push(vec![page]),
        }
    }
    for (index, pages) in rows.iter().enumerate() {
        if index > 0 {
            sheets = sheets.push(space::vertical().height(Length::Fixed(super::model::PAGE_GAP)));
        }
        // Aligned to the top, not stretched: two facing pages of unequal
        // height stand on the same line as they would on a desk.
        let mut facing = row![]
            .spacing(super::model::PAGE_GAP)
            .align_y(Alignment::Start);
        for page in pages {
            // The half-written mark goes to the sheet it was placed on, and to
            // no other: the editor is drawn where the mark will land.
            let composing = reader
                .composing
                .as_ref()
                .filter(|composing| composing.page == page.placed.page);
            facing = facing.push(sheet(
                page,
                mode,
                pointer,
                composing,
                compose,
                reader.date_picker,
                reader.date_language,
                reader.focused_widget,
                reader.focused_hint,
                on_event,
            ));
        }
        sheets = sheets.push(facing);
    }
    // …and a trailing one stands for every page below it, which is what makes
    // the scroll bar the whole document's rather than the window's: without
    // it the content ends at the last built sheet and the reader cannot get
    // past the pages they can already see.
    if let Some(last) = reader.visible.last() {
        // The *row's* bottom: two facing pages need not be the same height,
        // and a spacer measured from the shorter one would make the content
        // shorter than the column says the document is.
        let rest = reader.column.height - last.placed.row_bottom;
        if rest > 0.0 {
            sheets = sheets.push(space::vertical().height(Length::Fixed(rest)));
        }
    }

    // As wide as the column says the document is, and no wider: the column is
    // never narrower than the cell, so this is the cell's width until a zoom
    // makes the page wider than the window and exactly the page's width after
    // that. A `Fill` here would have no meaning in a surface that scrolls
    // sideways, where the space across is unbounded.
    let scroller = scrollable(
        container(sheets)
            .width(Length::Fixed(reader.column.width.max(0.0)))
            .align_x(Alignment::Center)
            // No padding: vertical padding would offset every page from where
            // the column placed it, and horizontal padding would make the
            // content wider than the column and so give a fitted page a
            // sideways scroll it has no room to use.
            .padding(Padding::ZERO),
    )
    .id(page_surface_id())
    // The bar the scrollable draws for itself is sized as a fraction of the
    // content, which over a five-hundred-page document is a two-pixel sliver
    // nobody can hit. The handle beside it is this widget's instead. The
    // horizontal bar is hidden for a different reason: the hand is how a page
    // wider than its window is moved across, and a bar under the page would be
    // a second answer to the same question.
    .direction(scrollable::Direction::Both {
        vertical: scrollable::Scrollbar::new().width(0.0).scroller_width(0.0),
        horizontal: scrollable::Scrollbar::new().width(0.0).scroller_width(0.0),
    })
    .width(Length::Fill)
    .height(Length::Fill);

    // In the editor the surface is a representation, so it takes no events.
    if !mode.interactive() {
        return row![scroller, scroll_handle(reader, mode, on_event)]
            .height(Length::Fill)
            .into();
    }
    // The widget's scroll position is the session's scroll offset: the
    // wheel, the handle and the keyboard all arrive here, and the session
    // decides from it which pages are on screen and which need drawing.
    // The surface's own size comes back with every one of these, which is how
    // a fit is fitted to the window that exists rather than to the cell the
    // layout asked for. Iced sends one whenever the bounds change as well as
    // when the reader scrolls, so a resize corrects the fit without anybody
    // having to scroll first.
    let scroller = scroller.on_scroll(move |viewport| {
        on_event(WidgetEvent::Read(ReadCommand::ScrollTo {
            offset: viewport.absolute_offset().y,
            offset_x: viewport.absolute_offset().x,
            viewport: viewport.bounds().height,
        }))
    });
    row![scroller, scroll_handle(reader, mode, on_event)]
        .height(Length::Fill)
        .into()
}

/// The reader's own scroll handle.
///
/// A slider rather than a scroll bar, because a scroll bar's thumb is as long
/// as the window is a fraction of the document: at five hundred pages that is
/// a couple of pixels, which is not something a hand can catch. A slider's
/// handle is the same size whatever the document's length, which is the whole
/// point of drawing one here.
fn scroll_handle<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let furthest = (reader.column.height - reader.viewport).max(0.0);
    // A document that fits in its window does not scroll, and a handle for a
    // movement that cannot happen is a control that lies.
    if furthest <= 0.0 {
        return space::horizontal()
            .width(Length::Fixed(HANDLE_WIDTH))
            .into();
    }
    // Inverted: a slider's range starts at the bottom, and the top of a
    // document is the start of it.
    let position = (furthest - reader.controls.offset.clamp(0.0, furthest)).max(0.0);
    let control = iced::widget::vertical_slider(0.0..=furthest, position, move |value| {
        on_event(WidgetEvent::Read(ReadCommand::DragScrollHandle(
            furthest - value,
        )))
    })
    .width(HANDLE_WIDTH)
    .height(Length::Fill)
    .style(|theme, status| {
        let mut style = iced::widget::slider::default(theme, status);
        style.handle.shape = iced::widget::slider::HandleShape::Rectangle {
            width: HANDLE_LENGTH,
            border_radius: 3.0.into(),
        };
        style
    });
    if mode.interactive() {
        control.into()
    } else {
        space::horizontal()
            .width(Length::Fixed(HANDLE_WIDTH))
            .into()
    }
}

/// How wide the scroll handle's lane is, in layout points.
const HANDLE_WIDTH: f32 = 14.0;

/// …and how long its handle is, whatever the document's length.
///
/// Big enough to catch with a mouse on the first try, which is the entire
/// reason this is a slider and not the scrollable's own bar.
const HANDLE_LENGTH: u16 = 48;

/// The page surface's scrollable, so the application can put it where a page
/// jump or a zoom moved the session's offset to.
///
/// One document, one surface, one id — the catalogue only ever mounts a single
/// page surface, because two would be two scroll positions in one document.
pub fn page_surface_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-reader-page-surface")
}

/// One page, drawn at the size the column gave it.
///
/// What the pointer is doing to the document, which is what the sheet needs
/// to know to say what the cursor is and who a press belongs to.
#[derive(Debug, Clone, Copy)]
struct Pointer {
    /// The armed annotation tool, if any.
    armed: Option<AnnotationTool>,
    /// Is the marquee crop armed or waiting on an answer? It outranks the
    /// armed tool: while it is on, no tool and no link can have the press.
    marqueeing: bool,
    /// Is the hand dragging the page about?
    panning: bool,
}

/// A page with no frame yet is still drawn, at its full size, as a blank
/// sheet: the alternative is a column that changes height as frames arrive,
/// which moves the text under the reader's eye.
#[allow(
    clippy::too_many_arguments,
    reason = "a sheet is a page plus every transient thing drawn over it; \
              bundling them into a struct for one caller would hide which of \
              them belong to this page and which to the document"
)]
fn sheet<'a, Message: Clone + 'static>(
    page: &crate::widgets::context::ReaderPage,
    mode: Mode,
    pointer: Pointer,
    composing: Option<&crate::widgets::context::ComposingMark>,
    buffer: Option<&'a iced::widget::text_editor::Content>,
    picker: Option<&crate::reader::DatePicker>,
    language: crate::datefield::Locale,
    focused: Option<&pulpit_render::document::protocol::FocusedWidget>,
    focused_hint: Option<&str>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
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
    let drawn = (page.placed.width, page.placed.height);
    let sheet = container(inner)
        .width(Length::Fixed(drawn.0))
        .height(Length::Fixed(drawn.1))
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::WHITE)),
            ..container::Style::default()
        });

    // The unfinished gesture and any committed-but-not-yet-rendered marks go
    // over the page, so a stroke follows the hand instead of the round trip
    // that rasterises it, and does not vanish at release while that round
    // trip runs (A2, §9.2). The retained layers come down the moment a frame
    // containing them arrives, so none of them can duplicate a rendered mark.
    // Everything drawn over the sheet is placed in the page's own points, and
    // the sheet is a picture of the crop window: `shown` is the window's size
    // and `origin` its corner, so one crop-aware conversion serves every
    // layer instead of each one repeating it.
    let shown = (
        page.canonical.0 * page.window.width,
        page.canonical.1 * page.window.height,
    );
    let origin = (
        page.canonical.0 * page.window.x,
        page.canonical.1 * page.window.y,
    );
    let searched = !page.found.is_empty() || !page.found_current.is_empty();
    let sheet: Element<'static, Message> = if page.preview.is_some()
        || !page.retained.is_empty()
        || searched
        || !page.selection.is_empty()
        || page.marquee.is_some()
    {
        let mut layers = iced::widget::stack![sheet];
        // Search hits go under the marks: they are a way of finding the page,
        // not something on it, and must never cover what the reader wrote.
        for (quads, opacity) in [
            (page.found.clone(), 0.28),
            (page.found_current.clone(), 0.55),
        ] {
            if quads.is_empty() {
                continue;
            }
            layers = layers.push(super::preview::layer(
                super::preview::GesturePreview {
                    points: Vec::new(),
                    quads,
                    // The accent, as everywhere else a thing is picked out.
                    color: {
                        let accent = theme::ambient::accent();
                        (accent.r, accent.g, accent.b)
                    },
                    opacity,
                    width: 0.0,
                },
                shown,
                origin,
                drawn,
            ));
        }
        for mark in page.retained.clone() {
            layers = layers.push(super::preview::layer(mark, shown, origin, drawn));
        }
        if let Some(preview) = page.preview.clone() {
            layers = layers.push(super::preview::layer(preview, shown, origin, drawn));
        }
        // The selection goes on top of everything: it is chrome describing
        // what the reader is holding, and chrome that something else can
        // cover is chrome nobody can trust.
        for selection in page.selection.clone() {
            layers = layers.push(super::preview::selection_layer(
                selection,
                theme::ambient::accent(),
                shown,
                origin,
                drawn,
            ));
        }
        // The marquee last of all: it is the reader's own rectangle, and
        // nothing on the page may cover the thing they are drawing.
        if let Some(rect) = page.marquee {
            layers = layers.push(super::preview::marquee_layer(rect, shown, origin, drawn));
        }
        layers.into()
    } else {
        sheet.into()
    };

    if !mode.interactive() {
        return sheet;
    }

    // The editor for a mark being written is *not* inside the mouse area
    // below: it goes over it, so a press meant for the caret or the buttons is
    // taken by them and does not also place a second mark under the first.
    let writing = composing.map(|composing| {
        compose_layer(
            composing,
            buffer,
            (page.placed.width, page.placed.height),
            shown,
            origin,
            on_event,
        )
    });

    // Pointer positions leave this widget in canonical page points and in no
    // other unit (A4). The conversion is per sheet because each sheet knows
    // its own page's size and the size it was drawn at, and a document of
    // mixed page sizes has no single scale to convert by.
    let index = page.placed.page;
    let (drawn_width, drawn_height) = (page.placed.width, page.placed.height);
    let (canonical_width, canonical_height) = page.canonical;
    // The sheet is a picture of the crop window, not necessarily of the whole
    // page, so the conversion starts at the window's own corner. Without this
    // every mark made under a crop would land a margin's width away from
    // where the reader put it.
    let window = page.window;
    let to_page = move |point: iced::Point| -> (f32, f32) {
        let x = if drawn_width > 0.0 {
            (window.x + point.x * window.width / drawn_width) * canonical_width
        } else {
            0.0
        };
        let y = if drawn_height > 0.0 {
            (window.y + point.y * window.height / drawn_height) * canonical_height
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
        // A mark that says something can be reopened by double-clicking it,
        // the way text is opened everywhere else (§8.5).
        .on_double_click(on_event(WidgetEvent::Read(ReadCommand::PageDoubleClicked)))
        .on_release(on_event(WidgetEvent::Read(ReadCommand::PageReleased)))
        // The pointer leaving the sheet mid-stroke ends the gesture rather
        // than leaving it open: a stroke that resumed when the pointer came
        // back would join two marks the user made separately.
        .on_exit(on_event(WidgetEvent::Read(ReadCommand::PageCancelled)));

    // An armed tool takes the pointer away from the document's own links and
    // fields, and the cursor says so before the reader finds out by pressing.
    // The highlighter is the one armed tool that does not draw where the
    // pointer goes — it sweeps the page's own text — so it wears the I-beam
    // the rest of the desktop uses for selecting text, and the crosshair is
    // left to mean "this tool marks the page here". With nothing armed the
    // hand is what the pointer is: the cursor is a hand, open until the
    // button goes down and closed while it is dragging the page about (§8.1).
    // The marquee is a rectangle drawn on the page, so it wears the crosshair
    // every rectangle tool wears — and it takes precedence, because while it
    // is armed no tool and no link can have the press.
    let area: Element<'static, Message> = if pointer.marqueeing {
        area.interaction(iced::mouse::Interaction::Crosshair).into()
    } else if let Some(tool) = pointer.armed {
        let cursor = match tool {
            AnnotationTool::Highlighter => iced::mouse::Interaction::Text,
            _ => iced::mouse::Interaction::Crosshair,
        };
        area.interaction(cursor).into()
    } else if pointer.panning {
        area.interaction(iced::mouse::Interaction::Grabbing).into()
    } else {
        area.interaction(iced::mouse::Interaction::Grab).into()
    };

    // The calendar over a date field, on the page that field is on. Placed
    // like the writing box and for the same reason: it belongs beside the
    // field, not in a dialog covering the form it is filling in.
    let calendar = picker
        .filter(|picker| picker.page == index)
        .map(|picker| date_picker_layer(picker, language, shown, origin, drawn, on_event));

    // The ring around the field with the focus, and what that field expects.
    // Both are pulpit's own chrome, drawn from the widget's page rectangle at
    // this sheet's current geometry — so they follow a scroll and a zoom, and
    // they outlive the frame swap that throws PDFium's own decoration away.
    let focused = focused.filter(|widget| widget.page == index);
    let ring = focused.map(|widget| {
        focus_ring_layer::<Message>(
            super::model::Anchor::of(widget.bounds, shown, origin, drawn),
            drawn,
        )
    });
    let hint = focused.zip(focused_hint).map(|(widget, hint)| {
        field_hint_layer::<Message>(
            super::model::Anchor::of(widget.bounds, shown, origin, drawn),
            hint,
            drawn,
        )
    });

    let overlays = [writing, calendar, ring, hint];
    if overlays.iter().all(Option::is_none) {
        return area;
    }
    let mut layers = iced::widget::stack![area];
    for overlay in overlays.into_iter().flatten() {
        layers = layers.push(overlay);
    }
    layers.into()
}

/// How far the focus ring stands off the widget it marks, in layout points.
const FOCUS_RING_MARGIN: f32 = 2.0;
/// How thick it is. Two points is the width every other focus indicator on
/// the desktop uses; thinner disappears against a field's own border.
const FOCUS_RING_WIDTH: f32 = 2.0;

/// The ring around the field holding the focus (§8.6).
///
/// Drawn here rather than taken from the picture. PDFium's own focus
/// decoration is in the rectangles it invalidates, and those arrive as
/// patches; a full page frame is rendered by the pool from a fresh form
/// environment that has no focus in it, so a ring that came from the bitmap
/// would blink out every time a frame superseded a patch (A2). A ring that is
/// an element cannot: it is rebuilt from the widget's page rectangle on every
/// view, whatever the pixels underneath are doing.
fn focus_ring_layer<Message: 'static>(
    anchor: super::model::Anchor,
    drawn: (f32, f32),
) -> Element<'static, Message> {
    let ring = anchor.inflated(FOCUS_RING_MARGIN, drawn);
    let outline = container(space::horizontal())
        .width(Length::Fixed(ring.width))
        .height(Length::Fixed(ring.height))
        .style(|_theme| container::Style {
            border: iced::Border {
                color: theme::ambient::accent(),
                width: FOCUS_RING_WIDTH,
                radius: 3.0.into(),
            },
            ..container::Style::default()
        });
    placed_over_the_sheet(outline, (ring.left, ring.top), drawn)
}

/// What the focused field expects, said beside the field rather than only in
/// the diagnostics — "this field takes a date, as dd mmmm yyyy" is an answer
/// to a question asked at the field, and a line in a log is not where the
/// reader is looking.
fn field_hint_layer<Message: 'static>(
    anchor: super::model::Anchor,
    hint: &str,
    drawn: (f32, f32),
) -> Element<'static, Message> {
    /// Roughly how wide a caption character is, and how tall the box is with
    /// its padding. Estimated rather than measured because the placement has
    /// to be decided before the text is laid out; it is only used to keep the
    /// box on the sheet, so being a little out costs a little clamping.
    const CHARACTER: f32 = 0.58;
    const PADDING: f32 = 6.0;

    let label = format!("Takes {hint}");
    let width = (label.chars().count() as f32 * theme::type_scale::CAPTION * CHARACTER
        + 2.0 * PADDING)
        .min(drawn.0.max(0.0));
    let height = theme::type_scale::CAPTION * 1.4 + 2.0 * PADDING;
    let (left, top) = anchor
        .inflated(FOCUS_RING_MARGIN, drawn)
        .place_beside((width, height), drawn);

    let bubble = container(text(label).size(theme::type_scale::CAPTION))
        .padding(PADDING)
        .style(theme::ambient::dialog);
    placed_over_the_sheet(bubble, (left, top), drawn)
}

/// Put `content` at a point on the sheet, in a box exactly the sheet's size.
///
/// Spacers rather than absolute positioning, the way every other layer over a
/// page is placed: the sheet is a fixed-size box, and a layer the same size
/// stacks over it without changing what the column thinks the page measures.
fn placed_over_the_sheet<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    at: (f32, f32),
    drawn: (f32, f32),
) -> Element<'a, Message> {
    container(column![
        space::vertical().height(Length::Fixed(at.1)),
        row![
            space::horizontal().width(Length::Fixed(at.0)),
            content.into()
        ],
    ])
    .width(Length::Fixed(drawn.0))
    .height(Length::Fixed(drawn.1))
    .into()
}

/// The calendar pulpit draws over a date field (§8.6).
///
/// A PDF says a field holds a date and says what shape the value takes; it
/// offers no picker, because a picker is a viewer's answer rather than the
/// file's. Acrobat and PDF Studio each draw one, and this is pulpit's.
///
/// What comes out of it is *text*, written the way the field's own pattern
/// asks for, handed to PDFium's editor like any other typed value — so §8.6's
/// one editing surface survives a calendar sitting on top of it.
fn date_picker_layer<Message: Clone + 'static>(
    picker: &crate::reader::DatePicker,
    language: crate::datefield::Locale,
    shown: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    use crate::datefield::{Date, Weekday};

    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };
    /// Wide enough for seven columns and a month name over them.
    const WIDTH: f32 = 224.0;
    const HEIGHT: f32 = 248.0;
    /// One day, square, so the grid reads as a calendar rather than a table.
    const CELL: f32 = 28.0;

    let scale_x = if shown.0 > 0.0 {
        drawn.0 / shown.0
    } else {
        1.0
    };
    let scale_y = if shown.1 > 0.0 {
        drawn.1 / shown.1
    } else {
        1.0
    };

    // Under the field by preference, which is where a dropdown goes; above it
    // when there is no room below, so a field near the foot of the page still
    // opens a calendar the reader can see all of.
    let left = ((picker.bounds.left - origin.0) * scale_x).clamp(0.0, (drawn.0 - WIDTH).max(0.0));
    let below = (picker.bounds.bottom - origin.1) * scale_y;
    let above = (picker.bounds.top - origin.1) * scale_y - HEIGHT;
    let top = if below + HEIGHT <= drawn.1 || above < 0.0 {
        below.clamp(0.0, (drawn.1 - HEIGHT).max(0.0))
    } else {
        above.max(0.0)
    };

    let step = |label: &str, forward: bool| {
        button(text(label.to_string()).size(theme::type_scale::LABEL))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button)
            .on_press(send(ReadCommand::StepDatePicker(forward)))
    };
    let header = row![
        step("‹", false),
        container(
            text(picker.month.title(language))
                .size(theme::type_scale::LABEL)
                .align_x(Alignment::Center)
        )
        .width(Length::Fill)
        .align_y(Alignment::Center),
        step("›", true),
    ]
    .align_y(Alignment::Center);

    let mut headings = Row::new();
    for weekday in Weekday::ALL {
        headings = headings.push(
            container(
                text(language.weekday_initial(weekday))
                    .size(theme::type_scale::CAPTION)
                    .align_x(Alignment::Center),
            )
            .width(Length::Fixed(CELL)),
        );
    }

    let mut grid = Column::new().spacing(1.0);
    for week in picker.month.grid() {
        let mut line = Row::new().spacing(1.0);
        for cell in week {
            line = line.push(match cell {
                Some(day) => {
                    let date = Date::new(picker.month.year, picker.month.month, day);
                    // Today is marked, because "what is the date" is the
                    // question a date field usually asks.
                    let style = if date == picker.today {
                        theme::ambient::selected_button
                    } else {
                        theme::ambient::tool_button
                    };
                    Element::from(
                        button(
                            text(day.to_string())
                                .size(theme::type_scale::CAPTION)
                                .align_x(Alignment::Center),
                        )
                        .width(Length::Fixed(CELL))
                        .padding(2.0)
                        .style(style)
                        .on_press(send(ReadCommand::PickDate(date))),
                    )
                }
                // The blanks a month leaves at its corners. Space rather than
                // an empty button, so nothing there can be pressed.
                None => Element::from(space::horizontal().width(Length::Fixed(CELL))),
            });
        }
        grid = grid.push(line);
    }

    let panel = container(
        column![
            header,
            headings,
            grid,
            button(text("Close").size(theme::type_scale::CAPTION))
                .padding(theme::space::XS)
                .style(theme::ambient::tool_button)
                .on_press(send(ReadCommand::CloseDatePicker)),
        ]
        .spacing(theme::space::XS),
    )
    .padding(theme::space::XS)
    .width(Length::Fixed(WIDTH))
    .style(theme::ambient::surface);

    let placed = column![
        space::vertical().height(Length::Fixed(top)),
        row![space::horizontal().width(Length::Fixed(left)), panel],
    ];
    container(placed)
        .width(Length::Fixed(drawn.0))
        .height(Length::Fixed(drawn.1))
        .into()
}

/// The caret for a mark being written, on the page where it will land (§8.5).
///
/// A real [`text_input`] rather than something hand-rolled, so an input
/// method, a clipboard, a selection and dead keys all behave the way they do
/// everywhere else on the machine — but placed on the sheet at the spot that
/// was clicked, rather than in a dialog that would cover the very page the
/// reader is writing on.
///
/// Positioned with spacers because the sheet is a fixed-size box and the spot
/// is a canonical page point (A4): the same conversion the pointer takes, the
/// other way round.
fn compose_layer<'a, Message: Clone + 'static>(
    composing: &crate::widgets::context::ComposingMark,
    buffer: Option<&'a iced::widget::text_editor::Content>,
    drawn: (f32, f32),
    canonical: (f32, f32),
    // The page point the sheet's corner stands for: the crop window's own, or
    // the page's origin when nothing is cropped.
    origin: (f32, f32),
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    /// How wide the writing box is on screen, in layout points.
    const EDITOR_WIDTH: f32 = 260.0;

    let scale_x = if canonical.0 > 0.0 {
        drawn.0 / canonical.0
    } else {
        1.0
    };
    let scale_y = if canonical.1 > 0.0 {
        drawn.1 / canonical.1
    } else {
        1.0
    };
    // Clamped to the sheet: a mark placed near the right or bottom edge is
    // still written in a box the reader can see all of.
    let width = EDITOR_WIDTH.min(drawn.0.max(0.0));
    // The box grows with what has been written, up to a point: a note is a
    // paragraph often enough that a one-line slot would hide most of it, and
    // past a handful of lines a box that kept growing would cover the page it
    // is a comment on. After that the editor scrolls, as it should.
    let lines = buffer.map_or(1, |buffer| buffer.line_count()).clamp(1, 6);
    let height = (COMPOSE_HEIGHT + (lines - 1) as f32 * theme::type_scale::BODY * 1.3)
        .min(drawn.1.max(COMPOSE_HEIGHT));
    let left = ((composing.at.x - origin.0) * scale_x).clamp(0.0, (drawn.0 - width).max(0.0));
    let top = ((composing.at.y - origin.1) * scale_y).clamp(0.0, (drawn.1 - height).max(0.0));

    // A real multi-line editor, so a note can be a paragraph. Return breaks
    // the line, the way it does in every other box that holds prose; the mark
    // is placed with the tick, with Ctrl+Return, or by pressing Escape's
    // opposite — never by the key that is also how a second line is written.
    let editor: Element<'a, Message> = match buffer {
        Some(buffer) => text_editor(buffer)
            .id(compose_input_id())
            .size(theme::type_scale::BODY)
            .height(Length::Fixed(height))
            .padding(theme::space::XS)
            .key_binding(move |press| {
                use iced::widget::text_editor::{Binding, KeyPress};
                let KeyPress { key, modifiers, .. } = &press;
                let is_enter = matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter)
                );
                if is_enter && (modifiers.command() || modifiers.control()) {
                    return Some(Binding::Custom(on_event(WidgetEvent::Read(
                        ReadCommand::CommitMark,
                    ))));
                }
                if matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                ) {
                    return Some(Binding::Custom(on_event(WidgetEvent::Read(
                        ReadCommand::CancelMark,
                    ))));
                }
                Binding::from_key_press(press)
            })
            .on_action(move |action| on_event(WidgetEvent::Read(ReadCommand::ComposeMark(action))))
            .into(),
        // The buffer belongs to the application; without one there is nothing
        // to type into, and an empty box would take the click anyway.
        None => space::horizontal().width(Length::Fixed(width)).into(),
    };
    let mut controls = row![container(editor).width(Length::Fixed(width))]
        .spacing(theme::space::XS)
        .align_y(Alignment::Center);

    // Placing the mark. The tick is the visible way to do it, because Return
    // now means what it means in prose.
    controls = controls.push(
        button(text("✓").size(theme::type_scale::LABEL))
            .padding(theme::space::XS)
            .style(theme::ambient::selected_button)
            .on_press(on_event(WidgetEvent::Read(ReadCommand::CommitMark))),
    );

    // A sticky note is read in a viewer's own popup, which draws `/Contents`
    // and not an appearance, so a typeset note would be a mark whose text
    // nobody sees. The choice is therefore only offered for the text tool.
    if composing.tool != AnnotationTool::Note {
        let typst = composing.typst;
        controls = controls.push(
            button(text(if typst { "Typst ✓" } else { "Typst" }).size(theme::type_scale::LABEL))
                .padding(theme::space::XS)
                .style(if typst {
                    theme::ambient::selected_button
                } else {
                    theme::ambient::tool_button
                })
                .on_press(on_event(WidgetEvent::Read(ReadCommand::ComposeAsTypst(
                    !typst,
                )))),
        );
    }
    controls = controls.push(
        button(text("✕").size(theme::type_scale::LABEL))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button)
            .on_press(on_event(WidgetEvent::Read(ReadCommand::CancelMark))),
    );

    let placed = column![
        space::vertical().height(Length::Fixed(top)),
        row![space::horizontal().width(Length::Fixed(left)), controls],
    ];
    container(placed)
        .width(Length::Fixed(drawn.0))
        .height(Length::Fixed(drawn.1))
        .into()
}

/// How tall the writing box is with one line in it. It grows from here as
/// lines are written, and this is the floor used to keep it on the sheet.
const COMPOSE_HEIGHT: f32 = 34.0;

/// The writing box's input, so the application can put the caret in it the
/// moment a spot is chosen. One mark is written at a time, so one id.
pub fn compose_input_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-reader-compose-mark")
}

/// The crop latch, and the question a drawn rectangle asks.
///
/// A latch rather than a momentary press, because what it does is not over
/// when the press is: while it is down the pointer draws rectangles instead of
/// reaching the page, and after a crop is taken it is what puts the margins
/// back. One press on, one press off.
fn crop_control<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    live: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    use super::model::{CropChoice, CropState};

    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };
    let crop = reader.controls.crop;
    let control = button(theme::icon::icon(
        theme::Icon::Crop,
        theme::type_scale::HEADING,
    ))
    .padding(Padding::from([4.0, 8.0]))
    .style(if crop.is_on() {
        theme::ambient::selected_button
    } else {
        theme::ambient::tool_button
    });
    let control = if live {
        control.on_press(send(ReadCommand::ArmCrop(!crop.is_on())))
    } else {
        control
    };
    let trigger = hint(control, crop.label());

    // The question, anchored to the button rather than thrown up as a dialog:
    // the rectangle the reader is being asked about is on the page behind it,
    // and a dialog in the middle of the window would cover the very thing the
    // answer is about.
    // Two choices and no third: Escape takes the rectangle back, and so does a
    // press on the page — a cancel button would be a control for something
    // the reader already has two ways of doing.
    let panel = (live && matches!(crop, CropState::Choosing(_))).then(|| {
        let choice = |label: &str, command: ReadCommand| -> Element<'static, Message> {
            button(text(label.to_string()).size(theme::type_scale::LABEL))
                .padding(Padding::from([6.0, 10.0]))
                .width(Length::Fixed(180.0))
                .style(theme::ambient::tool_button)
                .on_press(send(command))
                .into()
        };
        container(
            column![
                choice("Zoom on this page", ReadCommand::TakeCrop(CropChoice::Zoom),),
                choice("Crop every page", ReadCommand::TakeCrop(CropChoice::Pages)),
            ]
            .spacing(theme::space::XS),
        )
        .padding(theme::space::XS)
        .style(theme::ambient::dialog)
        .into()
    });
    crate::widgets::annotations::popover::Popover::new(trigger, panel).into()
}

/// The navigation band: where you are, and how big the page is.
fn navigation<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let live = mode.interactive() && reader.open;
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };

    let step = |icon: theme::Icon, label: &str, command: ReadCommand, enabled: bool| {
        let control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
            .padding(Padding::from([4.0, 8.0]))
            .style(theme::ambient::tool_button);
        let control = if live && enabled {
            control.on_press(send(command))
        } else {
            control
        };
        hint(control, label)
    };

    // Only the page number is typed into. The total is a fact about the
    // document, not a thing a reader can decide, so it sits beside the box as
    // text rather than inside it where a stray keystroke could eat it.
    let entry = {
        let value = reader
            .page_entry
            .clone()
            .unwrap_or_else(|| reader.page_label());
        let field = text_input("", &value)
            .size(theme::type_scale::LABEL)
            .width(Length::Fixed(48.0))
            .align_x(Alignment::Center);
        let field = if live {
            field
                .on_input(move |typed| send(ReadCommand::TypePage(typed)))
                .on_submit(send(ReadCommand::CommitPage))
        } else {
            field
        };
        row![
            field,
            text(reader.page_total())
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::muted()),
        ]
        .spacing(theme::space::XS)
        .align_y(Alignment::Center)
    };

    let previous = reader.controls.page.get() > 0;
    let next = reader.controls.page.get() + 1 < reader.page_count;

    // The scale, never the fit's name: the fit is what the icon beside it
    // says, and repeating it in words costs the band the one thing the icons
    // cannot show — how big the page actually is right now.
    let zoom_label = text(format!("{}%", (reader.scale * 100.0).round() as i32))
        .size(theme::type_scale::LABEL)
        .color(theme::ambient::muted());

    // Where you are and how big the page is are two different questions, so
    // the band answers them in two groups with a rule between them rather
    // than as one undifferentiated run of controls.
    let group = |controls: Row<'static, Message>| {
        container(
            controls
                .spacing(theme::space::XS)
                .align_y(Alignment::Center),
        )
    };
    let rule = || {
        container(space::horizontal())
            .width(Length::Fixed(1.0))
            .height(Length::Fixed(theme::space::M))
            .style(theme::ambient::separator)
    };

    let pages = group(row![
        // Back and forward, not strictly previous and next: these follow the
        // history when a jump has been made and step a page when it has not,
        // so returning from an overview jump is one press rather than a hunt
        // for the page you were on.
        step(
            theme::Icon::ChevronLeft,
            "Back",
            ReadCommand::HistoryBack,
            previous || reader.can_go_back
        ),
        entry,
        step(
            theme::Icon::ChevronRight,
            "Forward",
            ReadCommand::HistoryForward,
            next || reader.can_go_forward
        ),
    ]);

    let sizing = group(row![
        step(theme::Icon::ZoomOut, "Zoom out", ReadCommand::ZoomOut, true),
        zoom_label,
        step(theme::Icon::ZoomIn, "Zoom in", ReadCommand::ZoomIn, true),
        step(
            theme::Icon::FitWidth,
            "Fit width",
            ReadCommand::SetZoom(Zoom::FitWidth),
            true
        ),
        step(
            theme::Icon::FitHeight,
            "Fit height",
            ReadCommand::SetZoom(Zoom::FitHeight),
            true
        ),
        step(
            theme::Icon::FitPage,
            "Fit page",
            ReadCommand::SetZoom(Zoom::FitPage),
            true
        ),
        crop_control(reader, live, on_event),
        // The spread belongs with the zoom rather than in a group of its own:
        // how many pages stand across the window is the same question as how
        // big they are, and the reader who wants two of them is answering it.
        // One control, not two — the spread has exactly two states, and a
        // button that shows the one you are not in is a press rather than a
        // choice between a pressed and an unpressed twin.
        step(
            match reader.controls.spread.other() {
                PageSpread::Single => theme::Icon::SinglePage,
                PageSpread::Double => theme::Icon::TwoPages,
            },
            reader.controls.spread.other().label(),
            ReadCommand::SetSpread(reader.controls.spread.other()),
            true
        ),
    ]);

    let band = row![pages, rule(), sizing]
        .spacing(theme::space::M)
        .align_y(Alignment::Center);

    // The band and nothing beside it. The compatibility level and the unsaved
    // mark used to sit at the far end of this row; they are said elsewhere,
    // and a line of standing text in the reader's own chrome is a thing the
    // eye stops seeing long before it stops costing the row.
    container(band.width(Length::Fill))
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
    let collapsed = reader.controls.outline_collapsed;

    // The disclosure control is the rail's own: collapsing is a property of
    // this widget, not of where the layout put it, so it lives in the header
    // beside the view toggle rather than in the layout tree.
    let disclosure = || {
        let control = button(theme::icon::muted(
            if collapsed {
                theme::icon::Icon::ChevronRight
            } else {
                theme::icon::Icon::ChevronDown
            },
            theme::type_scale::LABEL,
        ))
        .padding(Padding::from([4.0, 6.0]))
        .style(theme::ambient::tool_button);
        if mode.interactive() {
            control.on_press(send(ReadCommand::SetOutlineCollapsed(!collapsed)))
        } else {
            control
        }
    };

    // Both views are named at once, and the one in front is underlined. A
    // tab says what the rail can show; the button it replaces named only the
    // view you were not looking at, which is the harder thing to read.
    let tab = |target: OutlineView| -> Element<'static, Message> {
        let selected = target == view;
        let label = text(target.label().to_string())
            .size(theme::type_scale::LABEL)
            .color(if selected {
                theme::ambient::text()
            } else {
                theme::ambient::muted()
            });
        let control = button(label)
            .padding(Padding::from([2.0, 6.0]))
            .style(theme::ambient::tool_button);
        // The tab in front is not a control: pressing it would change nothing,
        // so it does not offer to.
        let control = if live && !selected {
            control.on_press(send(ReadCommand::SetOutlineView(target)))
        } else {
            control
        };
        // A two-point rule under the selected tab, and nothing but space under
        // the others, so the row does not move as the selection changes.
        let underline: Element<'static, Message> = if selected {
            container(space::horizontal().width(Length::Fill).height(2.0))
                .style(theme::ambient::accent_rule)
                .width(Length::Fill)
                .into()
        } else {
            space::horizontal().width(Length::Fill).height(2.0).into()
        };
        column![control, underline]
            .spacing(2.0)
            .align_x(Alignment::Center)
            .into()
    };

    let tabs = row![tab(OutlineView::Bookmarks), tab(OutlineView::Thumbnails)]
        .spacing(theme::space::XS)
        .align_y(Alignment::Center);

    let header = row![disclosure(), tabs]
        .spacing(theme::space::XS)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    if collapsed {
        // Collapsed, the rail keeps only its chevron and the name of what is
        // put away: a row of tabs for a list nobody can see would be a control
        // that does nothing visible.
        let label = text(view.label().to_string())
            .size(theme::type_scale::LABEL)
            .color(theme::ambient::muted());
        let shut = row![disclosure(), label]
            .spacing(theme::space::XS)
            .align_y(Alignment::Center)
            .width(Length::Fill);
        return column![shut]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

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
                .padding(Padding::from([3.0, 6.0]))
                .style(theme::ambient::tool_button);
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
                    .padding(Padding::from([3.0, 6.0]))
                    .style(if selected {
                        theme::ambient::selected_button
                    } else {
                        theme::ambient::tool_button
                    });
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

    column![header, body]
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
        let control = if live {
            control.on_press(send(ReadCommand::Arm(None)))
        } else {
            control
        };
        hint(
            control,
            "Hand — drag the page about, and the document's own links and fields",
        )
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
        // A tool drawn in the colour it is about to lay down, like the
        // presenter palette: the tint is what says which colour is armed
        // without opening the options.
        let colour = tool_colour(tool, reader);
        let glyph: Element<'static, Message> = match colour {
            Some(colour) => {
                let (r, g, b) = colour.rgb();
                theme::icon::tinted(
                    icon,
                    theme::type_scale::HEADING,
                    iced::Color::from_rgb(r, g, b),
                )
            }
            None => theme::icon::icon(icon, theme::type_scale::HEADING),
        };
        let control = button(glyph)
            .padding(Padding::from([4.0, 8.0]))
            .style(if selected {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            });
        let control = if live {
            control.on_press(send(ReadCommand::Arm(Some(tool))))
        } else {
            control
        };
        let control = hint(control, tool.label());

        // A colour-bearing tool carries a narrow arrow in its corner, which
        // opens the swatches — and, for the pen, its width — in a popover
        // over the button itself.
        if colour.is_some() {
            let open = reader.controls.tool_options == Some(tool);
            let mut arrow = button(theme::icon::icon(theme::Icon::ChevronDown, 9.0))
                .padding(0)
                .width(Length::Fixed(12.0))
                .height(Length::Fixed(12.0))
                .style(theme::ambient::tool_button);
            if live {
                arrow = arrow.on_press(send(ReadCommand::ToolOptions(if open {
                    None
                } else {
                    Some(tool)
                })));
            }
            let corner = container(arrow)
                .height(Length::Fixed(theme::type_scale::HEADING + 8.0))
                .align_y(Alignment::End);
            let trigger = row![control, corner].spacing(0).align_y(Alignment::Center);
            let panel = (live && open).then(|| tool_options_panel(tool, reader, on_event));
            bar = bar.push(Element::from(
                crate::widgets::annotations::popover::Popover::new(trigger, panel),
            ));
        } else {
            bar = bar.push(control);
        }
    }

    bar = bar.push(space::horizontal().width(Length::Fixed(theme::space::M)));

    let history = |icon: theme::Icon, label: &str, command: ReadCommand, enabled: bool| {
        let control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
            .padding(Padding::from([4.0, 8.0]))
            .style(theme::ambient::tool_button);
        let control = if live && enabled {
            control.on_press(send(command))
        } else {
            control
        };
        hint(control, label)
    };
    // What to do with the mark that is held. Dim until something is, which is
    // also how the toolbar says that nothing has been picked up — the outline
    // on the page says the same thing from the other side (§8.4). There is no
    // button for editing what a mark says: that is what double-clicking it
    // does, the same as double-clicking text anywhere else.
    bar = bar.push(history(
        theme::Icon::Trash,
        "Delete the selected mark",
        ReadCommand::DeleteSelected,
        reader.selected,
    ));
    bar = bar.push(space::horizontal().width(Length::Fixed(theme::space::M)));
    bar = bar.push(history(
        theme::Icon::Undo,
        "Undo",
        ReadCommand::Undo,
        reader.can_undo,
    ));
    bar = bar.push(history(
        theme::Icon::Redo,
        "Redo",
        ReadCommand::Redo,
        reader.can_redo,
    ));
    // Save As rather than Save: the source is never overwritten (A6), and a
    // control labelled "Save" would be promising otherwise.
    bar = bar.push(history(
        theme::Icon::Save,
        "Save as…",
        ReadCommand::SaveAs,
        reader.open,
    ));

    container(bar)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}

/// The colour a document tool lays down, or `None` for the tools that lay
/// none — which are also the ones with no options to open.
fn tool_colour(tool: AnnotationTool, reader: &ReaderData<'_>) -> Option<InkColor> {
    match tool {
        AnnotationTool::Ink => Some(reader.controls.ink_color),
        AnnotationTool::Highlighter => Some(reader.controls.highlight_color),
        AnnotationTool::Text | AnnotationTool::Note => Some(reader.controls.text_color),
        _ => None,
    }
}

/// A tool's options: its swatches, and for the pen its width.
fn tool_options_panel<Message: Clone + 'static>(
    tool: AnnotationTool,
    reader: &ReaderData<'_>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };
    let selected = tool_colour(tool, reader).unwrap_or_default();

    let mut swatches = Row::new().spacing(theme::space::XS);
    for colour in InkColor::ALL {
        let (r, g, b) = colour.rgb();
        swatches = swatches.push(
            button(space::horizontal())
                .width(Length::Fixed(24.0))
                .height(Length::Fixed(24.0))
                .padding(0)
                .style(theme::color_swatch_button(
                    iced::Color::from_rgb(r, g, b),
                    selected == colour,
                ))
                .on_press(send(ReadCommand::SetToolColor(tool, colour))),
        );
    }

    let mut panel =
        column![text("Color").size(theme::type_scale::CAPTION), swatches].spacing(theme::space::XS);

    if tool == AnnotationTool::Ink {
        let width = reader.controls.ink_width;
        panel = panel
            .push(text(format!("Width — {width:.1} pt")).size(theme::type_scale::CAPTION))
            .push(
                slider(0.5..=12.0, width, move |value| {
                    send(ReadCommand::SetInkWidth(value))
                })
                .step(0.5_f32),
            );
    }

    // Type has a size for the same reason the pen has a width: it is the one
    // measure a reader changes often, and until now the only way to change it
    // was not to.
    if matches!(tool, AnnotationTool::Text | AnnotationTool::Note) {
        let size = reader.controls.text_size;
        panel = panel
            .push(text(format!("Size — {size:.0} pt")).size(theme::type_scale::CAPTION))
            .push(
                slider(6.0..=48.0, size, move |value| {
                    send(ReadCommand::SetTextSize(value))
                })
                .step(1.0_f32),
            );
    }

    container(panel)
        .width(Length::Fixed(232.0))
        .padding(theme::space::M)
        .style(theme::ambient::surface)
        .into()
}
