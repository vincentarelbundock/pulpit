//! Drawing the reader.
//!
//! Five widgets, one document. The page surface is the only one with any real
//! geometry in it, and even there the arithmetic belongs to
//! [`super::model::Column`]: this file turns already-placed pages into
//! elements, so what is on screen is decided by something that can be tested
//! without a window.

use iced::widget::{
    button, column, container, image, mouse_area, responsive, row, scrollable, slider, space, text,
    text_editor, text_input, tooltip, Column, Row,
};
use iced::{Alignment, Element, Length, Padding};

use pulpit_core::annotation::{AnnotationTool, InkColor};

use crate::theme;
use crate::theme::Icon;
use crate::widgets::context::{Mode, ReaderData};
use crate::widgets::event::ReadCommand;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetEvent, WidgetKind};

use super::model::{CropChoice, CropState, OutlineView, PageSpread, SidebarTab, Zoom};

/// One outline row plus the two-point gap below it.
pub const OUTLINE_ROW_HEIGHT: f32 = 28.0;

/// Hand one reader widget its part of the document.
pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let reader = &ctx.context.reader;
    let mode = ctx.context.mode;
    let on_event = ctx.on_event;
    match widget.kind() {
        WidgetKind::DocumentPage => page_surface(reader, ctx.compose, mode, on_event),
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
            .padding(theme::space::S)
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

    let surface = PageSurface::from(reader);
    // Read out here rather than inside: the closure outlives the borrow, and
    // the thumb only needs the number.
    let offset = reader.controls.offset;
    let page: Element<'a, Message> = responsive(move |viewport| {
        let sheets = surface.sheets(compose, mode, on_event);
        let scroller = scrollable(
            container(sheets)
                // A horizontally scrolling surface lays fluid children out
                // at their intrinsic width. Use the live viewport explicitly
                // so there is spare space in which to centre a fitted page,
                // while a page wider than the viewport keeps its full width
                // and remains horizontally scrollable.
                .width(Length::Fixed(page_surface_width(
                    surface.column_width,
                    viewport.width,
                )))
                .align_x(Alignment::Center)
                // No padding: vertical padding would offset every page from
                // where the column placed it, and horizontal padding would
                // make the content wider than the column and so give a
                // fitted page a sideways scroll it has no room to use.
                .padding(Padding::ZERO),
        )
        .id(page_surface_id())
        // The vertical bar is the same proportional native scrollbar every
        // other scrolling surface uses. Horizontal movement remains
        // gesture-only.
        .direction(scrollable::Direction::Both {
            vertical: crate::widgets::scroll::bar(),
            horizontal: scrollable::Scrollbar::new().width(0.0).scroller_width(0.0),
        })
        .style(theme::ambient::scrollbar)
        .width(Length::Fill)
        .height(Length::Fill);

        // In the editor the surface is a representation, so it takes no
        // events.
        if !mode.interactive() {
            return crate::widgets::scroll::thumbed(scroller, offset, move |_| {
                // A representation takes no events, so the thumb is drawn
                // and never dragged; the surface still shows where it sits.
                on_event(WidgetEvent::Read(ReadCommand::DragScrollTo(offset)))
            });
        }
        // The widget's scroll position is the session's scroll offset: the
        // wheel, the handle and the keyboard all arrive here, and the session
        // decides from it which pages are on screen and which need drawing.
        // The surface's own size comes back with every scroll event, which is
        // how a fit is fitted to the window that exists rather than to the
        // cell the layout asked for.
        crate::widgets::scroll::thumbed(
            scroller.on_scroll(move |viewport| {
                on_event(WidgetEvent::Read(ReadCommand::ScrollTo {
                    offset: viewport.absolute_offset().y,
                    offset_x: viewport.absolute_offset().x,
                    viewport: viewport.bounds().height,
                }))
            }),
            offset,
            move |offset| on_event(WidgetEvent::Read(ReadCommand::DragScrollTo(offset))),
        )
    })
    .into();
    // Whether these keys mean scrolling *now* — captured, never used to decide
    // whether to wrap. Taking the key scope away when a field takes the focus
    // would change the shape of the tree between the window root and the
    // scrollable, and Iced would tear that scrollable down and mount a fresh
    // one born at zero: the reader would lose its place on every click into a
    // form, and again on every click back out. The scope is layout-transparent,
    // so leaving it mounted and inert costs nothing. See
    // `crate::view::document_surface_fingerprint` for the same hazard stated
    // from the other end.
    let scrolls = mode.interactive()
        && reader.document_keyboard_focus
        && reader.focused_widget.is_none()
        && reader.composing.is_none();
    crate::widgets::panel::on_key(page, move |key, modifiers| {
        use iced::keyboard::{key::Named, Key};
        if !scrolls || modifiers.control() || modifiers.alt() || modifiers.logo() {
            return None;
        }
        match key {
            Key::Named(Named::ArrowDown) => Some(on_event(WidgetEvent::Read(
                ReadCommand::ScrollByPoints(48.0),
            ))),
            Key::Named(Named::ArrowUp) => Some(on_event(WidgetEvent::Read(
                ReadCommand::ScrollByPoints(-48.0),
            ))),
            Key::Named(Named::Tab) => Some(on_event(WidgetEvent::Panel(
                crate::widgets::event::PanelCommand::FocusSidebar,
            ))),
            _ => None,
        }
    })
}

/// The owned part of a page surface.
///
/// [`responsive`] rebuilds its child during layout, after the context borrowed
/// by [`view`] is gone. Keeping this small snapshot lets it use the viewport's
/// real width without tying the returned element to that short-lived context.
struct PageSurface {
    pages: Vec<crate::widgets::context::ReaderPage>,
    column_height: f32,
    column_width: f32,
    pointer: Pointer,
    composing: Option<crate::widgets::context::ComposingMark>,
    date_picker: Option<crate::reader::DatePicker>,
    time_picker: Option<crate::reader::TimePicker>,
    choice_list: Option<crate::reader::ChoiceList>,
    date_language: crate::datefield::Locale,
    focused_widget: Option<pulpit_render::document::protocol::FocusedWidget>,
    focused_hint: Option<String>,
    sheet_color: iced::Color,
}

impl From<&ReaderData<'_>> for PageSurface {
    fn from(reader: &ReaderData<'_>) -> Self {
        Self {
            pages: reader.visible.clone(),
            column_height: reader.column.height,
            column_width: reader.column.width,
            pointer: Pointer {
                armed: reader.controls.tool,
                marqueeing: reader.controls.crop.takes_the_pointer(),
                panning: reader.panning,
            },
            composing: reader.composing.clone(),
            date_picker: reader.date_picker.cloned(),
            time_picker: reader.time_picker.cloned(),
            choice_list: reader.choice_list.cloned(),
            date_language: reader.date_language,
            focused_widget: reader.focused_widget.cloned(),
            focused_hint: reader.focused_hint.map(str::to_owned),
            sheet_color: reader.sheet_color,
        }
    }
}

impl PageSurface {
    fn sheets<'a, Message: Clone + 'static>(
        &self,
        compose: Option<&'a iced::widget::text_editor::Content>,
        mode: Mode,
        on_event: fn(WidgetEvent) -> Message,
    ) -> Column<'a, Message> {
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
        if let Some(first) = self.pages.first() {
            if first.placed.top > 0.0 {
                sheets = sheets.push(space::vertical().height(Length::Fixed(first.placed.top)));
            }
        }
        // Pages that share a top are one row, which in a two-page spread is a
        // pair of facing sheets: the column decided that, and this only has to
        // draw what it decided.
        let mut rows: Vec<Vec<&crate::widgets::context::ReaderPage>> = Vec::new();
        for page in &self.pages {
            match rows.last_mut() {
                Some(row)
                    if (row[0].placed.top - page.placed.top).abs() < f32::EPSILON.max(0.01) =>
                {
                    row.push(page)
                }
                _ => rows.push(vec![page]),
            }
        }
        for (index, pages) in rows.iter().enumerate() {
            if index > 0 {
                sheets =
                    sheets.push(space::vertical().height(Length::Fixed(super::model::PAGE_GAP)));
            }
            // Aligned to the top, not stretched: two facing pages of unequal
            // height stand on the same line as they would on a desk.
            let mut facing = row![]
                .spacing(super::model::PAGE_GAP)
                .align_y(Alignment::Start);
            for page in pages {
                // The half-written mark goes to the sheet it was placed on, and to
                // no other: the editor is drawn where the mark will land.
                let composing = self
                    .composing
                    .as_ref()
                    .filter(|composing| composing.page == page.placed.page);
                facing = facing.push(sheet(
                    page,
                    mode,
                    self.pointer,
                    composing,
                    compose,
                    self.date_picker.as_ref(),
                    self.time_picker.as_ref(),
                    self.choice_list.as_ref(),
                    self.date_language,
                    self.focused_widget.as_ref(),
                    self.focused_hint.as_deref(),
                    self.sheet_color,
                    on_event,
                ));
            }
            sheets = sheets.push(facing);
        }
        // …and a trailing one stands for every page below it, which is what makes
        // the scroll bar the whole document's rather than the window's: without
        // it the content ends at the last built sheet and the reader cannot get
        // past the pages they can already see.
        if let Some(last) = self.pages.last() {
            // The *row's* bottom: two facing pages need not be the same height,
            // and a spacer measured from the shorter one would make the content
            // shorter than the column says the document is.
            let rest = self.column_height - last.placed.row_bottom;
            if rest > 0.0 {
                sheets = sheets.push(space::vertical().height(Length::Fixed(rest)));
            }
        }
        sheets
    }
}

fn page_surface_width(column_width: f32, viewport_width: f32) -> f32 {
    column_width.max(viewport_width).max(0.0)
}

#[cfg(test)]
mod layout_tests {
    use super::{
        bookmark_row_geometry, navigation_is_compact, page_surface_width, tools_are_compact,
        OUTLINE_COMPACT_ROW_HEIGHT,
    };

    #[test]
    fn a_fitted_document_tracks_the_live_viewport_width() {
        let document = 600.0;

        assert_eq!(page_surface_width(document, 720.0), 720.0);
        assert_eq!(page_surface_width(document, 960.0), 960.0);
    }

    #[test]
    fn a_document_wider_than_the_viewport_keeps_its_scrollable_width() {
        assert_eq!(page_surface_width(1_200.0, 960.0), 1_200.0);
    }

    #[test]
    fn long_bookmarks_gain_lines_only_when_the_rail_needs_them() {
        let title = "A long chapter title whose words need room to remain readable";
        let (_, narrow) = bookmark_row_geometry(title, 2, 170.0, false);
        let (_, wide) = bookmark_row_geometry(title, 2, 420.0, false);
        assert!(narrow > wide);
        assert!(wide >= OUTLINE_COMPACT_ROW_HEIGHT);
    }

    #[test]
    fn narrow_deep_outlines_preserve_more_room_for_words() {
        let (narrow_indent, _) = bookmark_row_geometry("Methods", 6, 170.0, false);
        let (wide_indent, _) = bookmark_row_geometry("Methods", 6, 420.0, false);
        assert!(narrow_indent < wide_indent);
        assert!(narrow_indent <= 170.0 * 0.32);
    }

    #[test]
    fn reader_bands_use_overflow_at_narrow_window_widths() {
        // The built-in Reader gives each run 47.5% of the band, less the
        // menu cell and gutters. These representative logical viewport
        // widths cover the supported minimum through an ordinary desktop.
        for viewport in [480.0_f32, 640.0, 720.0, 1008.0] {
            let cell = viewport * 0.475;
            assert!(navigation_is_compact(cell), "navigation at {viewport}");
            assert!(tools_are_compact(cell), "tools at {viewport}");
        }
        let desktop_cell = 1280.0 * 0.475;
        assert!(!navigation_is_compact(desktop_cell));
        // The palette is two tools longer than it was — a shape tool and the
        // stamp — and two tools no longer fit in an ordinary desktop's half
        // band. The band gives way to the armed tool and More there, which is
        // what compact mode is for; a band drawn full into a cell too narrow
        // for it would be a row of clipped controls.
        assert!(tools_are_compact(desktop_cell));
        let wide_cell = 1600.0 * 0.475;
        assert!(!tools_are_compact(wide_cell));
        // …and the threshold follows the palette rather than a number left
        // behind by it: the whole point of deriving it is that the next tool
        // added cannot silently overflow the band.
        assert!(
            tools_are_compact(590.0),
            "a palette longer than seven tools needs more than the width \
             seven tools were measured at"
        );
    }
}

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
    /// Is the marquee crop armed? It outranks the armed tool: while it is
    /// on, no tool and no link can have the press.
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
    time: Option<&crate::reader::TimePicker>,
    choice: Option<&crate::reader::ChoiceList>,
    language: crate::datefield::Locale,
    focused: Option<&pulpit_render::document::protocol::FocusedWidget>,
    focused_hint: Option<&str>,
    sheet_color: iced::Color,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'a, Message> {
    // The frame is always an upright raster; the sheet turns the picture to
    // match the view rotation. `Rotation::Solid` grows the layout to the
    // turned size, which is exactly the placed size the column reserved.
    let upright = if page.rotation.swaps_axes() {
        (page.placed.height, page.placed.width)
    } else {
        (page.placed.width, page.placed.height)
    };
    let inner: Element<'static, Message> = match &page.frame {
        Some(handle) => image(handle.clone())
            .width(Length::Fixed(upright.0))
            .height(Length::Fixed(upright.1))
            .rotation(iced::Rotation::Solid(iced::Radians::from(iced::Degrees(
                page.rotation.degrees() as f32,
            ))))
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
        .style(move |_theme| container::Style {
            // The paper-white sheet token, through the colour mode: a page
            // still rendering matches the pages around it instead of
            // flashing white into an inverted read.
            background: Some(iced::Background::Color(sheet_color)),
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
        || !page.dead_fields.is_empty()
        || page.patch.is_some()
    {
        let mut layers = iced::widget::stack![sheet];
        // Directly on the picture, under everything pulpit draws: the patch
        // *is* the page, for the rectangle it covers — the renderer's own
        // pixels for a part the frame predates (§9.4). A focus ring, a badge
        // or a mark still goes over it, exactly as over the frame.
        if let Some(patch) = &page.patch {
            layers = layers.push(patch_layer(patch, page.rotation, shown, origin, drawn));
        }
        // Under everything the reader made: a badge is a statement about the
        // file, and nothing that says what the document *is* may cover what
        // somebody wrote on it.
        if !page.dead_fields.is_empty() {
            // A tool armed to mark the page, the marquee, or a pan grab
            // must keep the press even when it starts on a signature
            // field's rect — the field only answers a click when nothing
            // else is claiming the pointer (see the cursor precedence rule
            // below, which this mirrors).
            let tool_owns_pointer =
                pointer.marqueeing || pointer.armed.is_some() || pointer.panning;
            layers = layers.push(super::preview::dead_field_layer(
                page.dead_fields.clone(),
                theme::ambient::muted(),
                shown,
                origin,
                drawn,
                (mode.interactive() && !tool_owns_pointer).then_some(on_event),
            ));
        }
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
                    // A found word is washed, never ruled: it is a place on
                    // the page, not a mark somebody made on it.
                    markup: pulpit_core::annotation::MarkupKind::Highlight,
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
    // The composing mark and the date picker arrive in the page's upright
    // canonical space — they come from the application and the worker, not
    // from this facet — so their anchors are turned here to match everything
    // else drawn on the sheet.
    let canonical_upright = if page.rotation.swaps_axes() {
        (page.canonical.1, page.canonical.0)
    } else {
        page.canonical
    };
    let writing = composing.cloned().map(|mut composing| {
        composing.at =
            page.rotation
                .rotate_point(composing.at, canonical_upright.0, canonical_upright.1);
        compose_layer(
            &composing,
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
            AnnotationTool::Highlighter | AnnotationTool::SelectText => {
                iced::mouse::Interaction::Text
            }
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
        .cloned()
        .map(|mut picker| {
            picker.bounds =
                page.rotation
                    .rotate_rect(picker.bounds, canonical_upright.0, canonical_upright.1);
            date_picker_layer(&picker, language, shown, origin, drawn, on_event)
        });

    // The ring around the field with the focus, and what that field expects.
    // Both are pulpit's own chrome, drawn from the widget's page rectangle at
    // this sheet's current geometry — so they follow a scroll, a zoom and a
    // rotation, and they outlive the frame swap that throws PDFium's own
    // decoration away.
    let focused = focused.filter(|widget| widget.page == index);
    let ring = focused.map(|widget| {
        let bounds =
            page.rotation
                .rotate_rect(widget.bounds, canonical_upright.0, canonical_upright.1);
        focus_ring_layer::<Message>(
            super::model::Anchor::of(bounds, shown, origin, drawn),
            drawn,
        )
    });
    let hint = focused.zip(focused_hint).map(|(widget, hint)| {
        let bounds =
            page.rotation
                .rotate_rect(widget.bounds, canonical_upright.0, canonical_upright.1);
        field_hint_layer::<Message>(
            super::model::Anchor::of(bounds, shown, origin, drawn),
            hint,
            drawn,
        )
    });

    // The option list over a choice field, on the page that field is on, and
    // over everything else on the sheet: it is the thing the reader is
    // pointing at. Its anchor turns with the page like every other overlay's.
    let options = choice
        .filter(|choice| choice.page == index)
        .cloned()
        .map(|mut choice| {
            choice.bounds =
                page.rotation
                    .rotate_rect(choice.bounds, canonical_upright.0, canonical_upright.1);
            choice_list_layer(&choice, shown, origin, drawn, on_event)
        });

    // The hour and minute steppers over a time field, placed exactly as the
    // calendar is and for the same reason.
    let clock = time
        .filter(|picker| picker.page == index)
        .cloned()
        .map(|mut picker| {
            picker.bounds =
                page.rotation
                    .rotate_rect(picker.bounds, canonical_upright.0, canonical_upright.1);
            time_picker_layer(&picker, language, shown, origin, drawn, on_event)
        });

    let overlays = [writing, calendar, clock, ring, hint, options];
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

/// The rectangle an edit changed, drawn over the page's frame (§9.4).
///
/// A layer rather than a blend into the frame: a keystroke invalidates a few
/// hundred pixels, and compositing meant cloning and re-uploading a
/// multi-megabyte page per character. Here the frame is never touched while
/// somebody types, which is what "the frame is never worse than it was" (A2)
/// asks for, and the patch raster is small enough to upload synchronously.
///
/// **Rotation.** The constraint is that the patch must land on the same part
/// of the page as the pixels it replaces, at every view rotation. Both the
/// frame and the patch are rasterised *upright*, and the sheet turns each of
/// them with `Rotation::Solid`, which grows a widget's layout to its turned
/// size. So the placement is computed in the *rotated* page space — the
/// facet has already turned `bounds` into it, exactly as it turns every other
/// rectangle — and the image is given its *upright* content size, which the
/// rotation then turns back into the box computed here. Under a quarter turn
/// the anchor's width is the raster's height and vice versa; that swap is the
/// whole of the arithmetic, and getting it backwards puts the patch on the
/// page transposed rather than merely misplaced.
///
/// The placement is snapped to the device pixel grid. A patch whose edge falls
/// on a fraction of a pixel is resampled there, and the seam against the frame
/// underneath is visible as a hairline; the two points of margin the request
/// already carries mean a snap of up to half a pixel cannot uncover anything
/// the patch was meant to cover.
fn patch_layer<Message: 'static>(
    patch: &crate::widgets::context::PagePatch,
    rotation: pulpit_core::page::PageRotation,
    shown: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
) -> Element<'static, Message> {
    let anchor = super::model::Anchor::of(patch.bounds, shown, origin, drawn);
    let scale = if patch.device_scale.is_finite() && patch.device_scale > 0.0 {
        patch.device_scale
    } else {
        1.0
    };
    let snap = |value: f32| (value * scale).round() / scale;
    let (left, top) = (snap(anchor.left), snap(anchor.top));
    // Snap the far edge, not the size, so a rounded origin cannot push the
    // far edge off the pixel it was meant to land on.
    let (width, height) = (
        (snap(anchor.left + anchor.width) - left).max(0.0),
        (snap(anchor.top + anchor.height) - top).max(0.0),
    );
    if width <= 0.0 || height <= 0.0 {
        return space::horizontal()
            .width(Length::Fixed(0.0))
            .height(Length::Fixed(0.0))
            .into();
    }
    // The upright size of the raster: the turn exchanges the axes back.
    let upright = if rotation.swaps_axes() {
        (height, width)
    } else {
        (width, height)
    };
    let raster = image(patch.image.clone())
        .width(Length::Fixed(upright.0))
        .height(Length::Fixed(upright.1))
        // Fill, not the default contain: the raster's pixel aspect and the
        // box's differ by the rounding in both, and contain would letterbox
        // that difference into a sliver of frame showing through at an edge.
        .content_fit(iced::ContentFit::Fill)
        .rotation(iced::Rotation::Solid(iced::Radians::from(iced::Degrees(
            rotation.degrees() as f32,
        ))));
    placed_over_the_sheet(raster, (left, top), drawn)
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

/// The list pulpit draws over a non-editable choice field (§8.6).
///
/// PDFium draws one of its own into the page bitmap when a combo box is
/// clicked. That list never reaches a saved file — it is viewer chrome — and
/// compositing it out of the slivers the engine reports as invalidated, one
/// round trip per hovered row, is the worst client the partial-repaint path
/// has. So the press is answered with focus alone and the list is drawn here.
/// What it produces is an *index*, handed to `FORM_SetIndexSelected`: the
/// engine still performs the selection and still generates the appearance, so
/// there is one implementation of the committed value and it is PDFium's.
///
/// Placed with spacers against the widget's own rectangle, exactly as the
/// calendar is, so it opens against the field rather than in a dialog over the
/// form being filled in.
fn choice_list_layer<Message: Clone + 'static>(
    choice: &crate::reader::ChoiceList,
    shown: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };
    /// Wide enough for an ordinary option, and never narrower than the field.
    const MIN_WIDTH: f32 = 160.0;
    /// As tall as it needs to be, up to this: a hundred-option list scrolls
    /// rather than covering the page it belongs to.
    const MAX_HEIGHT: f32 = 220.0;
    /// What one row occupies, used only to guess the panel's height for
    /// placement. The rows lay themselves out.
    const ROW: f32 = 24.0;

    let (scale_x, scale_y) = super::page_to_screen(shown, drawn);

    let width = ((choice.bounds.right - choice.bounds.left) * scale_x).max(MIN_WIDTH);
    let height = (choice.options.len() as f32 * ROW + theme::space::XS * 2.0).min(MAX_HEIGHT);

    // Under the field by preference, which is where a dropdown goes; above it
    // when there is no room below, so a field near the foot of the page still
    // opens a list the reader can see all of.
    let left = ((choice.bounds.left - origin.0) * scale_x).clamp(0.0, (drawn.0 - width).max(0.0));
    let below = (choice.bounds.bottom - origin.1) * scale_y;
    let above = (choice.bounds.top - origin.1) * scale_y - height;
    let top = if below + height <= drawn.1 || above < 0.0 {
        below.clamp(0.0, (drawn.1 - height).max(0.0))
    } else {
        above.max(0.0)
    };

    let mut rows = Column::new().spacing(1.0);
    for (index, option) in choice.options.iter().enumerate() {
        let index = index as u32;
        // Two things are marked and they are not the same thing: what the
        // field holds, and where the arrow keys are. Nothing is committed by
        // moving the highlight, so a list whose highlight looked like a
        // selection would say the value had already changed.
        let chosen = choice.is_selected(index);
        let style = if chosen || index == choice.highlighted {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        };
        // A multi-select list box says what it is by showing a box per row.
        // The style alone cannot: it is already carrying the highlight, and a
        // reader cannot be asked to tell "the arrow keys are here" from "this
        // one is chosen" by shade. The single-select list keeps no marks —
        // there is only ever one chosen row and closing the list is the
        // acknowledgement.
        let label = if choice.multiple {
            format!("{} {option}", if chosen { "[x]" } else { "[ ]" })
        } else {
            option.clone()
        };
        let command = if choice.multiple {
            ReadCommand::ToggleOption(index)
        } else {
            ReadCommand::PickOption(index)
        };
        rows = rows.push(
            button(text(label).size(theme::type_scale::LABEL))
                .width(Length::Fill)
                .padding(theme::space::XS)
                .style(style)
                .on_press(send(command)),
        );
    }

    let panel = container(
        column![
            iced::widget::scrollable(rows)
                .height(Length::Shrink)
                .style(theme::ambient::scrollbar),
            // "Done" rather than "Close" for a multi-select list: every tick
            // was already committed on its own, so there is nothing left to
            // confirm and nothing a "Cancel" could take back.
            button(
                text(if choice.multiple { "Done" } else { "Close" })
                    .size(theme::type_scale::CAPTION),
            )
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button)
            .on_press(send(ReadCommand::CloseChoiceList)),
        ]
        .spacing(theme::space::XS),
    )
    .padding(theme::space::XS)
    .max_height(MAX_HEIGHT)
    .width(Length::Fixed(width))
    .style(theme::ambient::surface);

    placed_over_the_sheet(panel, (left, top), drawn)
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

    let (scale_x, scale_y) = super::page_to_screen(shown, drawn);

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

    let step = |glyph: Icon, forward: bool| {
        button(theme::icon::icon(glyph, theme::type_scale::BODY))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button)
            .on_press(send(ReadCommand::StepDatePicker(forward)))
    };
    let header = row![
        step(Icon::ChevronLeft, false),
        container(
            text(picker.month.title(language))
                .size(theme::type_scale::LABEL)
                .align_x(Alignment::Center)
        )
        .width(Length::Fill)
        .align_y(Alignment::Center),
        step(Icon::ChevronRight, true),
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

    placed_over_the_sheet(panel, (left, top), drawn)
}

/// The hour and minute steppers pulpit draws over a time field (§8.6).
///
/// The calendar's counterpart, one format category along. A PDF says a field
/// holds a time and says its shape — `h:MM tt` — and offers nothing to enter
/// one with, so a viewer that wants a helper draws one. What comes out is
/// *text*, written the way the field's own pattern asks for and handed to
/// PDFium's editor like any typed value, so §8.6's one editing surface
/// survives the helper sitting on top of it.
///
/// Steppers rather than a clock face or a text box: the field itself is
/// already a text box, and what it lacks is a way to nudge a value without
/// knowing which of the world's time notations this document wants.
fn time_picker_layer<Message: Clone + 'static>(
    picker: &crate::reader::TimePicker,
    language: crate::datefield::Locale,
    shown: (f32, f32),
    origin: (f32, f32),
    drawn: (f32, f32),
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };
    /// Wide enough for two two-digit columns, a marker and the buttons under
    /// them; tall enough for the steppers and the row that accepts.
    const WIDTH: f32 = 176.0;
    const HEIGHT: f32 = 132.0;
    const COLUMN: f32 = 44.0;

    let (left, top) = super::model::Anchor::of(picker.bounds, shown, origin, drawn)
        .place_beside((WIDTH, HEIGHT), drawn);

    let stepper = move |value: String, minutes: i32| {
        column![
            button(theme::icon::icon(
                Icon::ChevronUp,
                theme::type_scale::CAPTION
            ))
            .width(Length::Fixed(COLUMN))
            .padding(2.0)
            .style(theme::ambient::tool_button)
            .on_press(send(ReadCommand::StepTimePicker(minutes))),
            container(
                text(value)
                    .size(theme::type_scale::LABEL)
                    .align_x(Alignment::Center)
            )
            .width(Length::Fixed(COLUMN))
            .align_y(Alignment::Center),
            button(theme::icon::icon(
                Icon::ChevronDown,
                theme::type_scale::CAPTION
            ))
            .width(Length::Fixed(COLUMN))
            .padding(2.0)
            .style(theme::ambient::tool_button)
            .on_press(send(ReadCommand::StepTimePicker(-minutes))),
        ]
        .align_x(Alignment::Center)
        .spacing(1.0)
    };

    let hour = if picker.twelve_hour() {
        picker.time.hour_on_the_clock().to_string()
    } else {
        format!("{:02}", picker.time.hour)
    };
    let mut dials = row![
        stepper(hour, 60),
        container(text(":").size(theme::type_scale::LABEL)).align_y(Alignment::Center),
        stepper(format!("{:02}", picker.time.minute), 1),
    ]
    .spacing(theme::space::XS)
    .align_y(Alignment::Center);
    // Half a day, which is exactly what an am/pm toggle is. Offered only when
    // the pattern carries the marker: a 24-hour field showing one would be
    // pulpit inventing a distinction the document does not draw.
    if picker.shows_meridiem() {
        let marker = language.meridiem(picker.time.afternoon());
        dials = dials.push(
            button(
                text(if marker.is_empty() {
                    if picker.time.afternoon() {
                        "PM".to_string()
                    } else {
                        "AM".to_string()
                    }
                } else {
                    marker
                })
                .size(theme::type_scale::CAPTION)
                .align_x(Alignment::Center),
            )
            .width(Length::Fixed(COLUMN))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button)
            .on_press(send(ReadCommand::StepTimePicker(12 * 60))),
        );
    }

    let panel = container(
        column![
            dials,
            row![
                button(text("Set").size(theme::type_scale::CAPTION))
                    .padding(theme::space::XS)
                    .style(theme::ambient::selected_button)
                    .on_press(send(ReadCommand::PickTime)),
                button(text("Close").size(theme::type_scale::CAPTION))
                    .padding(theme::space::XS)
                    .style(theme::ambient::tool_button)
                    .on_press(send(ReadCommand::CloseTimePicker)),
            ]
            .spacing(theme::space::XS),
        ]
        .spacing(theme::space::XS)
        .align_x(Alignment::Center),
    )
    .padding(theme::space::XS)
    .width(Length::Fixed(WIDTH))
    .style(theme::ambient::surface);

    placed_over_the_sheet(panel, (left, top), drawn)
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
    /// How wide the Typst source box is on screen, in layout points.
    const EDITOR_WIDTH: f32 = 260.0;

    let (scale_x, scale_y) = super::page_to_screen(canonical, drawn);
    let (width, height, font_size, padding, line_height) = if composing.typst {
        // Typst source is code, not the mark: it is typed at the UI's own
        // size, and the picture it compiles to is measured by Typst. The box
        // grows with what has been written, up to a point: past a handful of
        // lines a box that kept growing would cover the page it is a comment
        // on. After that the editor scrolls, as it should.
        let lines = buffer.map_or(1, |buffer| buffer.line_count()).clamp(1, 6);
        let height = (COMPOSE_HEIGHT + (lines - 1) as f32 * theme::type_scale::BODY * 1.3)
            .min(drawn.1.max(COMPOSE_HEIGHT));
        (
            EDITOR_WIDTH.min(drawn.0.max(0.0)),
            height,
            theme::type_scale::BODY,
            theme::space::XS,
            1.3,
        )
    } else {
        // The words are composed at the size and the leading the mark will be
        // set at, scaled to the page's zoom, in a box measured the way the
        // commit measures it — so what is typed is the size it will be on the
        // sheet, and the mark does not jump when it is placed (§8.5). An
        // empty box opens a few ems wide so there is somewhere to aim the
        // first character; from then on it tracks the measurement. The box is
        // bounded by the sheet rather than by a line count, and scrolls past
        // that, exactly as the committed mark is clipped to the page.
        let font_px = composing.font_size * scale_y;
        let padding = (font_px * 0.3).clamp(4.0, 24.0);
        let (width_pt, height_pt) =
            pulpit_core::annotate::text_box::fit(&composing.text, composing.font_size);
        (
            (width_pt.max(composing.font_size * 3.0) * scale_x + padding * 2.0)
                .min(drawn.0.max(1.0)),
            (height_pt * scale_y + padding * 2.0).min(drawn.1.max(1.0)),
            font_px,
            padding,
            pulpit_core::annotate::text_box::LEADING,
        )
    };
    let left = ((composing.at.x - origin.0) * scale_x).clamp(0.0, (drawn.0 - width).max(0.0));
    let top = ((composing.at.y - origin.1) * scale_y).clamp(0.0, (drawn.1 - height).max(0.0));

    // A real multi-line editor, so a note can be a paragraph. Return breaks
    // the line, the way it does in every other box that holds prose; the mark
    // is placed with the tick, with Ctrl+Return, or by pressing Escape's
    // opposite — never by the key that is also how a second line is written.
    let editor: Element<'a, Message> = match buffer {
        Some(buffer) => text_editor(buffer)
            .id(compose_input_id())
            .size(font_size)
            .line_height(iced::widget::text::LineHeight::Relative(line_height))
            .height(Length::Fixed(height))
            .padding(padding)
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
        button(theme::icon::icon(Icon::Check, theme::type_scale::BODY))
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
            button(text("Typst").size(theme::type_scale::LABEL))
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
        button(theme::icon::icon(Icon::Close, theme::type_scale::BODY))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button)
            .on_press(on_event(WidgetEvent::Read(ReadCommand::CancelMark))),
    );

    placed_over_the_sheet(controls, (left, top), drawn)
}

/// How tall the writing box is with one line in it. It grows from here as
/// lines are written, and this is the floor used to keep it on the sheet.
const COMPOSE_HEIGHT: f32 = 34.0;

/// The writing box's input, so the application can put the caret in it the
/// moment a spot is chosen. One mark is written at a time, so one id.
pub fn compose_input_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-reader-compose-mark")
}

/// The crop latch, and the arrow that says what a drawn rectangle will mean.
///
/// A latch rather than a momentary press, because what it does is not over
/// when the press is: while it is down the pointer draws rectangles instead of
/// reaching the page, and after a crop is taken it is what puts the margins
/// back. One press on, one press off.
///
/// The rectangle's meaning — a zoom into it, or a crop on every page — hangs
/// off an options arrow exactly like the shape tool's shape or the
/// highlighter's mark, chosen *before* the drag: asking afterwards hung a
/// dialog over the very rectangle the answer was about, and made every crop a
/// draw-then-answer two-step.
fn crop_control<Message: Clone + 'static>(
    crop: CropState,
    choice: CropChoice,
    options_open: bool,
    live: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };
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
    let control = hint(control, crop.label());

    let mut arrow = button(theme::icon::icon(theme::Icon::ChevronDown, 9.0))
        .padding(0)
        .width(Length::Fixed(12.0))
        .height(Length::Fixed(12.0))
        .style(theme::ambient::tool_button);
    if live {
        arrow = arrow.on_press(send(ReadCommand::CropOptions(!options_open)));
    }
    let trigger: Element<'static, Message> = row![
        control,
        container(arrow)
            .height(Length::Fixed(theme::type_scale::HEADING + 8.0))
            .align_y(Alignment::End),
    ]
    .spacing(0)
    .align_y(Alignment::Center)
    .into();

    let panel = options_open.then(|| {
        // Icons alone, as the reader's bands are: the glyph carries the
        // difference — a magnifier, the crop frame, the scan brackets — and
        // the hint under the resting pointer says what one press will do,
        // which is more than a short label ever managed.
        let item = |icon: theme::Icon,
                    tip: &str,
                    selected: bool,
                    command: ReadCommand|
         -> Element<'static, Message> {
            let mut control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
                .padding(Padding::from([4.0, 8.0]))
                .style(if selected {
                    theme::ambient::selected_button
                } else {
                    theme::ambient::tool_button
                });
            if live {
                control = control.on_press(send(command));
            }
            hint(control, tip)
        };
        let mut options = Row::new()
            .spacing(theme::space::XS)
            .align_y(Alignment::Center);
        for meaning in CropChoice::ALL {
            let (icon, tip) = match meaning {
                CropChoice::Zoom => (theme::Icon::ZoomIn, "A drawn rectangle zooms, once"),
                CropChoice::Pages => (theme::Icon::Crop, "A drawn rectangle crops every page"),
            };
            options = options.push(item(
                icon,
                tip,
                choice == meaning,
                ReadCommand::SetCropChoice(meaning),
            ));
        }
        // The third way a crop happens: measured rather than drawn. One
        // press, no rectangle — the margins are read off the deck's own
        // pictures and every page is cropped to the ink.
        options = options.push(item(
            theme::Icon::ScanText,
            "Measure the margins and crop them away",
            false,
            ReadCommand::AutoCrop,
        ));
        crate::widgets::common::options::Options::new("Crop")
            .on_close(live.then(|| send(ReadCommand::CropOptions(false))))
            .bare_row(options)
            .into()
    });
    crate::widgets::common::popover::Popover::new(trigger, panel)
        .on_dismiss(send(ReadCommand::CropOptions(false)))
        .into()
}

/// The navigation band: where you are, and how big the page is.
fn navigation<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let state = NavigationState {
        live: mode.interactive() && reader.open,
        page_value: reader
            .page_entry
            .clone()
            .unwrap_or_else(|| reader.page_label()),
        page_total: reader.page_total(),
        can_go_back: reader.controls.page.get() > 0 || reader.can_go_back,
        can_go_forward: reader.controls.page.get() + 1 < reader.page_count || reader.can_go_forward,
        zoom_label: format!("{}%", (reader.scale * 100.0).round() as i32),
        spread: reader.controls.spread,
        crop: reader.controls.crop,
        crop_choice: reader.controls.crop_choice,
        crop_options: reader.controls.crop_options,
        overflow_open: reader.controls.navigation_overflow,
    };
    responsive(move |size| navigation_band(state.clone(), size.width, on_event)).into()
}

#[derive(Clone)]
struct NavigationState {
    live: bool,
    page_value: String,
    page_total: String,
    can_go_back: bool,
    can_go_forward: bool,
    zoom_label: String,
    spread: PageSpread,
    crop: CropState,
    crop_choice: CropChoice,
    crop_options: bool,
    overflow_open: bool,
}

fn navigation_band<Message: Clone + 'static>(
    state: NavigationState,
    width: f32,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let send = move |command: ReadCommand| on_event(WidgetEvent::Read(command));
    let step = |icon, label, command, enabled| {
        navigation_button(icon, label, command, state.live && enabled, on_event)
    };

    let mut current = text_input("Page", &state.page_value)
        .size(theme::type_scale::LABEL)
        .style(theme::ambient::text_field)
        .width(Length::Fixed(48.0))
        .padding(Padding::from([4.0, 6.0]));
    if state.live {
        current = current
            .on_input(move |typed| send(ReadCommand::TypePage(typed)))
            .on_submit(send(ReadCommand::CommitPage));
    }
    let mut entry = Row::new().push(current);
    if width >= 230.0 {
        entry = entry.push(
            text(state.page_total.clone())
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::muted()),
        );
    }
    let pages = row![
        step(
            theme::Icon::ChevronLeft,
            "Back",
            ReadCommand::HistoryBack,
            state.can_go_back,
        ),
        entry.spacing(theme::space::XS).align_y(Alignment::Center),
        step(
            theme::Icon::ChevronRight,
            "Forward",
            ReadCommand::HistoryForward,
            state.can_go_forward,
        ),
    ]
    .spacing(theme::space::XS)
    .align_y(Alignment::Center);

    if navigation_is_compact(width) {
        let mut band = Row::new()
            .push(pages)
            .push(space::horizontal().width(Length::Fill))
            .spacing(theme::space::XS)
            .align_y(Alignment::Center);
        // Once armed, Crop stays visible — like the tools band keeping its
        // armed tool on the row — so the latch (and its options arrow) never
        // disappears into a closed menu while the pointer belongs to it.
        if state.crop.is_on() {
            band = band.push(crop_control(
                state.crop,
                state.crop_choice,
                state.crop_options,
                state.live,
                on_event,
            ));
        }
        band = band.push(navigation_overflow(&state, on_event));
        return container(band)
            .width(Length::Fill)
            .align_y(Alignment::Center)
            .into();
    }

    let sizing = row![
        step(theme::Icon::ZoomOut, "Zoom out", ReadCommand::ZoomOut, true),
        text(state.zoom_label.clone())
            .size(theme::type_scale::LABEL)
            .color(theme::ambient::muted()),
        step(theme::Icon::ZoomIn, "Zoom in", ReadCommand::ZoomIn, true),
        step(
            theme::Icon::FitWidth,
            "Fit width",
            ReadCommand::SetZoom(Zoom::FitWidth),
            true,
        ),
        step(
            theme::Icon::FitHeight,
            "Fit height",
            ReadCommand::SetZoom(Zoom::FitHeight),
            true,
        ),
        step(
            theme::Icon::FitPage,
            "Fit page",
            ReadCommand::SetZoom(Zoom::FitPage),
            true,
        ),
        crop_control(
            state.crop,
            state.crop_choice,
            state.crop_options,
            state.live,
            on_event
        ),
        step(
            match state.spread.other() {
                PageSpread::Single => theme::Icon::SinglePage,
                PageSpread::Double => theme::Icon::TwoPages,
            },
            state.spread.other().label(),
            ReadCommand::SetSpread(state.spread.other()),
            true,
        ),
        step(
            theme::Icon::RotatePage,
            "Rotate 90° clockwise",
            ReadCommand::RotateView,
            true,
        ),
    ]
    .spacing(theme::space::XS)
    .align_y(Alignment::Center);
    let rule = container(space::horizontal())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(theme::space::M))
        .style(theme::ambient::separator);
    container(
        row![pages, rule, sizing]
            .spacing(theme::space::M)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .align_y(Alignment::Center)
    .into()
}

fn navigation_button<Message: Clone + 'static>(
    icon: theme::Icon,
    label: &'static str,
    command: ReadCommand,
    enabled: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
        .padding(Padding::from([4.0, 8.0]))
        .style(theme::ambient::tool_button);
    let control = if enabled {
        control.on_press(on_event(WidgetEvent::Read(command)))
    } else {
        control
    };
    hint(control, label)
}

/// The width the whole navigation run — pages, zoom, spread, rotation — is
/// drawn at. Below it the band collapses behind an overflow menu, so this is
/// also the width a hugging cell asks for ([`WidgetKind::hug_width`]).
pub const NAVIGATION_RUN_WIDTH: f32 = 560.0;

fn navigation_is_compact(width: f32) -> bool {
    width < NAVIGATION_RUN_WIDTH
}

/// The "…" that opens an overflow menu: an ellipsis, held selected while its
/// menu is open, pressable only while `press` carries a message. Both bands
/// that collapse behind a menu — navigation and the document's own tools —
/// draw this same button, so the two overflow triggers cannot drift apart.
fn overflow_trigger<Message: Clone + 'static>(
    open: bool,
    press: Option<Message>,
) -> Element<'static, Message> {
    button(theme::icon::icon(
        theme::Icon::Ellipsis,
        theme::type_scale::HEADING,
    ))
    .padding(Padding::from([4.0, 8.0]))
    .style(if open {
        theme::ambient::selected_button
    } else {
        theme::ambient::tool_button
    })
    .on_press_maybe(press)
    .into()
}

fn navigation_overflow<Message: Clone + 'static>(
    state: &NavigationState,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let trigger = overflow_trigger(
        state.overflow_open,
        state.live.then(|| {
            on_event(WidgetEvent::Read(ReadCommand::NavigationOverflow(
                !state.overflow_open,
            )))
        }),
    );
    let panel = state
        .overflow_open
        .then(|| navigation_overflow_menu(state, on_event));
    crate::widgets::common::popover::Popover::new(hint(trigger, "More navigation controls"), panel)
        .on_dismiss(on_event(WidgetEvent::Read(
            ReadCommand::NavigationOverflow(false),
        )))
        .into()
}

fn navigation_overflow_menu<Message: Clone + 'static>(
    state: &NavigationState,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let item = |icon, label: &'static str, command| {
        let control = button(
            row![
                theme::icon::icon(icon, theme::type_scale::BODY),
                text(label).size(theme::type_scale::CAPTION),
            ]
            .spacing(theme::space::S)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([4.0, theme::space::S]))
        .style(theme::ambient::tool_button);
        if state.live {
            control.on_press(on_event(WidgetEvent::Read(command)))
        } else {
            control
        }
    };
    let spread = state.spread.other();
    let mut menu = column![crate::widgets::common::view::panel_header(
        format!("Zoom — {}", state.zoom_label),
        state
            .live
            .then(|| on_event(WidgetEvent::Read(ReadCommand::NavigationOverflow(false)))),
    )]
    .spacing(theme::space::XS);
    for control in [
        item(theme::Icon::ZoomOut, "Zoom out", ReadCommand::ZoomOut),
        item(theme::Icon::ZoomIn, "Zoom in", ReadCommand::ZoomIn),
        item(
            theme::Icon::FitWidth,
            "Fit width",
            ReadCommand::SetZoom(Zoom::FitWidth),
        ),
        item(
            theme::Icon::FitHeight,
            "Fit height",
            ReadCommand::SetZoom(Zoom::FitHeight),
        ),
        item(
            theme::Icon::FitPage,
            "Fit page",
            ReadCommand::SetZoom(Zoom::FitPage),
        ),
        item(
            theme::Icon::Crop,
            state.crop.label(),
            ReadCommand::ArmCrop(!state.crop.is_on()),
        ),
        item(
            match spread {
                PageSpread::Single => theme::Icon::SinglePage,
                PageSpread::Double => theme::Icon::TwoPages,
            },
            spread.label(),
            ReadCommand::SetSpread(spread),
        ),
        item(
            theme::Icon::RotatePage,
            "Rotate 90° clockwise",
            ReadCommand::RotateView,
        ),
    ] {
        menu = menu.push(control);
    }
    container(menu)
        .width(Length::Fixed(220.0))
        .padding(theme::space::S)
        .style(theme::ambient::surface)
        .into()
}

/// The accent rule down the left edge of an outline row carrying the
/// keyboard focus, or a blank of the same width when it is not.
///
/// A spacer rather than an absent marker: three kinds of outline row (a
/// field, a page, a bookmark) all share this rail, and an absent element
/// would let the label creep sideways the moment focus reached the row
/// instead of only lighting up in place.
fn outline_focus_marker<Message: 'static>(focused: bool) -> Element<'static, Message> {
    if focused {
        container(space::vertical().width(3.0).height(Length::Fill))
            .style(theme::ambient::accent_rule)
            .into()
    } else {
        space::horizontal().width(3.0).into()
    }
}

/// The document's authored outline. Page numbers live in the navigation band,
/// so this rail does not duplicate them as another page list.
fn outline<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    use crate::widgets::document::model::OutlineItemId;

    let live = mode.interactive() && reader.open;
    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };

    let view = reader.controls.outline;

    // Search and outline occupy the same rail, so they begin with the same
    // title scale and inset. Every view is a first-class tab in the icon row
    // above the title; the second row of text tabs that used to sit under it
    // was a split inside the split, and it is gone.
    // The bookmarks view is the one the reader curates, so its title row
    // carries the one way to add to it: bookmark the page being shown.
    let title: Element<'static, Message> =
        if matches!(view, OutlineView::Bookmarks) && reader.annotatable() {
            let mut add = button(theme::icon::icon(
                theme::Icon::BookmarkPlus,
                theme::type_scale::BODY,
            ))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button);
            if live {
                add = add.on_press(send(ReadCommand::AddBookmark));
            }
            row![
                text(view.label())
                    .size(theme::type_scale::TITLE)
                    .width(Length::Fill),
                hint(add, "Bookmark this page"),
            ]
            .align_y(Alignment::Center)
            .into()
        } else {
            text(view.label()).size(theme::type_scale::TITLE).into()
        };
    let header = column![
        sidebar_tabs(
            match view {
                OutlineView::Bookmarks => SidebarTab::Outline,
                OutlineView::Fields => SidebarTab::Fields,
                OutlineView::Annotations => SidebarTab::Annotations,
            },
            reader.annotatable(),
            reader.has_form,
            live,
            on_event
        ),
        title,
    ]
    .spacing(theme::space::XS);

    let body: Element<'static, Message> = match view {
        OutlineView::Bookmarks if reader.outline.is_empty() => {
            // A document with no bookmarks is not a fault, and the rail says
            // so rather than sitting empty and looking broken.
            nothing("This document has no bookmarks.")
        }
        OutlineView::Bookmarks => {
            let entries = reader.outline.clone();
            let focus = reader.outline_focus.cloned();
            virtual_bookmark_outline(
                entries,
                focus,
                reader.outline_scroll,
                reader.outline_viewport.clone(),
                reader.outline_width.clone(),
                live,
                reader.annotatable(),
                reader.bookmark_edit.clone(),
                on_event,
            )
        }
        OutlineView::Fields if reader.fields.is_empty() => {
            nothing("This document has no form fields.")
        }
        OutlineView::Fields => {
            let fields = reader.fields.clone();
            let focus = reader.outline_focus.cloned();
            virtual_outline(
                fields.len(),
                OUTLINE_ROW_HEIGHT,
                reader.outline_scroll,
                reader.outline_viewport.clone(),
                on_event,
                move |index| {
                    let field = &fields[index];
                    let id = OutlineItemId::Field {
                        name: field.name.clone(),
                        source_ordinal: index,
                    };
                    // A choice field with several selections has no single value,
                    // so "filled" asks both questions rather than only the first.
                    let filled = !field.value.is_empty() || !field.selected.is_empty();
                    let wanted = field.required && !filled;
                    // The name as the file gives it. A field with no name is a
                    // field nothing can say anything about, and it is still listed:
                    // it is on the page either way.
                    let name = if field.name.is_empty() {
                        "(unnamed)".to_string()
                    } else {
                        field.name.clone()
                    };
                    let label = text(name)
                        .size(theme::type_scale::LABEL)
                        .width(Length::Fill)
                        .color(if wanted {
                            theme::ambient::alert()
                        } else {
                            theme::ambient::text()
                        });
                    // Two words at most, in the muted role: what kind of control
                    // it is, and whether anything is in it. The status is said in
                    // words rather than in a colour alone, because a colour is the
                    // one thing a reader cannot be asked to decode.
                    let kind = text(field.kind.label().to_string())
                        .size(theme::type_scale::LABEL)
                        .color(theme::ambient::muted());
                    let status = text(
                        if wanted {
                            "required"
                        } else if filled {
                            "filled"
                        } else {
                            "empty"
                        }
                        .to_string(),
                    )
                    .size(theme::type_scale::LABEL)
                    .color(if wanted {
                        theme::ambient::alert()
                    } else {
                        theme::ambient::muted()
                    });
                    let focused = focus.as_ref() == Some(&id);
                    let marker = outline_focus_marker(focused);
                    let control = button(
                        row![marker, label, kind, status]
                            .spacing(theme::space::XS)
                            .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .height(Length::Fixed(OUTLINE_ROW_HEIGHT - 2.0))
                    .padding(Padding::from([3.0, 6.0]))
                    .style(if focused {
                        theme::ambient::focus_button
                    } else {
                        theme::ambient::tool_button
                    });
                    // A field the producer placed nowhere has no page to go to, so
                    // the row says it exists and does not offer a jump it cannot
                    // make.
                    let target = field.widgets.first().map(|widget| widget.page);
                    let control = match (live, target) {
                        (true, Some(_)) => {
                            control.on_press(send(ReadCommand::ActivateOutlineItem(id.clone())))
                        }
                        _ => control,
                    };
                    container(control)
                        .id(outline_item_id(&id))
                        .width(Length::Fill)
                        .height(Length::Fixed(OUTLINE_ROW_HEIGHT))
                        .into()
                },
            )
        }
        OutlineView::Annotations => annotations(reader, live, on_event),
    };

    let panel = container(
        column![header, body]
            .spacing(theme::space::XS)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(theme::space::S)
    .width(Length::Fill)
    .height(Length::Fill);

    let keyboard_focus = reader.outline_focus.is_some();
    let renaming = reader.bookmark_edit.is_some();
    if !live {
        return panel.into();
    }
    crate::widgets::panel::on_key(panel, move |key, modifiers| {
        use iced::keyboard::key::Named;
        use iced::keyboard::Key;

        if modifiers.control() || modifiers.alt() || modifiers.logo() {
            return None;
        }
        // While a bookmark title is being typed, the keys spell the title.
        // Only Escape is taken — to close the field — because this handler
        // sees every key before the text input does, and an arrow or an
        // Enter consumed here would be one stolen from the typing hand.
        if renaming {
            return matches!(key, Key::Named(Named::Escape))
                .then(|| send(ReadCommand::CancelBookmarkTitle));
        }
        if !keyboard_focus {
            return matches!(key, Key::Named(Named::Tab)).then(|| {
                on_event(WidgetEvent::Panel(
                    crate::widgets::event::PanelCommand::FocusSidebar,
                ))
            });
        }
        match key {
            Key::Named(Named::ArrowUp) => Some(send(ReadCommand::MoveOutlineFocus(-1))),
            Key::Named(Named::ArrowDown) => Some(send(ReadCommand::MoveOutlineFocus(1))),
            Key::Named(Named::Enter) => Some(send(ReadCommand::ActivateFocusedOutlineItem)),
            Key::Named(Named::Escape | Named::Tab) => Some(on_event(WidgetEvent::Panel(
                crate::widgets::event::PanelCommand::FocusDocument,
            ))),
            _ => None,
        }
    })
}

/// Every mark the document carries, in page order (§8.4).
///
/// A list of what is *in the document*, not a second store of it (A1): the
/// rows are built from the engine's own answers, and a mark deleted from here
/// is deleted from the page in the same transaction any other deletion uses.
fn annotations<Message: Clone + 'static>(
    reader: &ReaderData<'_>,
    live: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    use crate::widgets::document::model::OutlineItemId;

    let send = move |command: ReadCommand| -> Message { on_event(WidgetEvent::Read(command)) };
    let rows = reader.annotations.clone();
    let (read, pages) = reader.annotation_scan;
    let scanning = read < pages;

    // Not reachable as the code stands — the tab is not offered for a
    // document pulpit cannot annotate, and opening one resets the rail to
    // bookmarks — but the view is a function of the state it is given, and
    // this is what this state means. It says which of the two silences it is
    // rather than sitting on "looking…" for a document nothing will ever
    // look through.
    if !reader.annotatable() {
        return nothing("This document cannot carry marks.");
    }

    if rows.is_empty() {
        // Two different answers, and the difference matters: one says the
        // document has nothing on it, the other says nobody has looked yet.
        return nothing(if scanning {
            "Looking through the document…"
        } else {
            "This document has no marks."
        });
    }

    // What the list is, in one line: how many marks, and — while the sweep is
    // still walking the document — how much of it has been read, so a list
    // that is still growing does not read as a list that is complete.
    let counted = if rows.len() == 1 {
        "1 mark".to_string()
    } else {
        format!("{} marks", rows.len())
    };
    let said = if scanning {
        format!("{counted} · {read} of {pages} pages read")
    } else {
        counted
    };
    let status = text(said)
        .size(theme::type_scale::LABEL)
        .color(theme::ambient::muted());

    let focus = reader.outline_focus.cloned();
    let list = virtual_outline(
        rows.len(),
        ANNOTATION_ROW_HEIGHT,
        reader.outline_scroll,
        reader.outline_viewport.clone(),
        on_event,
        move |index| {
            let mark = &rows[index];
            let id = OutlineItemId::Annotation(mark.id.clone());
            let focused = focus.as_ref() == Some(&id);

            // The mark's own colour down the left edge. Three highlights on
            // one page have the same kind and the same page and may have no
            // text at all; the colour is what tells them apart.
            let (red, green, blue) = mark.color.rgb();
            let swatch: Element<'static, Message> =
                container(space::vertical().width(3.0).height(Length::Fill))
                    .style(theme::ink_rule(iced::Color::from_rgb(red, green, blue)))
                    .into();

            // What it says, or what it is when it says nothing — an ink
            // stroke carries no text, and a blank row would read as a mark
            // that failed to load rather than as a line on a page.
            let says = text(mark.description())
                .size(theme::type_scale::LABEL)
                .width(Length::Fill)
                // One line, and never two: the row is a fixed height and the
                // line below this one says what kind the mark is and what
                // page it is on — a wrapped description would push that out
                // of the row and take the support note with it.
                .wrapping(iced::widget::text::Wrapping::None)
                .color(if mark.text.is_empty() {
                    theme::ambient::muted()
                } else {
                    theme::ambient::text()
                });
            let mut about = format!("{} · page {}", mark.kind.label(), mark.page.get() + 1);
            // How well pulpit understands it, in words (§10.1). A mark it
            // only preserves is said to be read-only rather than being left
            // to look like one of the reader's own.
            if let Some(note) = mark.support_note() {
                about.push_str(" · ");
                about.push_str(note);
            }
            let about = text(about)
                .size(theme::type_scale::CAPTION)
                .wrapping(iced::widget::text::Wrapping::None)
                .color(theme::ambient::muted());

            let open = button(column![says, about])
                .width(Length::Fill)
                .height(Length::Fixed(ANNOTATION_ROW_HEIGHT - 2.0))
                .padding(Padding::from([3.0, 6.0]))
                .style(if focused {
                    theme::ambient::focus_button
                } else {
                    theme::ambient::tool_button
                });
            let open = if live {
                open.on_press(send(ReadCommand::ActivateOutlineItem(id.clone())))
            } else {
                open
            };

            let mut controls = row![swatch, open]
                .spacing(theme::space::XS)
                .align_y(Alignment::Center);
            // Deleting is a rewrite, and pulpit does not rewrite what it does
            // not model (A5) — so a mark it only preserves gets no control
            // rather than one that would refuse.
            if mark.deletable() {
                let mut remove = button(theme::icon::icon(
                    theme::Icon::Trash,
                    theme::type_scale::LABEL,
                ))
                .padding(theme::space::XS)
                .style(theme::ambient::tool_button);
                if live {
                    remove = remove.on_press(send(ReadCommand::DeleteAnnotation(mark.id.clone())));
                }
                controls = controls.push(hint(remove, "Delete this mark"));
            }

            container(controls)
                .id(outline_item_id(&id))
                .width(Length::Fill)
                .height(Length::Fixed(ANNOTATION_ROW_HEIGHT))
                .into()
        },
    );

    column![status, list]
        .spacing(theme::space::XS)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Select the contents of the document's one shared sidebar.
///
/// `annotatable` is what the marks tab asks before it is drawn: a document
/// pulpit cannot annotate has no marks and never will, and a tab that is
/// always empty is a control that teaches the reader to ignore the rail —
/// the same rule the form tab is offered under.
pub fn sidebar_tabs<Message: Clone + 'static>(
    selected: SidebarTab,
    annotatable: bool,
    has_form: bool,
    live: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    use crate::widgets::event::PanelCommand;

    let tab = |icon, label, selected, command| {
        // These are the primary selectors for the rail, so give their glyphs
        // the same visual scale as the title naming the selected section.
        let mut control = button(theme::icon::icon(icon, theme::type_scale::TITLE))
            .padding(theme::space::XS)
            .style(if selected {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            });
        if live && !selected {
            control = control.on_press(on_event(WidgetEvent::Panel(command)));
        }
        hint(control, label)
    };

    // One row, one level: every way of looking at the document is a tab
    // here, never a tab within a tab. The outline's views used to be a
    // second row of text tabs under this one, which read as a split inside
    // the split.
    let mut tabs = row![tab(
        theme::Icon::Bookmark,
        "Bookmarks",
        selected == SidebarTab::Outline,
        PanelCommand::ShowView(OutlineView::Bookmarks)
    ),]
    .spacing(theme::space::XS);
    // Only where there is a form. A document with fields grows one more way
    // to look at itself; a deck of slides is left exactly as it was.
    if has_form {
        tabs = tabs.push(tab(
            theme::Icon::TextCursor,
            "Fields",
            selected == SidebarTab::Fields,
            PanelCommand::ShowView(OutlineView::Fields),
        ));
    }
    tabs = tabs.push(tab(
        theme::Icon::Search,
        "Search",
        selected == SidebarTab::Search,
        PanelCommand::ShowSearch,
    ));
    // Last, and reached by pressing it and by nothing else: the
    // marks are worth a tab in the rail a reader has already opened, and not
    // worth a key of their own (§8.4).
    if annotatable {
        tabs = tabs.push(tab(
            theme::Icon::StickyNote,
            "Annotations",
            selected == SidebarTab::Annotations,
            PanelCommand::ShowAnnotations,
        ));
    }

    let mut close = button(theme::icon::icon(
        theme::Icon::Close,
        theme::type_scale::TITLE,
    ))
    .padding(theme::space::XS)
    .style(theme::ambient::tool_button);
    if live {
        close = close.on_press(on_event(WidgetEvent::Panel(PanelCommand::CloseSidebar)));
    }

    row![tabs, space::horizontal(), hint(close, "Close sidebar")]
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .into()
}

/// A row of the annotations panel: what the mark says, and a line under it
/// saying what kind it is and where. Two lines rather than one because the
/// text is what a reader looks for and the kind is what they check.
pub const ANNOTATION_ROW_HEIGHT: f32 = 46.0;

const OUTLINE_LINE_HEIGHT: f32 = 15.0;
const OUTLINE_ROW_PADDING: f32 = 8.0;
const OUTLINE_COMPACT_ROW_HEIGHT: f32 = 24.0;

/// Responsive geometry for an authored bookmark.
///
/// Continuation lines begin where the first line begins (the depth spacer is
/// outside the text), producing a hanging indent. Deep trees surrender some
/// indentation on narrow rails so hierarchy never consumes the title.
/// The width the rename and delete controls take from a row's label — two
/// icon buttons and their spacing — reserved only where the document can be
/// edited at all, so a read-only rail keeps the full line.
pub const BOOKMARK_ACTIONS_WIDTH: f32 = 44.0;

pub fn bookmark_row_geometry(title: &str, depth: usize, width: f32, editable: bool) -> (f32, f32) {
    let width = width.max(80.0);
    let indent_step = (width / 30.0).clamp(6.0, 10.0);
    let indent = (depth.min(6) as f32 * indent_step).min(width * 0.32);
    // Editable rows also give up the scroll lane, so the rename and delete
    // buttons never sit under the thumb (see `widgets::scroll::thumbed`).
    let actions = if editable {
        BOOKMARK_ACTIONS_WIDTH + crate::widgets::tokens::SCROLL_LANE_WIDTH + theme::space::XS
    } else {
        0.0
    };
    let label_width = (width - indent - 31.0 - actions).max(36.0);
    let ems = title.chars().map(|character| match character {
        ' ' | '\t' => 0.32,
        'i' | 'l' | 'I' | '.' | ',' | ':' | ';' | '!' | '|' => 0.34,
        'm' | 'w' | 'M' | 'W' => 0.9,
        character if character.is_ascii_uppercase() => 0.68,
        _ => 0.56,
    });
    // A small allowance covers word wrapping: the renderer moves a whole
    // word when it does not fit, while the estimate above is continuous.
    let estimated_width = ems.sum::<f32>() * theme::type_scale::LABEL * 1.08;
    let lines = (estimated_width / label_width).ceil().max(1.0);
    let height =
        (lines * OUTLINE_LINE_HEIGHT + OUTLINE_ROW_PADDING).max(OUTLINE_COMPACT_ROW_HEIGHT);
    (indent, height)
}

#[allow(clippy::too_many_arguments)]
fn virtual_bookmark_outline<Message: Clone + 'static>(
    entries: std::sync::Arc<Vec<crate::widgets::context::OutlineRow>>,
    focus: Option<crate::widgets::document::model::OutlineItemId>,
    scroll: f32,
    measured_viewport: std::rc::Rc<std::cell::Cell<f32>>,
    measured_width: std::rc::Rc<std::cell::Cell<f32>>,
    live: bool,
    editable: bool,
    editing: Option<(usize, String)>,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    responsive(move |size| {
        use crate::widgets::document::model::OutlineItemId;

        measured_viewport.set(size.height);
        measured_width.set(size.width);
        let geometry: Vec<(f32, f32)> = entries
            .iter()
            .map(|entry| bookmark_row_geometry(&entry.title, entry.depth, size.width, editable))
            .collect();
        let heights: Vec<f32> = geometry.iter().map(|(_, height)| *height).collect();
        let window = crate::widgets::scroll::variable_window(&heights, scroll, size.height);
        // One shape for every small control a row carries, the annotations
        // panel's: an icon button in the tool style, wrapped in its hint.
        let action = move |icon, describe: &'static str, command: ReadCommand| {
            let mut control = button(theme::icon::icon(icon, theme::type_scale::LABEL))
                .padding(theme::space::XS)
                .style(theme::ambient::tool_button);
            if live {
                control = control.on_press(on_event(WidgetEvent::Read(command)));
            }
            hint(control, describe)
        };
        let mut rows = Column::new();
        if window.before > 0.0 {
            rows = rows.push(space::vertical().height(window.before));
        }
        for index in window.rows {
            let entry = &entries[index];
            let (indent, height) = geometry[index];
            let id = OutlineItemId::Bookmark {
                source_ordinal: entry.source_ordinal,
            };
            let focused = focus.as_ref() == Some(&id);
            let marker = outline_focus_marker(focused);

            // The row whose title is open for editing holds the field where
            // its label was, with a confirm and a cancel beside it. Submit
            // commits; Escape reaches this panel's key handler and cancels.
            if editable
                && editing
                    .as_ref()
                    .is_some_and(|(ordinal, _)| *ordinal == entry.source_ordinal)
            {
                let draft = editing
                    .as_ref()
                    .map(|(_, draft)| draft.clone())
                    .unwrap_or_default();
                let mut field = iced::widget::text_input("Bookmark title", &draft)
                    .id(bookmark_rename_input_id())
                    .size(theme::type_scale::LABEL)
                    .style(theme::ambient::text_field)
                    .padding(Padding::from([2.0, 6.0]))
                    .width(Length::Fill);
                if live {
                    field = field
                        .on_input(move |typed| {
                            on_event(WidgetEvent::Read(ReadCommand::TypeBookmarkTitle(typed)))
                        })
                        .on_submit(on_event(WidgetEvent::Read(
                            ReadCommand::CommitBookmarkTitle,
                        )));
                }
                rows = rows.push(
                    container(
                        row![
                            marker,
                            space::horizontal().width(Length::Fixed(indent)),
                            field,
                            action(
                                theme::Icon::Check,
                                "Rename",
                                ReadCommand::CommitBookmarkTitle
                            ),
                            action(
                                theme::Icon::Close,
                                "Keep the old title",
                                ReadCommand::CancelBookmarkTitle
                            ),
                            space::horizontal()
                                .width(Length::Fixed(crate::widgets::tokens::SCROLL_LANE_WIDTH)),
                        ]
                        .spacing(theme::space::XS)
                        .align_y(Alignment::Center),
                    )
                    .id(outline_item_id(&id))
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
                );
                continue;
            }

            let label = text(entry.title.clone())
                .size(theme::type_scale::LABEL)
                .line_height(iced::widget::text::LineHeight::Absolute(iced::Pixels(
                    OUTLINE_LINE_HEIGHT,
                )))
                .width(Length::Fill);
            let mut control = button(
                row![
                    marker,
                    space::horizontal().width(Length::Fixed(indent)),
                    label
                ]
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fixed(height - 2.0))
            .padding(Padding::from([4.0, 6.0]))
            .style(if focused {
                theme::ambient::focus_button
            } else {
                theme::ambient::tool_button
            });
            if live {
                control = control.on_press(on_event(WidgetEvent::Read(
                    ReadCommand::ActivateOutlineItem(id.clone()),
                )));
            }
            let mut content = row![control]
                .spacing(theme::space::XS)
                .align_y(Alignment::Center);
            if editable {
                content = content
                    .push(action(
                        theme::Icon::Pencil,
                        "Rename this bookmark",
                        ReadCommand::RenameBookmark(entry.source_ordinal),
                    ))
                    .push(action(
                        theme::Icon::Trash,
                        "Delete this bookmark",
                        ReadCommand::DeleteBookmark(entry.source_ordinal),
                    ))
                    // The scroll thumb draws in a lane over the right edge;
                    // leave it empty ground so the delete button is never
                    // under the thumb.
                    .push(
                        space::horizontal()
                            .width(Length::Fixed(crate::widgets::tokens::SCROLL_LANE_WIDTH)),
                    );
            }
            rows = rows.push(
                container(content)
                    .id(outline_item_id(&id))
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            );
        }
        if window.after > 0.0 {
            rows = rows.push(space::vertical().height(window.after));
        }
        crate::widgets::scroll::thumbed(
            crate::widgets::scroll::vertical(rows)
                .id(outline_scrollable_id())
                .width(Length::Fill)
                .height(Length::Fill)
                .on_scroll(move |viewport| {
                    on_event(WidgetEvent::Read(ReadCommand::OutlineScrolled {
                        offset: viewport.absolute_offset().y.max(0.0).round() as u32,
                        viewport: viewport.bounds().height.max(0.0).round() as u32,
                    }))
                }),
            scroll,
            move |offset| on_event(WidgetEvent::Read(ReadCommand::OutlineDragScrollTo(offset))),
        )
    })
    .into()
}

/// Build only the fixed-height outline rows near the viewport while keeping
/// the scrollbar's full extent.
fn virtual_outline<Message: Clone + 'static>(
    count: usize,
    row_height: f32,
    scroll: f32,
    measured_viewport: std::rc::Rc<std::cell::Cell<f32>>,
    on_event: fn(WidgetEvent) -> Message,
    row_at: impl Fn(usize) -> Element<'static, Message> + 'static,
) -> Element<'static, Message> {
    responsive(move |size| {
        measured_viewport.set(size.height);
        let window = crate::widgets::scroll::virtual_window(count, row_height, scroll, size.height);
        let mut rows = Column::new();
        if window.before > 0.0 {
            rows = rows.push(space::vertical().height(window.before));
        }
        for index in window.rows {
            rows = rows.push(row_at(index));
        }
        if window.after > 0.0 {
            rows = rows.push(space::vertical().height(window.after));
        }
        crate::widgets::scroll::thumbed(
            crate::widgets::scroll::vertical(rows)
                .id(outline_scrollable_id())
                .width(Length::Fill)
                .height(Length::Fill)
                .on_scroll(move |viewport| {
                    on_event(WidgetEvent::Read(ReadCommand::OutlineScrolled {
                        offset: viewport.absolute_offset().y.max(0.0).round() as u32,
                        viewport: viewport.bounds().height.max(0.0).round() as u32,
                    }))
                }),
            scroll,
            move |offset| on_event(WidgetEvent::Read(ReadCommand::OutlineDragScrollTo(offset))),
        )
    })
    .into()
}

/// The one mounted outline stream.
pub fn outline_scrollable_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-document-outline")
}

/// The bookmark rename field. One stable identity, because at most one row
/// is ever open for editing and a keybinding has to be able to focus it.
pub fn bookmark_rename_input_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-bookmark-rename")
}

/// Stable widget identity for a row whose list position may change.
pub fn outline_item_id(
    item: &crate::widgets::document::model::OutlineItemId,
) -> iced::advanced::widget::Id {
    use crate::widgets::document::model::OutlineItemId;

    let key = match item {
        OutlineItemId::Bookmark { source_ordinal } => format!("bookmark-{source_ordinal}"),
        OutlineItemId::Field {
            name,
            source_ordinal,
        } => format!("field-{source_ordinal}-{name}"),
        OutlineItemId::Annotation(id) => format!("annotation-{}", id.as_str()),
    };
    iced::advanced::widget::Id::from(format!("pulpit-outline-{key}"))
}

/// The annotation toolbar: what a press does to the page.
fn tools<Message: Clone + 'static>(
    _widget: &Widget,
    reader: &ReaderData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let state = DocumentToolsState {
        live: mode.interactive() && reader.annotatable(),
        open: reader.open,
        tool: reader.controls.tool,
        tool_options: reader.controls.tool_options,
        tool_wheel: reader.controls.tool_wheel,
        overflow_open: reader.controls.tool_overflow,
        ink_color: reader.controls.ink_color,
        ink_width: reader.controls.ink_width,
        highlight_color: reader.controls.highlight_color,
        select_kind: reader.controls.select_kind,
        markup_kind: reader.controls.markup_kind,
        shape_kind: reader.controls.shape_kind,
        stamp_mark: reader.controls.stamp_mark,
        text_color: reader.controls.text_color,
        text_size: reader.controls.text_size,
        selected: reader.selected,
        can_undo: reader.can_undo,
        can_redo: reader.can_redo,
    };
    responsive(move |size| document_tools_band(state, size.width, on_event)).into()
}

#[derive(Debug, Clone, Copy)]
struct DocumentToolsState {
    live: bool,
    open: bool,
    tool: Option<AnnotationTool>,
    tool_options: Option<AnnotationTool>,
    tool_wheel: Option<AnnotationTool>,
    overflow_open: bool,
    ink_color: InkColor,
    ink_width: f32,
    highlight_color: InkColor,
    select_kind: pulpit_core::annotation::SelectKind,
    markup_kind: pulpit_core::annotation::MarkupKind,
    shape_kind: pulpit_core::annotation::ShapeKind,
    stamp_mark: pulpit_core::annotation::StampChoice,
    text_color: InkColor,
    text_size: f32,
    selected: bool,
    can_undo: bool,
    can_redo: bool,
}

fn document_tools_band<Message: Clone + 'static>(
    state: DocumentToolsState,
    width: f32,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let hand = document_command_button(
        theme::Icon::Hand,
        "Hand — use links and fields",
        ReadCommand::Arm(None),
        state.tool.is_none(),
        state.live,
        on_event,
    );

    if tools_are_compact(width) {
        let mut bar = Row::new()
            .push(hand)
            .spacing(theme::space::XS)
            .align_y(Alignment::Center);
        // Keep the armed tool (and therefore its options arrow) on the row.
        // Everything else is still reachable by name through More.
        if let Some(tool) = state.tool {
            bar = bar.push(document_tool_control(tool, state, on_event));
        }
        bar = bar.push(document_tools_overflow(state, on_event));
        return container(bar)
            .width(Length::Fill)
            .align_y(Alignment::Center)
            .into();
    }

    let mut bar = Row::new()
        .push(hand)
        .spacing(theme::space::XS)
        .align_y(Alignment::Center);
    for tool in AnnotationTool::DOCUMENT {
        bar = bar.push(document_tool_control(tool, state, on_event));
    }
    bar = bar
        .push(space::horizontal().width(Length::Fixed(theme::space::M)))
        .push(document_command_button(
            theme::Icon::Trash,
            "Delete the selected mark",
            ReadCommand::DeleteSelected,
            false,
            state.live && state.selected,
            on_event,
        ))
        .push(space::horizontal().width(Length::Fixed(theme::space::M)))
        .push(document_command_button(
            theme::Icon::Undo,
            "Undo",
            ReadCommand::Undo,
            false,
            state.live && state.can_undo,
            on_event,
        ))
        .push(document_command_button(
            theme::Icon::Redo,
            "Redo",
            ReadCommand::Redo,
            false,
            state.live && state.can_redo,
            on_event,
        ))
        .push(document_command_button(
            theme::Icon::Save,
            "Save as…",
            ReadCommand::SaveAs,
            false,
            state.live && state.open,
            on_event,
        ))
        .push(document_command_button(
            theme::Icon::Printer,
            "Print…",
            ReadCommand::Print,
            false,
            state.live && state.open,
            on_event,
        ))
        .push(document_command_button(
            theme::Icon::Stamp,
            "Sign…",
            ReadCommand::Sign,
            false,
            state.live && state.open,
            on_event,
        ));
    container(bar)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}

/// Below this the band shows the armed tool and More instead of the whole
/// palette.
///
/// Derived from the palette's own length rather than written down once: the
/// figure was measured when `DOCUMENT` held seven tools, and a palette that
/// grew without moving it would be drawn full into a band too narrow to hold
/// it. `PER_TOOL` is one tool's glyph, its padding and its options arrow.
fn tools_are_compact(width: f32) -> bool {
    const MEASURED_AT: f32 = 590.0;
    const MEASURED_TOOLS: usize = 7;
    const PER_TOOL: f32 = 56.0;

    let extra = AnnotationTool::DOCUMENT
        .len()
        .saturating_sub(MEASURED_TOOLS);
    width < MEASURED_AT + extra as f32 * PER_TOOL
}

fn document_command_button<Message: Clone + 'static>(
    icon: theme::Icon,
    label: &'static str,
    command: ReadCommand,
    selected: bool,
    enabled: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let control = button(theme::icon::icon(icon, theme::type_scale::HEADING))
        .padding(Padding::from([4.0, 8.0]))
        .style(if selected {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
    let control = if enabled {
        control.on_press(on_event(WidgetEvent::Read(command)))
    } else {
        control
    };
    hint(control, label)
}

fn document_tool_control<Message: Clone + 'static>(
    tool: AnnotationTool,
    state: DocumentToolsState,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let selected = state.tool == Some(tool);
    let mut control = button(document_tool_glyph(tool, state, theme::type_scale::HEADING))
        .padding(Padding::from([4.0, 8.0]))
        .style(if selected {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
    if state.live {
        control = control.on_press(on_event(WidgetEvent::Read(ReadCommand::Arm(Some(tool)))));
    }
    let control = hint(control, tool.label());
    // Asked of the tool rather than of its colour. A colour used to be the
    // only thing any of these had to configure, so "has a colour" stood in
    // for "has a panel"; the band has a panel and no colour.
    if !tool.has_options() {
        return control;
    }

    let options_open = state.tool_options == Some(tool);
    let mut arrow = button(theme::icon::icon(theme::Icon::ChevronDown, 9.0))
        .padding(0)
        .width(Length::Fixed(12.0))
        .height(Length::Fixed(12.0))
        .style(theme::ambient::tool_button);
    if state.live {
        arrow = arrow.on_press(on_event(WidgetEvent::Read(ReadCommand::ToolOptions(
            (!options_open).then_some(tool),
        ))));
    }
    let trigger: Element<'static, Message> = row![
        control,
        container(arrow)
            .height(Length::Fixed(theme::type_scale::HEADING + 8.0))
            .align_y(Alignment::End),
    ]
    .spacing(0)
    .align_y(Alignment::Center)
    .into();

    let trigger = crate::widgets::common::color::wheel(
        trigger,
        state.live && state.tool_wheel == Some(tool),
        state.color(tool).unwrap_or_default(),
        on_event(WidgetEvent::Read(ReadCommand::ToolColorWheel(None))),
        move |colour| on_event(WidgetEvent::Read(ReadCommand::SetToolColor(tool, colour))),
    );

    let panel = options_open.then(|| document_tool_options_panel(tool, state, on_event));
    // Below the tool's own icon: this toolbar sits at the top of the window,
    // so a panel that opened upward would leave it.
    crate::widgets::common::popover::Popover::new(trigger, panel)
        .prefer_below()
        .on_dismiss(on_event(WidgetEvent::Read(ReadCommand::ToolOptions(None))))
        .into()
}

impl DocumentToolsState {
    fn color(self, tool: AnnotationTool) -> Option<InkColor> {
        match tool {
            // The shape tool and the stamp draw in the pen's ink, so their
            // panels offer the pen's colour and setting it there sets the
            // pen's: one pen, whichever panel it was reached from.
            AnnotationTool::Ink | AnnotationTool::Shape | AnnotationTool::Stamp => {
                Some(self.ink_color)
            }
            AnnotationTool::Highlighter => Some(self.highlight_color),
            AnnotationTool::Text | AnnotationTool::Note => Some(self.text_color),
            _ => None,
        }
    }
}

/// A tool's icon in the Reader's toolbar, in the palette's text colour.
///
/// Never in the ink the tool lays down, for the reason given on the
/// presenter palette's `tool_glyph`: an icon says which tool, and a swatch
/// says which colour.
fn document_tool_glyph<Message: 'static>(
    tool: AnnotationTool,
    state: DocumentToolsState,
    size: f32,
) -> Element<'static, Message> {
    let icon = match tool {
        AnnotationTool::Select => theme::Icon::Select,
        AnnotationTool::Ink => theme::Icon::Pen,
        // The highlighter's glyph is the mark it is set to make, which is the
        // one thing about it a toolbar can show without opening its options.
        AnnotationTool::Highlighter => {
            crate::widgets::common::view::markup_kind_glyph(state.markup_kind)
        }
        // …and the shape tool's is the shape it is set to draw, for the same
        // reason: four shapes under one button, and the button says which.
        AnnotationTool::Shape => crate::widgets::common::view::shape_kind_glyph(state.shape_kind),
        AnnotationTool::Text => theme::Icon::Type,
        AnnotationTool::Note => theme::Icon::StickyNote,
        // …and the stamp's is the mark it is set to put down.
        AnnotationTool::Stamp => crate::widgets::common::view::stamp_mark_glyph(state.stamp_mark),
        AnnotationTool::Eraser => theme::Icon::Eraser,
        AnnotationTool::SelectText => theme::Icon::TextCursor,
        AnnotationTool::Pointer | AnnotationTool::Spotlight => theme::Icon::Pointer,
    };
    theme::icon::icon(icon, size)
}

fn document_tools_overflow<Message: Clone + 'static>(
    state: DocumentToolsState,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let trigger = overflow_trigger(
        state.overflow_open,
        state.live.then(|| {
            on_event(WidgetEvent::Read(ReadCommand::ToolOverflow(
                !state.overflow_open,
            )))
        }),
    );
    let panel = state
        .overflow_open
        .then(|| document_tools_overflow_menu(state, on_event));
    crate::widgets::common::popover::Popover::new(hint(trigger, "More annotation controls"), panel)
        .on_dismiss(on_event(WidgetEvent::Read(ReadCommand::ToolOverflow(
            false,
        ))))
        .into()
}

fn document_tools_overflow_menu<Message: Clone + 'static>(
    state: DocumentToolsState,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let mut menu = column![crate::widgets::common::view::panel_header(
        "More annotation controls",
        state
            .live
            .then(|| on_event(WidgetEvent::Read(ReadCommand::ToolOverflow(false)))),
    )]
    .spacing(theme::space::XS);

    for tool in AnnotationTool::DOCUMENT {
        let selected = state.tool == Some(tool);
        let mut arm = button(
            row![
                document_tool_glyph(tool, state, theme::type_scale::BODY),
                text(tool.label()).size(theme::type_scale::CAPTION),
            ]
            .spacing(theme::space::S)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([4.0, theme::space::S]))
        .style(if selected {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
        if state.live {
            arm = arm.on_press(on_event(WidgetEvent::Read(ReadCommand::Arm(Some(tool)))));
        }
        if tool.has_options() {
            let open = state.tool_options == Some(tool);
            let mut arrow = button(theme::icon::icon(
                if open {
                    theme::Icon::ChevronUp
                } else {
                    theme::Icon::ChevronDown
                },
                theme::type_scale::BODY,
            ))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button);
            if state.live {
                arrow = arrow.on_press(on_event(WidgetEvent::Read(ReadCommand::ToolOptions(
                    (!open).then_some(tool),
                ))));
            }
            menu = menu.push(row![arm, arrow].spacing(theme::space::XS));
            if open {
                menu = menu.push(document_tool_options_panel(tool, state, on_event));
            }
        } else {
            menu = menu.push(arm);
        }
    }

    for (icon, label, command, enabled) in [
        (
            theme::Icon::Trash,
            "Delete selected mark",
            ReadCommand::DeleteSelected,
            state.selected,
        ),
        (theme::Icon::Undo, "Undo", ReadCommand::Undo, state.can_undo),
        (theme::Icon::Redo, "Redo", ReadCommand::Redo, state.can_redo),
        (
            theme::Icon::Save,
            "Save as…",
            ReadCommand::SaveAs,
            state.open,
        ),
        (
            theme::Icon::Printer,
            "Print…",
            ReadCommand::Print,
            state.open,
        ),
        (theme::Icon::Stamp, "Sign…", ReadCommand::Sign, state.open),
    ] {
        let mut control = button(
            row![
                theme::icon::icon(icon, theme::type_scale::BODY),
                text(label).size(theme::type_scale::CAPTION),
            ]
            .spacing(theme::space::S),
        )
        .width(Length::Fill)
        .padding(Padding::from([4.0, theme::space::S]))
        .style(theme::ambient::tool_button);
        if state.live && enabled {
            control = control.on_press(on_event(WidgetEvent::Read(command)));
        }
        menu = menu.push(control);
    }

    container(scrollable(menu).style(theme::ambient::scrollbar))
        .width(Length::Fixed(250.0))
        .max_height(400.0)
        .padding(theme::space::S)
        .style(theme::ambient::surface)
        .into()
}

fn document_tool_options_panel<Message: Clone + 'static>(
    tool: AnnotationTool,
    state: DocumentToolsState,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let mut panel = crate::widgets::common::options::Options::new(tool.label()).on_close(
        state
            .live
            .then(|| on_event(WidgetEvent::Read(ReadCommand::ToolOptions(None)))),
    );
    // A tool that lays no colour down gets no colour row. Until the band had
    // options of its own no such tool opened this panel, so the row was
    // unconditional and fell back to black — which would now offer the
    // rubber band a colour it has no way to use.
    if let Some(selected) = state.color(tool) {
        panel = panel.row(
            "Color",
            crate::widgets::common::color::swatches(
                selected,
                crate::widgets::common::color::SWATCH,
                state.live,
                move |colour| on_event(WidgetEvent::Read(ReadCommand::SetToolColor(tool, colour))),
                on_event(WidgetEvent::Read(ReadCommand::ToolColorWheel(Some(tool)))),
                hint,
            ),
        );
    }
    // The band's three kinds, the one thing it has to configure.
    if tool == AnnotationTool::Select {
        let mut kinds = Row::new().spacing(theme::space::XS);
        for kind in pulpit_core::annotation::SelectKind::ALL {
            kinds = kinds.push(document_command_button(
                crate::widgets::common::view::select_kind_glyph(kind),
                kind.label(),
                ReadCommand::SetSelectKind(kind),
                state.select_kind == kind,
                state.live,
                on_event,
            ));
        }
        panel = panel.row("Takes", kinds);
    }
    // The highlighter's three marks, beside the colour they are laid down in.
    if tool == AnnotationTool::Highlighter {
        let mut kinds = Row::new().spacing(theme::space::XS);
        for kind in pulpit_core::annotation::MarkupKind::ALL {
            kinds = kinds.push(document_command_button(
                crate::widgets::common::view::markup_kind_glyph(kind),
                kind.label(),
                ReadCommand::SetMarkupKind(kind),
                state.markup_kind == kind,
                state.live,
                on_event,
            ));
        }
        panel = panel.row("Marks", kinds);
    }
    // The shape tool's four, in the same place and for the same reason.
    if tool == AnnotationTool::Shape {
        let mut kinds = Row::new().spacing(theme::space::XS);
        for kind in pulpit_core::annotation::ShapeKind::ALL {
            kinds = kinds.push(document_command_button(
                crate::widgets::common::view::shape_kind_glyph(kind),
                kind.label(),
                ReadCommand::SetShapeKind(kind),
                state.shape_kind == kind,
                state.live,
                on_event,
            ));
        }
        panel = panel.row("Shapes", kinds);
    }
    // …and the stamp's two.
    if tool == AnnotationTool::Stamp {
        let mut marks = Row::new().spacing(theme::space::XS);
        for mark in pulpit_core::annotation::StampChoice::ALL {
            marks = marks.push(document_command_button(
                crate::widgets::common::view::stamp_mark_glyph(mark),
                mark.label(),
                ReadCommand::SetStampMark(mark),
                state.stamp_mark == mark,
                state.live,
                on_event,
            ));
        }
        panel = panel.row("Marks", marks);
    }
    // Measures are in page points here rather than fractions of the page, so
    // the caption carries the number: a reader setting a pen to two points is
    // setting it to something the document itself is measured in.
    if matches!(tool, AnnotationTool::Ink | AnnotationTool::Shape) {
        panel = panel.row(
            format!("Width — {:.1} pt", state.ink_width),
            slider(0.5..=12.0, state.ink_width, move |value| {
                on_event(WidgetEvent::Read(ReadCommand::SetInkWidth(value)))
            })
            .step(0.5_f32)
            .width(Length::Fixed(
                crate::widgets::common::options::CONTROL_WIDTH,
            )),
        );
    }
    if matches!(tool, AnnotationTool::Text | AnnotationTool::Note) {
        panel = panel.row(
            format!("Size — {:.0} pt", state.text_size),
            slider(6.0..=48.0, state.text_size, move |value| {
                on_event(WidgetEvent::Read(ReadCommand::SetTextSize(value)))
            })
            .step(1.0_f32)
            .width(Length::Fixed(
                crate::widgets::common::options::CONTROL_WIDTH,
            )),
        );
    }
    panel.into()
}
