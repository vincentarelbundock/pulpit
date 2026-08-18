//! The presenter window, its three pages, and the optional audience window.
//!
//! The audience view is deliberately minimal: a black background and the
//! current slide, aspect-fit. No controls, no overlays, nothing that can
//! invalidate the last valid frame. Letterbox bars are black; white belongs
//! to the explicit white-blanking mode alone.
//!
//! The presenter window draws the **active layout**, so what was designed in
//! the editor is exactly what appears here.

use iced::widget::{
    button, column, container, image, mouse_area, pick_list, responsive, row, scrollable, space,
    stack, text, text_input, Column, Row,
};
use iced::{window, Alignment, Color, ContentFit, Element, Length};

use crate::settings::Action;
use pulpit_core::{Blank, NotesMapping, PairedRule, Region};
use pulpit_display::{Role, RoleTarget};
use pulpit_render::cache::FrameKind;

use crate::platform::Appearance;

use crate::app::{App, LayoutDialog, Message};
use crate::designer::Page;
use crate::designer_view;
use crate::panel::panel;
use crate::theme;
use crate::theme::{space as gap, target, type_scale};
use crate::toast::Intent;

pub fn view(app: &App, window: window::Id) -> Element<'_, Message> {
    // One palette for the whole pass. The audience window is deliberately
    // exempt from theming: its colours are output, not chrome.
    theme::ambient::set(app.theme.palette);

    if Some(window) == app.audience_window {
        return audience(app);
    }

    let mut page = match app.page {
        Page::Presenter => presenter(app),
        Page::Library => library_page(app),
        Page::Editor => editor_page(app),
        Page::Settings => settings_page(app),
    };

    // A ringing cue washes the presenter window, under everything else so it
    // tints the page rather than covering any control — including the Snooze
    // and Dismiss buttons that answer it. Never on the audience window: the
    // room must not learn that the speaker is out of time.
    // Running past the target washes it the same way, from the same helper:
    // "you are out of time" is one piece of news whether the clock or the
    // timer noticed it. With both going at once the stronger of the two is
    // drawn, rather than two tints stacked into something twice as dark.
    let reduce_motion = app.motion == crate::platform::Motion::Reduced;
    let alert = [
        app.alarm_controls.flash(app.now, reduce_motion),
        app.timer_controls.flash(app.now, reduce_motion),
    ]
    .into_iter()
    .flatten()
    .fold(None::<f32>, |strongest, alpha| {
        Some(strongest.map_or(alpha, |current: f32| current.max(alpha)))
    });
    if let Some(alpha) = alert {
        page = stack![page, alarm_flash(alpha)].into();
    }

    // Toasts float above whatever page is showing, and never on the audience
    // window.
    if let Some(overlay) = toasts(app) {
        page = stack![page, overlay].into();
    }
    if app.confirm_reset_colors {
        page = stack![page, reset_colors_dialog()].into();
    }
    // What the open document asked to do to the reader's place in it. Above
    // the page, because it is a question about the page.
    if let Some(request) = app.pending_form_goto.as_ref() {
        page = stack![page, form_navigation_dialog(request)].into();
    }
    // What a save would leave empty, asked before the file is written.
    // A small always-present affordance for the signature panel (§31.4),
    // whenever the open document has at least one signature to report on.
    // Not folded into the reader toolbar's generic `Message` widget (see
    // `widgets::document::view::tools`'s note) because the panel is `App`
    // state, not `ReadCommand` state.
    if !app.document_signatures.is_empty() && !app.signature_panel_open {
        page = stack![page, signature_panel_toggle(app)].into();
    }
    if app.signature_panel_open {
        page = stack![page, signature_panel(app)].into();
    }
    // §31.3, A9: offered before anything can mutate a document that already
    // carries a signature. Above the page, non-dismissable except its own
    // two buttons — declining silently would leave the reader guessing
    // which mode they are in.
    if app.pending_append_only_offer {
        page = stack![page, append_only_offer_dialog()].into();
    }
    // The Sign flow (SPEC-signing.md §31.1), one dialog for whichever step
    // it is on.
    if let Some(flow) = app.signing.as_ref() {
        let on_page_zero = app
            .reader
            .current_page()
            .is_some_and(|p| p == pulpit_core::page::PageIndex(0));
        page = stack![page, sign_dialog(flow, on_page_zero)].into();
    }
    if let Some(review) = app.pending_save_review.as_ref() {
        page = stack![page, save_review_dialog(review)].into();
    }
    // The alarm popup is a top-level overlay rather than something drawn
    // inside the clock's pane: a clock can be a narrow cell in a strip, and a
    // popup anchored there would be clipped by its own widget.
    if app.alarm_controls.open {
        page = stack![page, alarms_dialog(app)].into();
    }
    if app.shortcuts_open {
        page = stack![page, shortcuts_overlay(app, true)].into();
    }
    if app.about_open {
        page = stack![page, about_overlay()].into();
    }
    // The timer menu is the same kind of overlay, for the same reason.
    if app.timer_controls.open {
        page = stack![page, timer_dialog(app)].into();
    }
    // The restore offer sits above everything else in the presenter window,
    // and has no counterpart on the audience window: the audience must learn
    // nothing about the interrupted session until it is confirmed.
    if let Some(plan) = app.pending_restore.as_ref() {
        page = stack![page, restore_session_dialog(plan)].into();
    }
    // The same rule for a document: what a previous run left unsaved is
    // offered, never applied, and the offer has no way out but an answer.
    if let Some(journal) = app.reader_recovery.as_ref() {
        page = stack![page, restore_edits_dialog(journal)].into();
    }
    // Its own renderer, its own atlas, its own residency — exactly as the
    // projector has, and for the same reason. A slide panel's picture is well
    // over the two mebibytes at which Iced stops uploading inline, so without
    // this a panel draws nothing on the pass a new frame first reaches it.
    crate::residency::resident(page, app.presenter_resident_handles(), app.upload_meter())
}

// --------------------------------------------------------------- audience

fn audience(app: &App) -> Element<'_, Message> {
    let background = match app.state.blank() {
        Blank::White => Color::WHITE,
        _ => Color::BLACK,
    };

    let frame = app.audience_frame();
    // Blanking wins over everything: with no frame there is nothing to mark
    // up, and putting a spotlight on a black screen would undo the blank.
    let annotated =
        frame.is_some() && app.annotations.audience_visible && !app.annotations.is_empty();
    // The audience sees the same overlay frames the presenter does: one
    // authoritative session feeds both windows, which is the whole point of
    // the media architecture. What it never sees is the chrome — no focus
    // rings, no warnings, no runtime names.
    let overlays = app.audience_overlays();
    let content: Element<'_, Message> = match frame {
        Some(picture) => {
            let handle = picture.handle;
            let slide = move |handle: iced::widget::image::Handle| -> Element<'static, Message> {
                image(handle)
                    .content_fit(ContentFit::Contain)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            };
            let aspect = app.slide_aspect();
            let crop = app
                .state
                .audience_source()
                .map(|source| source.region)
                .unwrap_or(Region::FULL);
            let picture: Element<'_, Message> = if overlays.is_empty() {
                slide(handle)
            } else {
                responsive(move |size| {
                    crate::widgets::slides::view::composite(
                        slide(handle.clone()),
                        &overlays,
                        size,
                        aspect,
                        ContentFit::Contain,
                        crop,
                    )
                })
                .into()
            };
            if annotated {
                // The same geometry the presenter panel uses, from the same
                // function: the two windows are different sizes and must
                // still put a stroke on the same word.
                let style = app.annotation_options().style();
                let aspect = app.slide_aspect();
                let crop = app
                    .state
                    .audience_source()
                    .map(|source| source.region)
                    .unwrap_or(Region::FULL);
                // Always a stack, empty or not, for the reason given in
                // `widgets::slides::view::composite`: the projector's picture
                // must not change depth in the widget tree — and so lose a
                // frame — because a stroke was drawn or rubbed out.
                match crate::widgets::annotations::view::marks(
                    app.annotations_snapshot(),
                    app.audience_marks_cache(),
                    style,
                    aspect,
                    ContentFit::Contain,
                    crop,
                    false,
                    app.rendered_text_snapshot(),
                ) {
                    Some(marks) => stack![picture, marks].into(),
                    None => stack![picture].into(),
                }
            } else {
                stack![picture].into()
            }
        }
        None => space::horizontal()
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    };

    // The projector's own renderer, with its own image cache: an audience
    // frame is tens of megabytes, and without this the first draw of every new
    // one was a black flash while the upload ran on a worker thread. See
    // `residency`.
    crate::residency::resident(
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(move |_| container::Style {
                background: Some(background.into()),
                ..container::Style::default()
            }),
        app.audience_resident_handles(),
        app.upload_meter(),
    )
}

// -------------------------------------------------------------- presenter

fn presenter(app: &App) -> Element<'_, Message> {
    let frame = |slide: usize, kind: FrameKind, max_width: u32| {
        app.frame_for_width(slide, kind, max_width)
            .map(|picture| picture.handle)
    };
    let context = app.render_context(
        crate::widgets::Mode::Live,
        &frame,
        crate::widgets::sample::NOTES,
    );
    if app.reader_fullscreen {
        if let Some(page) = app
            .active_layout
            .widgets()
            .into_iter()
            .find(|widget| widget.kind() == crate::widgets::WidgetKind::DocumentPage)
        {
            return crate::layout_renderer::widget(
                page,
                &context,
                app.compose_buffer(),
                interaction,
            );
        }
    }
    let body = crate::layout_renderer::layout(
        &app.active_layout,
        &context,
        app.compose_buffer(),
        interaction,
    );

    // Anything the layout does not carry gets a strip of its own. Floating it
    // over the layout would cover whatever the presenter put top-left.
    let framed = container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(gap::S);
    // Side panels belong to the working surface. Keeping this boundary below
    // the toolbar means neither their opening nor their closing can move the
    // menu button or any of the controls in that band.
    let has_outline_panel = app
        .active_layout
        .widgets()
        .iter()
        .any(|widget| widget.kind() == crate::widgets::WidgetKind::DocumentOutline);
    let body = if has_outline_panel {
        // The layout renderer substitutes Search into the outline's slot, so
        // both panels have exactly the same position and extent.
        framed.into()
    } else {
        // Presenter layouts have no outline slot. They retain a transient
        // rail, but it still uses the same infrastructure and stays below
        // the toolbar.
        crate::layout_renderer::side_panel(
            search_workspace(app),
            framed,
            280.0,
            app.search_reveal(),
        )
    };
    let mut page: Element<'_, Message> = match presenter_toolbar(app) {
        Some(toolbar) => column![toolbar, body].into(),
        None => body,
    };

    // With no file open, the layout has no useful content to draw. Make the
    // keyboard-first interface teach itself, using the very same reference
    // as the Help command so startup documentation cannot go stale.
    if app.state.document().is_none() {
        page = stack![page, shortcuts_overlay(app, false)].into();
    }

    if app.menu_open {
        page = stack![page, menu(app)].into();
    }
    if app.audience_start_menu_open {
        page = stack![page, audience_start_menu(app)].into();
    }
    // The scrub layer is *always* stacked, empty when idle. Stacking it only
    // while scrubbing changed the shape of the widget tree the moment the
    // first thumbnail arrived, which threw away the slider's drag state
    // mid-drag: the handle froze as soon as the preview appeared.
    page = stack![page, scrub_layer(app)].into();
    if app.overview {
        page = stack![page, overview(app)].into();
    }
    if let Some(prompt) = unbound_key(app) {
        page = stack![page, prompt].into();
    }
    page
}

/// Search is a transient rail beside the working surface. The document's own
/// outline remains mounted, so bookmarks and search can be consulted together.
fn search_workspace(app: &App) -> Element<'_, Message> {
    let search = crate::widgets::context::SearchData {
        state: &app.search,
        input_focus: app.search_input_focused(),
        results_focus: app.search_results_focused(),
        scroll: app.search_scroll,
        viewport: app.search_viewport.clone(),
    };
    crate::widgets::search::view::pane(search, true, interaction)
}

/// What the slider is pointing at, while it is being dragged.
///
/// A number tells you where you are in the deck; it does not tell you which
/// slide that is. Dragging past forty pages looking for "the one with the
/// graph" is exactly the moment a picture is worth more than an index, so
/// the picture appears while the handle is down and goes when it is let go.
fn scrub_layer(app: &App) -> Element<'_, Message> {
    let blank = || {
        container(space::horizontal())
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };
    if !app.scrubbing {
        // Each scrub session re-solves its anchor once; a stale frame must
        // not survive a layout change between sessions.
        app.scrub_anchor_cache.borrow_mut().take();
        return blank();
    }
    let slide = app.state.preview();
    // The deck's thumbnail, not a fresh render at card size. Dragging a
    // slider produces a new slide every few milliseconds; asking the renderer
    // for each one would queue work faster than it could be done, and arrive
    // after the presenter had moved on. The warmed picture is already there,
    // and shown a little larger than it was rendered — slightly soft, and
    // instant, which is the right trade for something you look at for a
    // second while your hand is moving.
    let Some(handle) = app.thumbnails.get(slide) else {
        return blank();
    };

    let label = format!("{} / {}", slide + 1, app.state.slide_count());
    let aspect = app.slide_aspect();
    // The pane is found now; where it lands is a question of pixels, and the
    // pixels are only known inside the closure. `compute` works in points —
    // split gaps are subtracted in points — so it must be given the real
    // size, not a unit square.
    let slider = slider_cell(app);
    // Borrowed, and memoised by panel size: the slider's pane only moves
    // when the window resizes or the layout changes, and re-solving the
    // whole layout per pass — while the presenter is dragging, exactly when
    // the frame budget matters — was the scrub layer's entire cost.
    let root = &app.active_layout.root;
    let anchor_cache = &app.scrub_anchor_cache;

    responsive(move |size| {
        let anchor = slider.and_then(|id| {
            let key = (size.width.to_bits(), size.height.to_bits());
            if let Some((cached, frame)) = *anchor_cache.borrow() {
                if cached == key {
                    return frame;
                }
            }
            let area = crate::layout::Frame::new(0.0, 0.0, size.width, size.height);
            let (placements, _) = crate::layout::tree::compute(root, area, true);
            let frame = placements
                .into_iter()
                .find(|placement| placement.id == id)
                .map(|placement| placement.frame);
            *anchor_cache.borrow_mut() = Some((key, frame));
            frame
        });
        let width = (SCRUB_PREVIEW_WIDTH as f32).min(size.width * 0.6).max(1.0);
        // Estimated rather than measured — the picture, the reading beneath
        // it, and the card's own padding — and used only to place the card,
        // where being a few pixels out cannot be seen.
        let height = width / aspect + type_scale::BODY * 1.4 + gap::S * 3.0;

        let card = container(
            column![
                container(
                    image(handle.clone())
                        .content_fit(ContentFit::Contain)
                        .width(Length::Fixed(width))
                )
                .style(theme::ambient::canvas),
                text(label.clone())
                    .size(type_scale::BODY)
                    .color(theme::ambient::text()),
            ]
            .spacing(gap::S)
            .align_x(Alignment::Center),
        )
        .padding(gap::S)
        .style(theme::ambient::dialog);

        // Beside the control that summoned it: centred over the slider's
        // pane, sitting just above it — or just below when the slider lives
        // at the top of the screen. Only without a slider in the layout
        // (scrubbing by keyboard, say) does it fall back to the centre.
        let (left, top) = match anchor {
            Some(pane) => {
                let pane_top = pane.y;
                let pane_bottom = pane.y + pane.height;
                let centred = pane.x + pane.width / 2.0 - width / 2.0;
                let left = centred.clamp(gap::S, (size.width - width - gap::S).max(gap::S));
                let above = pane_top - height - gap::S;
                let top = if above >= gap::S {
                    above
                } else {
                    (pane_bottom + gap::S).min((size.height - height - gap::S).max(gap::S))
                };
                (left, top)
            }
            None => (
                ((size.width - width) / 2.0).max(0.0),
                ((size.height - height) / 2.0).max(0.0),
            ),
        };

        container(card)
            .padding(iced::Padding {
                top,
                left,
                right: 0.0,
                bottom: 0.0,
            })
            .into()
    })
    .into()
}

/// The cell holding the slider, if the active layout has one.
fn slider_cell(app: &App) -> Option<crate::layout::NodeId> {
    app.active_layout.root.cells().into_iter().find_map(|cell| {
        (cell.widget.as_ref()?.kind() == crate::widgets::WidgetKind::SlideSlider).then_some(cell.id)
    })
}

/// Wide enough to recognise a slide by, small enough not to cover the screen.
const SCRUB_PREVIEW_WIDTH: u32 = 420;

/// The overview's scrolling grid, named so the keyboard can scroll it to
/// keep the selected page on screen.
pub fn overview_scrollable() -> iced::widget::Id {
    iced::widget::Id::new("overview-grid")
}

/// The whole deck as thumbnails: pick the one you want by looking at it.
///
/// Over the presenter screen rather than beside it, because it is a thing you
/// open, use and dismiss, and while it is open it is the only thing you are
/// doing. Anywhere off the grid closes it, as does the key that opened it.
fn overview(app: &App) -> Element<'_, Message> {
    let count = app.state.slide_count();
    let committed = app.state.committed();
    // What the arrow keys have landed on, and what Return would pick. It is
    // the preview, so looking around the grid never moves the audience.
    let selected = app.state.preview();
    let ready = app.thumbnails.len();
    let scroll = app.overview_scroll;

    // Only the rows on screen are built. A five-hundred-page deck is five
    // hundred buttons and five hundred images otherwise, laid out on every
    // view pass whether or not anyone can see them; with this the work is
    // the size of the window, not the size of the deck. The rows above and
    // below become plain spacers, so the scrollbar still measures the whole
    // grid.
    let aspect = app.slide_aspect();

    let grid = responsive(move |size| {
        let plan = grid_plan(count, size, aspect);
        let row_height = plan.cell_height + gap::S;

        // The keyboard moves about this grid, and only here is its shape
        // known: how wide a row is, and how much of it fits on screen.
        app.overview_grid.set(crate::app::OverviewGrid {
            columns: plan.columns,
            row_height,
            viewport_height: size.height,
        });

        // The first responsive pass exists to measure the grid. Do not show
        // row zero while the next tick is positioning the actual selection.
        if app.overview_is_positioning() {
            return space::vertical()
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }

        let first = ((scroll / row_height).floor() as usize).saturating_sub(OVERSCAN_ROWS);
        let visible = (size.height / row_height).ceil() as usize + OVERSCAN_ROWS * 2 + 1;
        let last = (first + visible).min(plan.rows);

        let mut content = Column::new().spacing(gap::S);
        if first > 0 {
            content = content.push(
                space::horizontal().height(Length::Fixed(first as f32 * row_height - gap::S)),
            );
        }
        for row_index in first..last {
            let mut line = Row::new().spacing(gap::S);
            for column in 0..plan.columns {
                let slide = row_index * plan.columns + column;
                if slide >= count {
                    break;
                }
                line = line.push(thumbnail_cell(
                    app,
                    slide,
                    slide == selected,
                    slide == committed,
                    &plan,
                ));
            }
            content = content.push(line);
        }
        if last < plan.rows {
            content = content.push(space::horizontal().height(Length::Fixed(
                (plan.rows - last) as f32 * row_height - gap::S,
            )));
        }

        crate::widgets::scroll::vertical(content)
            .id(overview_scrollable())
            .height(Length::Fill)
            .on_scroll(|viewport| Message::OverviewScrolled(viewport.absolute_offset().y))
            .into()
    });

    // How far along the warming is, but only while there is something to
    // say: a bare count of pages is noise once they have all arrived.
    let progress: Element<'_, Message> = if ready < count {
        text(format!("{ready} of {count} slides ready"))
            .size(type_scale::CAPTION)
            .color(theme::ambient::muted())
            .into()
    } else {
        text(format!("{count} slides"))
            .size(type_scale::CAPTION)
            .color(theme::ambient::muted())
            .into()
    };

    let body = column![
        row![
            text("Slide overview").size(type_scale::TITLE),
            space::horizontal(),
            progress,
            button(theme::icon::icon(theme::Icon::Close, type_scale::BODY))
                .padding(gap::XS)
                .style(theme::ambient::tool_button)
                .on_press(Message::ToggleOverview),
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center),
        grid,
    ]
    .spacing(gap::S)
    .padding(gap::M);

    // A press anywhere outside the grid dismisses it.
    mouse_area(
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::ambient::dialog),
    )
    .on_right_press(Message::ToggleOverview)
    .into()
}

/// One page in the grid.
fn thumbnail_cell<'a>(
    app: &'a App,
    slide: usize,
    // The page the grid has landed on: what Return would pick.
    selected: bool,
    // The page the audience is on.
    current: bool,
    plan: &GridPlan,
) -> Element<'a, Message> {
    let inner: Element<'a, Message> = match app.thumbnails.get(slide) {
        Some(handle) => image(handle)
            .content_fit(ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fixed(plan.picture_height))
            .into(),
        // Not rendered yet. A numbered blank of the same size keeps the
        // grid's shape, so pages do not jump about under the pointer as the
        // pictures arrive.
        None => container(
            text(format!("{}", slide + 1))
                .size(type_scale::BODY)
                .color(theme::ambient::muted()),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fixed(plan.picture_height))
        .style(theme::ambient::canvas)
        .into(),
    };

    button(
        column![
            inner,
            text(format!("{}", slide + 1))
                .size(type_scale::CAPTION)
                .color(if current {
                    theme::ambient::accent()
                } else {
                    theme::ambient::muted()
                }),
        ]
        .spacing(gap::XS)
        .align_x(Alignment::Center),
    )
    .padding(gap::XS)
    .width(Length::Fixed(plan.cell_width))
    .height(Length::Fixed(plan.cell_height))
    // The filled cell is the one Return would pick; the accented number is
    // the page the audience is looking at. They are the same cell until the
    // arrow keys move away from it.
    .style(if selected {
        theme::ambient::selected_button
    } else {
        theme::ambient::tool_button
    })
    .on_press(Message::GoToFromOverview(slide))
    .into()
}

/// How many rows beyond the viewport are built, so a fast scroll does not
/// show a band of nothing before the next row is asked for.
const OVERSCAN_ROWS: usize = 2;

/// The smallest a page may be drawn at.
///
/// Below about this width a slide stops being a slide and becomes a grey
/// rectangle with a smudge on it: a 4:3 page at 160pt shows a 24pt title at
/// roughly 8pt, which is the last size at which you can still tell "the one
/// with the graph" from "the one with the table". Rather than shrink past it
/// to fit more in, the grid scrolls.
const MIN_CELL_WIDTH: f32 = 160.0;

/// The largest. The pictures are rendered at [`THUMBNAIL_WIDTH`]; drawing
/// them much beyond that is upscaling, and a soft picture looks like a fault
/// rather than a choice. A short deck gets big, sharp cells and stops there.
const MAX_CELL_WIDTH: f32 = 300.0;

/// The caption strip and the button's own padding, on top of the picture.
const CELL_CHROME: f32 = 26.0;

/// How the grid is laid out for one viewport: as large as the pages can be
/// drawn while still fitting, down to the readable floor.
pub struct GridPlan {
    columns: usize,
    rows: usize,
    cell_width: f32,
    cell_height: f32,
    picture_height: f32,
}

/// Fit `count` pages into `size` as generously as they will go.
///
/// A dozen slides in a wide window should fill it, not sit in a corner at
/// postage-stamp size; two hundred should stop shrinking at the point they
/// become unreadable and scroll instead. So: try one column, then two, then
/// three — each is smaller than the last — and take the first arrangement
/// whose rows fit the height. If none does, use the floor and let it scroll.
fn grid_plan(count: usize, size: iced::Size, aspect: f32) -> GridPlan {
    let count = count.max(1);
    let aspect = if aspect.is_finite() && aspect > 0.1 {
        aspect
    } else {
        16.0 / 9.0
    };
    let plan_for = |columns: usize, width: f32| {
        let picture_height = (width / aspect).max(1.0);
        let cell_height = picture_height + CELL_CHROME;
        GridPlan {
            columns,
            rows: count.div_ceil(columns),
            cell_width: width,
            cell_height,
            picture_height,
        }
    };

    for columns in 1..=count {
        let available = size.width - gap::S * (columns.saturating_sub(1)) as f32;
        let width = available / columns as f32;
        if width < MIN_CELL_WIDTH {
            break;
        }
        let plan = plan_for(columns, width.min(MAX_CELL_WIDTH));
        let total = plan.rows as f32 * (plan.cell_height + gap::S) - gap::S;
        if total <= size.height {
            return plan;
        }
    }

    // Nothing fits without scrolling: as many readable cells as the width
    // holds, and a scrollbar.
    let columns = (((size.width + gap::S) / (MIN_CELL_WIDTH + gap::S)).floor() as usize).max(1);
    let available = size.width - gap::S * (columns.saturating_sub(1)) as f32;
    let width = (available / columns as f32).max(MIN_CELL_WIDTH);
    plan_for(columns, width.min(MAX_CELL_WIDTH))
}

/// Whatever the layout does not carry itself.
///
/// The menu button and the audience lifecycle controls are widgets now, to be
/// placed where the presenter wants them. A layout that places neither — every
/// layout written before they existed — still has to be able to open a menu
/// and start a projector, so the strip stays for exactly the halves that are
/// missing and disappears as they are placed.
fn presenter_toolbar(app: &App) -> Option<Element<'_, Message>> {
    let mut strip = Row::new().align_y(Alignment::Center);
    let mut anything = false;
    if !placed(app, crate::widgets::WidgetKind::MainMenu) {
        strip = strip.push(menu_button(app));
        anything = true;
    }
    // A document layout gets no Start and Stop unless it asks for them. The
    // reader is a window onto a file, not a talk: a projector control there is
    // a control for something that is not happening, and it costs the page the
    // height of a button.
    let presenting =
        crate::layout::PrimaryViewer::of(&app.active_layout) == crate::layout::PrimaryViewer::Slide;
    if presenting && !placed(app, crate::widgets::WidgetKind::AudienceControls) {
        strip = strip.push(audience_lifecycle_controls(app));
        anything = true;
    }
    anything.then(|| strip.into())
}

/// Does the layout carry this widget itself?
fn placed(app: &App, kind: crate::widgets::WidgetKind) -> bool {
    app.active_layout
        .widgets()
        .iter()
        .any(|widget| widget.kind() == kind)
}

/// How far down a flyout hangs: below the strip when there is one, and from
/// the top of the window when the layout carries the control instead.
fn flyout_top(app: &App, kind: crate::widgets::WidgetKind) -> f32 {
    if placed(app, kind) {
        gap::S
    } else {
        theme::controls::BUTTON_HEIGHT + gap::S * 2.0
    }
}

fn menu_button(app: &App) -> Element<'_, Message> {
    let glyph = if app.menu_open {
        theme::Icon::Close
    } else {
        theme::Icon::Menu
    };
    container(
        button(theme::icon::icon(glyph, 18.0))
            .width(Length::Fixed(theme::controls::BUTTON_HEIGHT))
            .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
            .style(theme::controls::selectable(
                app.theme.palette,
                app.menu_open,
            ))
            .on_press(Message::ToggleMenu),
    )
    .padding(gap::S)
    .into()
}

/// A conventional split Start button: the broad side performs the default,
/// while the arrow exposes deliberate placement variants.
fn audience_lifecycle_controls(app: &App) -> Element<'_, Message> {
    const CONTROL_WIDTH: f32 = 112.0;
    const START_LABEL_WIDTH: f32 = CONTROL_WIDTH - theme::controls::BUTTON_HEIGHT;
    // Larger than the usual label so the words carry the button, while still
    // leaving a margin of empty space inside it.
    const LIFECYCLE_LABEL: f32 = 16.0;
    // Once the audience is running the arrow does nothing, so the control
    // becomes one undivided button and its label sits in the middle of it
    // rather than in the middle of the narrower left half.
    let started = app.audience_started;
    let start = button(
        text(if started { "Started" } else { "Start" })
            .size(LIFECYCLE_LABEL)
            .center(),
    )
    .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
    .width(Length::Fixed(if started {
        CONTROL_WIDTH
    } else {
        START_LABEL_WIDTH
    }))
    .style(move |base, status| {
        let palette = app.theme.palette;
        if started {
            theme::controls::filled_tonal(palette)(base, status)
        } else {
            theme::controls::split_left(palette)(base, status)
        }
    })
    .on_press(Message::StartAudience);

    let stop = button(text("Stop").size(LIFECYCLE_LABEL).center())
        .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
        .width(Length::Fixed(CONTROL_WIDTH))
        .style(theme::controls::filled_tonal(app.theme.palette))
        .on_press(Message::StopAudience);

    let start_dropdown = if started {
        row![start]
    } else {
        let arrow = button(theme::icon::icon(
            theme::Icon::ChevronDown,
            type_scale::BODY,
        ))
        .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
        .width(Length::Fixed(theme::controls::BUTTON_HEIGHT))
        .style(theme::controls::split_right(app.theme.palette))
        .on_press(Message::ToggleAudienceStartMenu);
        row![start, arrow]
    }
    .spacing(0);

    container(row![start_dropdown, stop].spacing(gap::XS))
        .padding(iced::Padding::from([gap::S, gap::S]))
        .into()
}

/// Display choices and alternate Start actions. Choosing a display both saves
/// it as the new default and starts the audience immediately.
fn audience_start_menu(app: &App) -> Element<'_, Message> {
    const PANEL_WIDTH: f32 = 320.0;
    let palette = app.theme.palette;
    let option = |label: String, selected: bool, message| {
        button(text(label).size(type_scale::LABEL))
            .width(Length::Fill)
            .height(Length::Fixed(theme::controls::MENU_ITEM_HEIGHT))
            .padding(iced::Padding::from([0.0, gap::L]))
            .style(theme::controls::selectable(palette, selected))
            .on_press(message)
    };
    let mut items = Column::new()
        .spacing(gap::XS)
        .width(Length::Fixed(PANEL_WIDTH))
        .push(
            text("Start audience on")
                .size(type_scale::LABEL)
                .color(theme::ambient::muted()),
        );

    let automatic = matches!(
        app.coordinator.roles.target(Role::Audience),
        RoleTarget::Auto
    );
    items = items.push(option(
        "Automatic".into(),
        automatic,
        Message::StartAudienceAutomatic,
    ));
    for (index, monitor) in app.coordinator.snapshot.monitors.iter().enumerate() {
        items = items.push(option(
            monitor.label(),
            !automatic && app.resolved_index(Role::Audience) == Some(index),
            Message::StartAudienceOnDisplay { monitor: index },
        ));
    }
    items = items.push(option(
        "Start windowed".into(),
        false,
        Message::StartAudienceWindowed,
    ));

    let panel = container(items)
        .padding(gap::M)
        .style(theme::controls::menu_surface(palette));

    let dismiss = |element| mouse_area(element).on_press(Message::CloseMenu);
    let toolbar_height = flyout_top(app, crate::widgets::WidgetKind::AudienceControls);
    // Hamburger button and its two-sided padding, plus the Start control's
    // left padding — unless the layout carries one or both itself, in which
    // case the panel hangs from the corner rather than from a strip that is
    // not there.
    let left = if placed(app, crate::widgets::WidgetKind::AudienceControls) {
        gap::S
    } else if placed(app, crate::widgets::WidgetKind::MainMenu) {
        gap::S * 2.0
    } else {
        theme::controls::BUTTON_HEIGHT + gap::S * 3.0
    };
    column![
        space::vertical().height(Length::Fixed(toolbar_height)),
        row![
            dismiss(space::horizontal().width(Length::Fixed(left))),
            panel,
            dismiss(space::horizontal().width(Length::Fill)),
        ],
        dismiss(space::vertical().height(Length::Fill)),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Wide enough for a shortcut label beside the longest entry.
const MENU_WIDTH: f32 = 300.0;
/// Every menu row is exactly this tall, and the heading exactly that: the
/// flyout is positioned by arithmetic, so the rows cannot be free-sized.
const MENU_ROW: f32 = target::MINIMUM;
const MENU_HEADER: f32 = 20.0;

/// The main menu: the handful of commands that are not on the layout.
fn menu(app: &App) -> Element<'_, Message> {
    let entry = |label: &'static str, shortcut: Option<String>, message: Message| {
        let mut row = Row::new()
            .spacing(gap::M)
            .align_y(Alignment::Center)
            .push(text(label).size(type_scale::BODY));
        if let Some(shortcut) = shortcut {
            row = row.push(space::horizontal()).push(
                text(shortcut)
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::muted()),
            );
        }
        button(container(row).center_y(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fixed(MENU_ROW))
            .padding(iced::Padding::from([0.0, gap::S]))
            .style(theme::ambient::tool_button)
            .on_press(Message::MenuAction(Box::new(message)))
    };
    // Every shortcut here is read from the keymap, so a rebinding shows up in
    // the menu and a moved default cannot leave a stale key behind.
    let shortcut = |action: Action| app.action_shortcut(action);
    let heading = |label: &'static str| {
        container(
            text(label)
                .size(type_scale::CAPTION)
                .color(theme::ambient::muted()),
        )
        .padding(iced::Padding {
            top: gap::S,
            left: gap::S,
            ..iced::Padding::from(0.0)
        })
    };

    let mut items = Column::new()
        .spacing(gap::XS)
        .width(Length::Fixed(MENU_WIDTH));
    items = items.push(
        container(
            text("pulpit")
                .size(type_scale::LABEL)
                .color(theme::ambient::muted()),
        )
        .height(Length::Fixed(MENU_HEADER)),
    );
    items = items.push(heading("File"));
    items = items.push(entry(
        "Open…",
        shortcut(Action::OpenDocument),
        Message::OpenDialog,
    ));
    items = items.push(entry(
        "Reload",
        shortcut(Action::ReloadDocument),
        Message::Do(Action::ReloadDocument),
    ));
    if app.state.document().is_some() && app.platform.capabilities.native_dialogs {
        items = items.push(entry("Show in file manager", None, Message::RevealDocument));
    }
    items = items.push(heading("View"));
    if app.state.document().is_some() {
        items = items.push(entry(
            "Jump to slide…",
            shortcut(Action::ShowOverview),
            Message::Do(Action::ShowOverview),
        ));
    }
    items = items.push(entry(
        "Layouts…",
        shortcut(Action::ShowLayouts),
        Message::ShowLibrary,
    ));
    items = items.push(entry("Settings…", None, Message::ShowSettings));
    items = items.push(heading("Presentation"));
    items = items.push(entry(
        "Swap displays",
        shortcut(Action::SwapDisplays),
        Message::Do(Action::SwapDisplays),
    ));
    items = items.push(entry(
        if crate::layout::PrimaryViewer::of(&app.active_layout)
            == crate::layout::PrimaryViewer::Document
        {
            if app.reader_fullscreen {
                "Page: fullscreen"
            } else {
                "Page: windowed"
            }
        } else if app.coordinator.roles.audience_fullscreen {
            "Audience: fullscreen"
        } else {
            "Audience: windowed"
        },
        shortcut(Action::ToggleAudienceFullscreen),
        Message::Do(Action::ToggleAudienceFullscreen),
    ));
    items = items.push(heading("Timer"));
    // The timer has no control of its own unless a clock widget is on the
    // layout, so its two commands are always reachable from here as well.
    items = items.push(entry(
        if app.state.timer().is_running() {
            "Pause timer"
        } else {
            "Start timer"
        },
        shortcut(Action::ToggleTimer),
        Message::Do(Action::ToggleTimer),
    ));
    items = items.push(entry(
        "Reset timer",
        shortcut(Action::ResetTimer),
        Message::Do(Action::ResetTimer),
    ));

    items = items.push(heading("Help"));
    items = items.push(entry(
        "Keyboard shortcuts…",
        shortcut(Action::ShowShortcuts),
        Message::ToggleShortcuts,
    ));
    items = items.push(entry("Documentation", None, Message::OpenDocumentation));
    items = items.push(entry("Diagnostics…", None, Message::ShowSettings));
    items = items.push(entry("About Pulpit", None, Message::ShowAbout));

    items = items.push(
        container(space::vertical().height(Length::Fixed(1.0)))
            .width(Length::Fill)
            .style(theme::ambient::separator),
    );
    items = items.push(entry(
        "Exit",
        shortcut(Action::Quit),
        Message::Do(Action::Quit),
    ));

    let panel = container(
        container(items)
            .padding(gap::M)
            .style(theme::ambient::dialog),
    )
    .padding(iced::Padding {
        left: gap::S,
        ..iced::Padding::from(0.0)
    });

    // A press anywhere that is not the menu closes it. Those areas are
    // separate dismissal regions rather than one layer underneath the panel:
    // a full-screen layer under it swallows presses meant for the entries.
    // (Escape closes it too; that is handled in the update loop.)
    let dismiss = |element| mouse_area(element).on_press(Message::CloseMenu);
    let spacer = |height: f32| {
        dismiss(
            container(space::vertical())
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
    };
    let rest = || {
        dismiss(
            container(space::vertical())
                .width(Length::Fill)
                .height(Length::Fill),
        )
    };
    let beside = dismiss(
        container(space::horizontal())
            .width(Length::Fill)
            .height(Length::Fill),
    );

    // The menu hangs below the button's strip.
    let above = flyout_top(app, crate::widgets::WidgetKind::MainMenu);
    Row::new()
        .push(
            column![spacer(above), panel, rest()]
                .width(Length::Fixed(MENU_WIDTH + gap::M * 2.0 + gap::S)),
        )
        .push(beside)
        .into()
}

/// The complete live keymap, grouped by the work each command performs.
fn shortcuts_overlay(app: &App, dismissable: bool) -> Element<'_, Message> {
    const GROUPS: [(&str, &[Action]); 6] = [
        (
            "Navigation",
            &[
                Action::Next,
                Action::Previous,
                Action::First,
                Action::Last,
                Action::ShowOverview,
            ],
        ),
        (
            "Preview",
            &[
                Action::PreviewNext,
                Action::PreviewPrevious,
                Action::CommitPreview,
                Action::CancelPreview,
            ],
        ),
        (
            "Presentation",
            &[
                Action::Blank,
                Action::BlankAlternate,
                Action::ToggleTimer,
                Action::ResetTimer,
                Action::SwapDisplays,
                Action::ToggleAudienceFullscreen,
            ],
        ),
        (
            "Annotations",
            &[
                Action::AnnotateInk,
                Action::AnnotateHighlighter,
                Action::AnnotateEraser,
                Action::AnnotatePointer,
                Action::UndoAnnotation,
                Action::RedoAnnotation,
                Action::ClearAnnotations,
                Action::ToggleAnnotationAudience,
            ],
        ),
        (
            "Reader",
            &[
                Action::ToggleReader,
                Action::ToggleOutline,
                Action::FocusSearch,
                Action::FindNext,
                Action::FindPrevious,
                Action::ZoomIn,
                Action::ZoomOut,
                Action::ZoomReset,
                Action::FitPage,
                Action::FitWidth,
                Action::RotateReader,
                Action::ToggleDualPage,
                Action::FocusNextLink,
                Action::FocusPreviousLink,
            ],
        ),
        (
            "Application",
            &[
                Action::OpenDocument,
                Action::ReloadDocument,
                Action::ShowLayouts,
                Action::ShowShortcuts,
                Action::Quit,
            ],
        ),
    ];

    let group = |name: &'static str, actions: &'static [Action]| {
        let mut content = Column::new().spacing(gap::XS).push(
            text(name)
                .size(type_scale::LABEL)
                .color(theme::ambient::text()),
        );
        for action in actions {
            let keys = app.action_shortcuts(*action);
            let keys = if keys.is_empty() {
                "Unbound".to_string()
            } else {
                keys.join("  ·  ")
            };
            content = content.push(
                row![
                    text(action.label()).size(type_scale::CAPTION),
                    space::horizontal(),
                    text(keys)
                        .size(type_scale::CAPTION)
                        .color(theme::ambient::muted()),
                ]
                .spacing(gap::M),
            );
        }
        container(content).width(Length::Fill).padding(gap::S)
    };

    let first = Column::new()
        .spacing(gap::M)
        .push(group(GROUPS[0].0, GROUPS[0].1))
        .push(group(GROUPS[3].0, GROUPS[3].1));
    let second = Column::new()
        .spacing(gap::M)
        .push(group(GROUPS[1].0, GROUPS[1].1))
        .push(group(GROUPS[4].0, GROUPS[4].1));
    let third = Column::new()
        .spacing(gap::M)
        .push(group(GROUPS[2].0, GROUPS[2].1))
        .push(group(GROUPS[5].0, GROUPS[5].1));
    let title = if dismissable {
        "Keyboard shortcuts"
    } else {
        "Open a PDF — or start from the keyboard"
    };
    let mut header = Row::new()
        .align_y(Alignment::Center)
        .push(text(title).size(type_scale::TITLE));
    if dismissable {
        header = header.push(space::horizontal()).push(
            button(text("Close"))
                .style(theme::ambient::tool_button)
                .on_press(Message::CloseShortcuts),
        );
    } else {
        header = header.push(space::horizontal()).push(
            button(text("Open…"))
                .style(theme::ambient::forward_button)
                .on_press(Message::OpenDialog),
        );
    }
    let mut content = Column::new().spacing(gap::M).push(header).push(
        row![
            first.width(Length::Fill),
            second.width(Length::Fill),
            third.width(Length::Fill),
        ]
        .spacing(gap::L),
    );
    if dismissable {
        content = content.push(
            container(
                button(text("Customize shortcuts…"))
                    .style(theme::ambient::tool_button)
                    .on_press(Message::ShowSettings),
            )
            .width(Length::Fill)
            .align_x(Alignment::End),
        );
    }
    let card = container(content)
        .padding(gap::L)
        .max_width(1100.0)
        .style(theme::ambient::dialog);
    let layer = container(scrollable(card).style(theme::ambient::scrollbar))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .padding(gap::L);
    if dismissable {
        let backdrop = mouse_area(
            container(space::vertical())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::ambient::scrim),
        )
        .on_press(Message::CloseShortcuts);
        stack![backdrop, layer].into()
    } else {
        layer.into()
    }
}

fn about_overlay() -> Element<'static, Message> {
    let card = container(
        column![
            text("Pulpit").size(type_scale::TITLE),
            text(format!("Version {}", env!("CARGO_PKG_VERSION"))).size(type_scale::BODY),
            text("A PDF presenter built for unreliable, changing display topologies.")
                .size(type_scale::BODY)
                .color(theme::ambient::muted()),
            button(text("Close"))
                .style(theme::ambient::tool_button)
                .on_press(Message::CloseAbout),
        ]
        .spacing(gap::M),
    )
    .padding(gap::L)
    .max_width(520.0)
    .style(theme::ambient::dialog);
    let backdrop = mouse_area(
        container(space::vertical())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::ambient::scrim),
    )
    .on_press(Message::CloseAbout);
    let layer = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    stack![backdrop, layer].into()
}

/// A toggle that shows whether it is the current choice. Selection uses the
/// accent fill; focus is drawn separately by the focus ring.
fn selectable<'a>(
    button: iced::widget::Button<'a, Message>,
    selected: bool,
) -> iced::widget::Button<'a, Message> {
    if selected {
        button
            .padding(gap::S)
            .style(theme::ambient::selected_button)
    } else {
        button.padding(gap::S).style(theme::ambient::tool_button)
    }
}

/// Corner notices. Sticky ones carry what to do next; routine ones fade.
fn toasts(app: &App) -> Option<Element<'_, Message>> {
    if app.toasts.is_empty() {
        return None;
    }
    let mut stack_of_toasts = Column::new().spacing(gap::S).align_x(Alignment::End);
    for toast in app.toasts.iter() {
        let intent = match toast.intent {
            Intent::Info => theme::ambient::accent(),
            Intent::Warning | Intent::Error => theme::ambient::alert(),
        };
        let mut body = Column::new().spacing(gap::XS).push(
            row![
                // A shape as well as a colour: status is never colour alone.
                container(
                    space::horizontal()
                        .width(Length::Fixed(8.0))
                        .height(Length::Fixed(8.0))
                )
                .style(theme::ambient::dot(intent)),
                text(toast.intent.label())
                    .size(type_scale::CAPTION)
                    .color(intent),
                space::horizontal(),
                button(theme::icon::icon(theme::Icon::Close, type_scale::CAPTION))
                    .padding(gap::XS)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::DismissToast(toast.id)),
            ]
            .spacing(gap::S)
            .align_y(Alignment::Center),
        );
        // Borrowed, not cloned: the element already lives no longer than
        // `app`, and a toast redraws twenty times a second while shown.
        body = body.push(text(toast.message.as_str()).size(type_scale::LABEL));
        if let Some(action) = &toast.action {
            body = body.push(
                text(action.as_str())
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::muted()),
            );
        }
        stack_of_toasts = stack_of_toasts.push(
            container(body)
                .padding(gap::M)
                .width(Length::Fixed(360.0))
                .style(theme::ambient::toast(intent)),
        );
    }

    // One gesture to clear a pile, so a stack of notices is never a chore.
    if app.toasts.iter().count() > 1 {
        // Name what is being cleared when some of it needs a decision.
        let label = match app.toasts.sticky_count() {
            0 => "Dismiss all".to_string(),
            1 => "Dismiss all (1 needs attention)".to_string(),
            many => format!("Dismiss all ({many} need attention)"),
        };
        stack_of_toasts = stack_of_toasts.push(
            button(text(label).size(type_scale::CAPTION))
                .padding(gap::XS)
                .style(theme::ambient::tool_button)
                .on_press(Message::DismissAllToasts),
        );
    }

    // Top-right, not bottom-right: a notice that stays up for as long as its
    // condition is true must not sit on the navigation controls, which live
    // along the bottom of every layout that has them.
    Some(
        container(stack_of_toasts)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::Start)
            .padding(iced::Padding {
                // Clear of the menu button's row.
                top: target::MINIMUM + gap::M,
                right: gap::L,
                bottom: gap::L,
                left: gap::L,
            })
            .into(),
    )
}
/// Translate a widget interaction into an application message.
fn interaction(interaction: crate::widgets::WidgetEvent) -> Message {
    use pulpit_core::Command as Nav;
    match interaction {
        crate::widgets::WidgetEvent::Ignored => Message::Ignore,
        // The two navigation buttons are the only source of these events, and
        // they follow the history: back returns to where a jump was made
        // from, and falls back to stepping when there is nothing to unwind.
        // The keys are bound straight to `Nav::Next`/`Nav::Previous` and stay
        // sequential.
        crate::widgets::WidgetEvent::Next => Message::NavForward,
        crate::widgets::WidgetEvent::Previous => Message::NavBack,
        // Scrubbing moves the presenter's preview only; the audience follows
        // when the slider is released.
        crate::widgets::WidgetEvent::ScrubTo(slide) => Message::Nav(Nav::PreviewGoTo(slide)),
        crate::widgets::WidgetEvent::CommitScrub => Message::Nav(Nav::CommitPreview),
        crate::widgets::WidgetEvent::ShowOverview => Message::ToggleOverview,
        crate::widgets::WidgetEvent::SlideCursor { x, y } => Message::SlideCursor { x, y },
        crate::widgets::WidgetEvent::SlidePressed => Message::SlidePressed,
        crate::widgets::WidgetEvent::Annotate(command) => Message::Annotate(command),
        crate::widgets::WidgetEvent::Read(command) => Message::Read(command),
        crate::widgets::WidgetEvent::Find(command) => Message::Find(command),
        crate::widgets::WidgetEvent::Panel(command) => Message::Panel(command),
        crate::widgets::WidgetEvent::Alarm(command) => Message::Alarm(command),
        crate::widgets::WidgetEvent::Timer(command) => Message::Timer(command),
        crate::widgets::WidgetEvent::ToggleTimer => Message::Nav(Nav::ToggleTimer),
        crate::widgets::WidgetEvent::Transport(request) => Message::Transport(request),
        crate::widgets::WidgetEvent::EndPresentation => Message::Do(Action::Quit),
        crate::widgets::WidgetEvent::Chrome(command) => {
            use crate::widgets::event::ChromeCommand;
            match command {
                ChromeCommand::ToggleMenu => Message::ToggleMenu,
                ChromeCommand::StartAudience => Message::StartAudience,
                ChromeCommand::StopAudience => Message::StopAudience,
                ChromeCommand::ToggleStartMenu => Message::ToggleAudienceStartMenu,
            }
        }
    }
}

/// Settings, displays and diagnostics, taking the whole window.
///
/// A full page rather than a panel that squashes the presentation: the
/// presenter screen is a layout the user designed, and nothing should push it
/// around.
fn settings_page(app: &App) -> Element<'_, Message> {
    // The way out sits above the title, not beside it: one is navigation, the
    // other is where you are. Just the title, too — the desktop, the backend
    // and the palette in use are facts about this session, and they are in
    // "This session" below with the rest of the capability report.
    let header = column![
        button(
            row![
                theme::icon::icon(theme::Icon::ArrowLeft, 18.0),
                text("Back").size(type_scale::BODY)
            ]
            .spacing(gap::S)
            .align_y(Alignment::Center)
        )
        .padding(gap::S)
        .style(theme::ambient::tool_button)
        .on_press(Message::ShowPresenter),
        text("Settings").size(type_scale::TITLE),
    ]
    .spacing(gap::M);

    // A full page, not a squashed drawer: generous rhythm and page margins.
    let mut body = Column::new()
        .spacing(gap::XL)
        .padding(gap::XXL)
        .push(header);

    // Appearance
    let appearance_count = Appearance::ALL.len();
    let mut appearances = Row::new().spacing(-1.0);
    for (index, appearance) in Appearance::ALL.into_iter().enumerate() {
        let selected = app.settings.appearance.appearance == appearance;
        appearances = appearances.push(
            button(text(appearance.label()).size(type_scale::LABEL).center())
                .height(Length::Fixed(theme::controls::BUTTON_HEIGHT))
                .padding(iced::Padding::from([0.0, gap::L]))
                .style(theme::controls::segment(
                    app.theme.palette,
                    selected,
                    index,
                    appearance_count,
                ))
                .on_press(Message::SetAppearance(appearance)),
        );
    }
    // Drawn like every other group on this page: a heading and its controls,
    // no panel behind them. A box around one section and not the rest read as
    // though that section were a different kind of thing.
    let mut theme_section = Column::new().spacing(gap::M).push(appearances);
    // Only a system that cannot answer the question needs a word about it.
    if app.theme.fell_back {
        theme_section = theme_section.push(
            text("This system does not expose a light/dark preference, so the dark palette is in use.")
                .size(type_scale::CAPTION)
                .color(theme::ambient::muted()),
        );
    }
    body = body.push(section("Theme", theme_section.into()));

    body = body.push(section("Colors", color_editor(app)));

    // Motion. The desktop's own preference is followed by default; the
    // override is here because a presenter may want an author's animated
    // slide to play on stage even on a machine set to reduce motion.
    let mut motions = Row::new().spacing(gap::S);
    for setting in crate::platform::MotionSetting::ALL {
        motions = motions.push(
            selectable(
                button(text(setting.label()).size(type_scale::LABEL)),
                app.settings.appearance.motion == setting,
            )
            .on_press(Message::SetMotion(setting)),
        );
    }
    body = body.push(section(
        "Motion",
        column![
            motions,
            text(format!(
                "Currently {}. Reducing motion stops animated slide content \
                 from starting on its own; it can still be played from the \
                 presenter controls.",
                if app.motion.is_reduced() {
                    "reduced"
                } else {
                    "unrestricted"
                }
            ))
            .size(type_scale::CAPTION)
            .color(theme::ambient::muted()),
        ]
        .spacing(gap::S)
        .into(),
    ));

    // Blanking. Which colour is wanted is a property of the room, not the
    // deck: black vanishes in a dark hall, white reads as deliberate under
    // bright house lights.
    let mut blank_colors = Row::new().spacing(gap::S);
    for color in crate::settings::BlankColor::ALL {
        blank_colors = blank_colors.push(
            selectable(
                button(text(color.label()).size(type_scale::LABEL)),
                app.settings.display.blank_color == color,
            )
            .on_press(Message::SetBlankColor(color)),
        );
    }
    body = body.push(section(
        "Blank screen",
        column![
            blank_colors,
            text(
                "What the blank key turns the audience screen into. \
                 Both colours stay available as separate shortcuts."
            )
            .size(type_scale::CAPTION)
            .color(theme::ambient::muted()),
        ]
        .spacing(gap::S)
        .into(),
    ));

    // Displays are chosen from the menu, where they are reachable mid-talk;
    // repeating them here would be a second place to keep in step.

    // What this desktop can and cannot do.
    let mut limitations = Column::new().spacing(gap::XS);
    for line in app.platform.capabilities.report() {
        limitations = limitations.push(
            text(line)
                .size(type_scale::CAPTION)
                .color(theme::ambient::muted()),
        );
    }
    body = body.push(section("This session", limitations.into()));

    body = body.push(section("Notes mapping", mappings(app)));

    // Diagnostics. Rebuilt at most once a second: the report is a multi-KB
    // string whose paragraph iced re-shapes whenever its content changes,
    // and building it per view pass re-shaped it twenty times a second for
    // the whole time the page was open.
    let report = app.diagnostics_report();
    // The report scrolls against its own right edge — padding goes on the
    // text, not the container, so there is no dead strip beside the bar — and
    // the copy button sits in the corner of the box rather than above it.
    let copy = container(
        button(text("Copy").size(type_scale::CAPTION))
            .padding(gap::XS)
            .style(theme::ambient::tool_button)
            .on_press(Message::CopyDiagnostics),
    )
    .width(Length::Fill)
    .align_x(Alignment::End)
    .padding(gap::S);
    body = body.push(section(
        "Diagnostics",
        container(stack![
            scrollable(
                container(text(report).size(type_scale::CAPTION)).padding(iced::Padding {
                    right: gap::XL,
                    ..iced::Padding::from(gap::S)
                })
            )
            .height(Length::Fixed(260.0))
            .width(Length::Fill)
            .style(theme::ambient::scrollbar),
            copy,
        ])
        .style(theme::ambient::surface)
        .into(),
    ));

    scrollable(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::ambient::scrollbar)
        .into()
}

fn color_editor(app: &App) -> Element<'_, Message> {
    let scheme = app.editing_colors;
    let colors = &app.settings.appearance.colors;
    let palette = colors.palette(scheme);

    // An ordinary control, drawn like the ordinary controls around it: the
    // press only opens a question, and the red belongs on the answer to it.
    let reset = button(text("Reset to Pulpit defaults…").size(type_scale::CAPTION))
        .padding(gap::S)
        .style(theme::ambient::tool_button)
        .on_press_maybe(
            (colors.has_overrides() || !app.color_drafts.is_empty())
                .then_some(Message::AskResetColors),
        );

    let mut roles = Column::new().spacing(gap::M);
    for role in crate::theme::ColorRole::ALL {
        let value = app
            .color_drafts
            .get(&(scheme, role))
            .cloned()
            .unwrap_or_else(|| colors.value(scheme, role));
        let parsed = crate::settings::parse_hex_color(&value);
        let swatch = parsed.unwrap_or_else(|| palette.color(role));
        let field = text_input("#RRGGBB", &value)
            .on_input(move |value| Message::SetColor(role, value))
            .style(theme::ambient::text_field)
            .width(Length::Fixed(124.0));
        let mut role_row = column![row![
            column![
                text(role.label()).size(type_scale::BODY),
                text(role.description())
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::muted()),
            ]
            .spacing(gap::XS)
            // Wide enough for the longest description, no wider: the swatch
            // and field read as part of the row, not pinned to the far edge.
            .width(Length::Fixed(360.0)),
            // The swatch is the wheel's handle. Typing `#RRGGBB` stays the
            // way to reproduce a colour exactly — a brand hex out of a style
            // guide is *given*, not chosen — and the wheel is the way to
            // choose one, which a hex field is a poor instrument for.
            role_swatch(app, role, swatch),
            field,
        ]
        .spacing(gap::M)
        .align_y(Alignment::Center)]
        .spacing(gap::XS);
        if parsed.is_none() {
            role_row = role_row.push(
                text("Use a six-digit HEX color such as #C9CCD4.")
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::alert()),
            );
        } else if let Some(warning) = contrast_warning(palette, role) {
            role_row = role_row.push(
                text(warning)
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::alert()),
            );
        }
        roles = roles.push(role_row);
    }

    column![
        row![
            text("Edit palette").size(type_scale::LABEL),
            pick_list(
                crate::settings::ColorScheme::ALL,
                Some(scheme),
                Message::EditColorScheme,
            )
            .width(Length::Fixed(140.0))
            .style(theme::ambient::drop_down)
            .menu_style(theme::ambient::drop_down_menu),
            // Beside the palette it resets, not pushed to the far edge: the
            // two controls are about the same set of colours, and a button
            // alone across the page reads as belonging to the page.
            reset,
            space::horizontal().width(Length::Fill),
        ]
        .spacing(gap::M)
        .align_y(Alignment::Center),
        text("Every view and widget inherits these roles. High contrast remains controlled by the system.")
            .size(type_scale::CAPTION)
            .color(theme::ambient::muted()),
        roles,
    ]
    .spacing(gap::M)
    .into()
}

fn contrast_warning(
    palette: crate::theme::Palette,
    role: crate::theme::ColorRole,
) -> Option<&'static str> {
    use crate::theme::tokens::contrast;
    use crate::theme::ColorRole;
    let fails = match role {
        ColorRole::Text => {
            contrast(palette.text, palette.canvas) < 4.5
                || contrast(palette.text, palette.surface) < 4.5
        }
        ColorRole::Muted => contrast(palette.muted, palette.canvas) < 4.5,
        ColorRole::Accent => contrast(palette.accent, palette.canvas) < 3.0,
        ColorRole::Alert => contrast(palette.alert, palette.canvas) < 3.0,
        ColorRole::Canvas | ColorRole::Surface | ColorRole::SlideCanvas => false,
    };
    fails.then_some("This pairing falls below the recommended contrast ratio.")
}

/// The wash over the presenter window while a cue is going off.
///
/// A tint, not a curtain: the slide and the notes stay readable through it,
/// because the moment a presenter is told they are out of time is the moment
/// they most need to see what they were about to say. It carries no controls
/// and handles no events, so presses fall through to the clock beneath it.
fn alarm_flash(alpha: f32) -> Element<'static, Message> {
    let colour = theme::ambient::alert();
    container(space::horizontal())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Color { a: alpha, ..colour }.into()),
            ..container::Style::default()
        })
        .into()
}

/// One role's colour, as a swatch that opens a colour wheel.
fn role_swatch(
    app: &App,
    role: crate::theme::ColorRole,
    current: Color,
) -> Element<'static, Message> {
    let trigger = button(space::horizontal())
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .padding(0)
        .style(theme::color_swatch_button(current, false))
        .on_press(Message::OpenColorPicker(
            (app.color_picker_open != Some(role)).then_some(role),
        ));

    crate::vendor::iced_aw::ColorPicker::new(
        app.color_picker_open == Some(role),
        current,
        trigger,
        Message::OpenColorPicker(None),
        // Back out as the text the field holds, so both ways of setting a
        // colour go down the same path — including the contrast check.
        move |color| Message::SetColor(role, crate::settings::format_hex_color(color)),
    )
    .into()
}

/// The alarm popup: what is set, and how to set another.
///
/// Typed, in the four digits the time already has: a presenter knows they hand
/// off at twenty past two, and "1420" says it in one gesture where a dial took
/// a dozen. The entry is the first thing in the panel rather than behind a
/// second press, because setting an alarm is what the panel is for.
fn alarms_dialog(app: &App) -> Element<'_, Message> {
    use crate::widgets::event::AlarmCommand;
    let controls = &app.alarm_controls;
    let options = crate::widgets::ClockOptions::default();

    let from_now = |label: &'static str, minutes: u32| {
        button(text(label).size(type_scale::LABEL))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::Alarm(AlarmCommand::DraftFromNow(minutes * 60)))
    };

    // What is already set, each with the way to take it off again.
    let mut list = column![].spacing(gap::S);
    if controls.alarms.is_empty() {
        list = list.push(
            text("No alarms set.")
                .size(type_scale::BODY)
                .color(theme::ambient::muted()),
        );
    }
    for alarm in &controls.alarms {
        let passed = alarm.at < crate::view::seconds_of_day();
        list = list.push(
            row![
                text(options.format_alarm(alarm.at))
                    .size(type_scale::BODY)
                    // A cue that has gone by is dimmed rather than removed:
                    // seeing that 14:20 has passed is worth a line.
                    .color(if passed {
                        theme::ambient::muted()
                    } else {
                        theme::ambient::text()
                    }),
                space::horizontal(),
                button(text("Remove").size(type_scale::LABEL))
                    .padding(gap::S)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::Alarm(AlarmCommand::Remove(alarm.at))),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(gap::S),
        );
    }

    let entered = controls.entered();
    let mut add = button(text("Add").size(type_scale::LABEL))
        .padding(gap::S)
        // Not the alert style: adding a cue is the ordinary thing this panel
        // is for, and red is kept for the cue that is going off.
        .style(theme::ambient::selected_button);
    // Nothing to add is nothing to press: an unreadable field greys the
    // control rather than accepting the press and quietly doing nothing.
    if !controls.is_full() && entered.is_some() {
        add = add.on_press(Message::Alarm(AlarmCommand::Add));
    }

    let field = time_picker(
        &controls.entry,
        (&ALARM_HOURS, &ALARM_MINUTES),
        ("00", "00"),
        |field, typed| Message::Alarm(AlarmCommand::Type(field, typed)),
        Message::Alarm(AlarmCommand::Add),
    );

    // AM and PM only mean something while the hour could be either. Past
    // twelve the digits have already said which half of the day this is, so
    // the toggle greys out rather than offering a choice that is not there.
    let ambiguous = controls.hour_is_ambiguous();
    let half = |label: &'static str, afternoon: bool| {
        let chosen = ambiguous && controls.afternoon == afternoon;
        let mut control = button(text(label).size(type_scale::LABEL).color(if ambiguous {
            theme::ambient::text()
        } else {
            theme::ambient::muted()
        }))
        .padding(gap::S)
        .style(if chosen {
            theme::ambient::selected_button
        } else {
            theme::ambient::tool_button
        });
        if ambiguous {
            control = control.on_press(Message::Alarm(AlarmCommand::SetAfternoon(afternoon)));
        }
        control
    };

    // What is typed is not said again beside the button: the fields already
    // read as a time, and an echo of them is a second thing to keep track of.
    // Only the refusal is worth a word, and only while there is one.
    let mut entry_row = row![
        field,
        half("AM", false),
        half("PM", true),
        space::horizontal(),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(gap::S);
    let typing = !controls.entry.left.is_empty() || !controls.entry.right.is_empty();
    if entered.is_none() && typing {
        entry_row = entry_row.push(
            text("not a time")
                .size(type_scale::BODY)
                .color(theme::ambient::alert()),
        );
    }

    // Three questions, in the order they are asked: set one, see what is set,
    // say what happens when one goes off.
    let body = column![
        text("Alarms").size(type_scale::TITLE),
        dialog_section(
            "New alarm",
            column![
                entry_row.push(add),
                row![
                    from_now("in 5m", 5),
                    from_now("in 10m", 10),
                    from_now("in 20m", 20),
                    from_now("in 30m", 30),
                ]
                .spacing(gap::S),
            ]
            .spacing(gap::S),
        ),
        rule(),
        dialog_section("Set", list),
        rule(),
        dialog_section(
            "When one goes off",
            snooze_row(
                controls.snooze_minutes,
                Message::Alarm(AlarmCommand::NudgeSnooze(-1)),
                Message::Alarm(AlarmCommand::NudgeSnooze(1)),
            ),
        ),
        text(if controls.is_full() {
            "That is as many alarms as pulpit will hold."
        } else {
            "Escape or a press outside closes this. A cue that goes off is dismissed with Escape too."
        })
        .size(type_scale::CAPTION)
        .color(theme::ambient::muted()),
    ]
    .spacing(gap::M);

    panel(body, Some(Message::Alarm(AlarmCommand::Open(false))))
}

/// A named group of controls inside a popup.
///
/// A dialog that is one column of rows makes the reader work out for
/// themselves which row answers which question. A quiet caption over each
/// group does that work instead, and costs a line of small muted text: the
/// controls stay exactly where they were, they are simply told apart.
fn dialog_section<'a>(
    title: &'static str,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![
        text(title)
            .size(type_scale::CAPTION)
            .color(theme::ambient::muted()),
        body.into(),
    ]
    .spacing(gap::S)
    .into()
}

/// The halves of the two time pickers, named so the typing can be moved from
/// one to the next without the presenter reaching for Tab.
pub static ALARM_HOURS: std::sync::LazyLock<iced::widget::Id> =
    std::sync::LazyLock::new(|| iced::widget::Id::new("alarm-hours"));
pub static ALARM_MINUTES: std::sync::LazyLock<iced::widget::Id> =
    std::sync::LazyLock::new(|| iced::widget::Id::new("alarm-minutes"));
pub static TIMER_MINUTES: std::sync::LazyLock<iced::widget::Id> =
    std::sync::LazyLock::new(|| iced::widget::Id::new("timer-minutes"));
pub static TIMER_SECONDS: std::sync::LazyLock<iced::widget::Id> =
    std::sync::LazyLock::new(|| iced::widget::Id::new("timer-seconds"));

/// A time typed as two boxes with a colon between them.
///
/// The colon is a real character between two fields rather than punctuation
/// that appears under the typing: it cannot be backspaced over by accident, and
/// each box says by its own width how much it holds. Both pickers are built
/// here so the clock's and the timer's agree down to the digit.
fn time_picker<'a>(
    entry: &crate::widgets::timing::model::TimeEntry,
    ids: (&iced::widget::Id, &iced::widget::Id),
    placeholders: (&'static str, &'static str),
    on_type: fn(crate::widgets::event::TimeField, String) -> Message,
    submit: Message,
) -> Element<'a, Message> {
    use crate::widgets::event::TimeField;
    let half = |id: &iced::widget::Id, placeholder: &'static str, value: &str, field: TimeField| {
        text_input(placeholder, value)
            .id(id.clone())
            .on_input(move |typed| on_type(field, typed))
            // Enter commits from either half: a presenter who typed the hour
            // and meant o'clock should not have to cross the colon first.
            .on_submit(submit.clone())
            .style(theme::ambient::text_field)
            .size(type_scale::TITLE)
            .padding(gap::S)
            .align_x(Alignment::Center)
            .width(Length::Fixed(64.0))
    };
    row![
        half(ids.0, placeholders.0, &entry.left, TimeField::Left),
        text(":")
            .size(type_scale::TITLE)
            .color(theme::ambient::muted()),
        half(ids.1, placeholders.1, &entry.right, TimeField::Right),
    ]
    .spacing(gap::XS)
    .align_y(Alignment::Center)
    .into()
}

/// The hairline between groups, so the same line is drawn the same way
/// wherever a popup wants one.
fn rule<'a>() -> Element<'a, Message> {
    container(space::vertical().height(Length::Fixed(1.0)))
        .style(theme::ambient::separator)
        .into()
}

/// The last line of a popup: whatever the panel offers that is not a way out.
///
/// There is no Done here. The close glyph in the corner and Escape already say
/// it, in the same place for every panel, and a third way out only adds a
/// button that has to be found. So the bar holds the actions that *do*
/// something — a clearing or destructive press, left-aligned, away from the
/// corner the finger goes to when it means "close".
fn dialog_footer<'a>(action: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![
        rule(),
        row![action.into()]
            .spacing(gap::S)
            .align_y(iced::Alignment::Center),
    ]
    .spacing(gap::M)
    .into()
}

/// How long a snooze lasts, offered in both popups because it governs both:
/// the cue put off on the clock and the target pushed out on the timer are the
/// same "give me five more minutes".
fn snooze_row<'a>(minutes: u32, less: Message, more: Message) -> Element<'a, Message> {
    let step = |label: &'static str, message: Message| {
        button(text(label).size(type_scale::LABEL))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(message)
    };
    row![
        text(format!("Snooze for {minutes}m"))
            .size(type_scale::BODY)
            .color(theme::ambient::muted()),
        space::horizontal(),
        step("−1m", less),
        step("+1m", more),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(gap::S)
    .into()
}

/// The timer menu: which way the timer runs, and how long the talk is.
///
/// The clock's popup for the other half of the pair, and dialled for the same
/// reason: this is set at the lectern, sometimes while the room is filling.
fn timer_dialog(app: &App) -> Element<'_, Message> {
    use crate::widgets::event::TimerCommand;
    let controls = &app.timer_controls;

    let direction = |label: &'static str, count_down: bool| {
        let chosen = controls.count_down == count_down;
        button(text(label).size(type_scale::LABEL))
            .padding(gap::S)
            .style(if chosen {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            })
            .on_press(Message::Timer(TimerCommand::SetCountDown(count_down)))
    };
    let step = |label: &'static str, delta: i32| {
        button(text(label).size(type_scale::LABEL))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::Timer(TimerCommand::NudgeTarget(delta)))
    };
    let preset = |label: &'static str, minutes: u32| {
        button(text(label).size(type_scale::LABEL))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::Timer(TimerCommand::SetTarget(minutes * 60)))
    };

    // Typed the way the alarms are typed, one unit down: minutes and seconds
    // in two boxes, because a lightning talk is five minutes *thirty* and
    // rounding it to fit the control is the control's fault, not the talk's.
    let field = time_picker(
        &controls.entry,
        (&TIMER_MINUTES, &TIMER_SECONDS),
        ("00", "00"),
        |field, typed| Message::Timer(TimerCommand::Type(field, typed)),
        Message::Timer(TimerCommand::CommitLength),
    );
    let typed = controls.entered();
    let mut set = button(text("Set").size(type_scale::LABEL))
        .padding(gap::S)
        .style(theme::ambient::selected_button);
    if typed.is_some() && typed != controls.target_seconds {
        set = set.on_press(Message::Timer(TimerCommand::CommitLength));
    }

    let length = match controls.target_seconds {
        Some(seconds) => format!(
            "counting to {}",
            crate::widgets::timing::model::format_length(seconds)
        ),
        None => "open-ended".to_string(),
    };

    // The clock's three questions, in the same shape: which way it runs, how
    // long the talk is, and what happens at the end of it.
    let body = column![
        text("Timer").size(type_scale::TITLE),
        dialog_section(
            "Direction",
            row![
                direction("Count up", false),
                direction("Count down", true),
                space::horizontal(),
            ]
            .spacing(gap::S)
            .align_y(iced::Alignment::Center),
        ),
        rule(),
        dialog_section(
            "Length",
            column![
                row![field, set, space::horizontal(),]
                    .align_y(iced::Alignment::Center)
                    .spacing(gap::S),
                row![
                    text(length)
                        .size(type_scale::BODY)
                        .color(theme::ambient::muted()),
                    space::horizontal(),
                    step("−5m", -5),
                    step("−1m", -1),
                    step("+1m", 1),
                    step("+5m", 5),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(gap::S),
                row![
                    preset("15m", 15),
                    preset("20m", 20),
                    preset("30m", 30),
                    preset("45m", 45),
                    preset("60m", 60),
                ]
                .spacing(gap::S),
            ]
            .spacing(gap::S),
        ),
        rule(),
        // Said plainly, because the alternative is a countdown that refuses
        // to be chosen with no word about why.
        dialog_section(
            "When time runs out",
            snooze_row(
                controls.snooze_minutes,
                Message::Timer(TimerCommand::NudgeSnooze(-1)),
                Message::Timer(TimerCommand::NudgeSnooze(1)),
            ),
        ),
        text("A countdown needs a length to count to; asking for one without a target sets 20 minutes.")
            .size(type_scale::CAPTION)
            .color(theme::ambient::muted()),
        // Clearing the target is the one press here that is not a way out.
        dialog_footer(
            button(text("No target").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::Timer(TimerCommand::ClearTarget)),
        ),
    ]
    .spacing(gap::M);

    panel(body, Some(Message::Timer(TimerCommand::Open(false))))
}

fn reset_colors_dialog() -> Element<'static, Message> {
    let body = column![
        text("Reset colors?").size(type_scale::TITLE),
        text("This will replace your custom Light and Dark colors with the default Pulpit theme.")
            .size(type_scale::BODY),
        row![
            button(text("Cancel").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::CancelResetColors),
            button(text("Reset colors").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::alert_button)
                .on_press(Message::ResetColors),
        ]
        .spacing(gap::S),
    ]
    .spacing(gap::M);

    panel(body, Some(Message::CancelResetColors))
}

/// The offer to follow a jump the open document's JavaScript asked for (§8.6).
///
/// A question rather than an action, and worded in the past tense about what
/// the *document* wanted, so it cannot be mistaken for pulpit proposing to move
/// the reader. The destination is inside the document and reaches nothing
/// outside it, which is why this is offered at all rather than only logged like
/// mail, print and submit.
fn form_navigation_dialog(request: &crate::app::FormNavigation) -> Element<'static, Message> {
    let body = column![
        text("Follow this document's request?").size(type_scale::TITLE),
        text(format!("This form asked to {}.", request.what)).size(type_scale::BODY),
        text("Nothing moves until you choose.").size(type_scale::LABEL),
        row![
            button(text("Stay here").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::DeclineFormNavigation),
            button(text("Go").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::alert_button)
                .on_press(Message::FollowFormNavigation),
        ]
        .spacing(gap::S),
    ]
    .spacing(gap::M);

    // A press on the ground behind declines it: staying where you are is the
    // answer that changes nothing, so it is the safe one to make easy.
    panel(body, Some(Message::DeclineFormNavigation))
}

/// What a save would leave empty, before it is written (§6.4).
///
/// A decision and not a rule: "Save anyway" is a full answer, because the
/// document names these fields required for its own submit button and pulpit
/// only ever writes copies. It is asked here rather than reported afterwards
/// because this is the last moment at which filling the field is still
/// possible.
fn save_review_dialog(review: &crate::app::SaveReview) -> Element<'static, Message> {
    let mut body = column![
        text("Save with required fields empty?").size(type_scale::TITLE),
        text(review.headline()).size(type_scale::BODY),
        text(review.listing()).size(type_scale::BODY),
    ]
    .spacing(gap::M);

    let mut actions = row![button(text("Cancel").size(type_scale::LABEL))
        .padding(gap::S)
        .style(theme::ambient::tool_button)
        .on_press(Message::CancelSaveReview),]
    .spacing(gap::S);
    // Only offered when there is somewhere to go: a field the producer left
    // unnamed cannot be focused, and a button that does nothing is worse than
    // no button.
    if review.first_named().is_some() {
        actions = actions.push(
            button(text("Review").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::ReviewRequiredFields),
        );
    }
    actions = actions.push(
        button(text("Save anyway").size(type_scale::LABEL))
            .padding(gap::S)
            .style(theme::ambient::alert_button)
            .on_press(Message::SaveWithoutFilling),
    );
    body = body.push(actions);

    // The ground behind declines the save: writing no file is the answer that
    // changes nothing.
    panel(body, Some(Message::CancelSaveReview))
}

/// The always-present corner affordance that opens the signature panel.
fn signature_panel_toggle(app: &App) -> Element<'_, Message> {
    let count = app.document_signatures.len();
    let label = if count == 1 {
        "1 signature".to_string()
    } else {
        format!("{count} signatures")
    };
    container(
        button(text(label).size(type_scale::LABEL))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::ToggleSignaturePanel),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::End)
    .align_y(iced::alignment::Vertical::Top)
    .padding(gap::M)
    .into()
}

/// The signature panel (§31.4): every discovered signature's status line,
/// with an expandable detail view and two copy actions each.
fn signature_panel(app: &App) -> Element<'_, Message> {
    let mut body = column![row![
        text("Signatures").size(type_scale::TITLE),
        space::horizontal(),
    ]
    .align_y(Alignment::Center),]
    .spacing(gap::M);

    for (index, entry) in app.document_signatures.iter().enumerate() {
        let line = crate::signing::signature_line_for_verification(entry);
        let mut row_body = column![row![
            text(line.summary_text()).size(type_scale::BODY),
            space::horizontal(),
            button(text("Details").size(type_scale::LABEL))
                .padding(gap::XS)
                .style(theme::ambient::tool_button)
                .on_press(Message::ToggleSignatureDetail(index)),
        ]
        .align_y(Alignment::Center)
        .spacing(gap::S),]
        .spacing(gap::S);

        if app.signature_panel_expanded == Some(index) {
            if let pulpit_render::verify::SignatureVerification::Checked(status) = entry {
                row_body = row_body.push(
                    column![
                        text(format!("Field: {}", status.field_name)).size(type_scale::CAPTION),
                        text(format!("Subject: {}", status.signer_cert.subject))
                            .size(type_scale::CAPTION),
                        text(format!("Issuer: {}", status.signer_cert.issuer))
                            .size(type_scale::CAPTION),
                        text(format!("Serial: {}", status.signer_cert.serial))
                            .size(type_scale::CAPTION),
                        text(format!(
                            "Fingerprint (SHA-256): {}",
                            status.signer_cert.sha256_fingerprint
                        ))
                        .size(type_scale::CAPTION),
                        text("Certificate chain: embedded, not validated (§20.3)")
                            .size(type_scale::CAPTION),
                        text(format!("Coverage: {:?}", status.coverage)).size(type_scale::CAPTION),
                        text(format!(
                            "Signing time (claimed, not attested): {}",
                            status
                                .claimed_time
                                .map(|t| t.to_string())
                                .unwrap_or_else(|| "not stated".to_string())
                        ))
                        .size(type_scale::CAPTION),
                        text(format!(
                            "Digest: {}; signature: {}",
                            status.digest_algorithm, status.signature_algorithm
                        ))
                        .size(type_scale::CAPTION),
                    ]
                    .spacing(gap::XS)
                    .padding(gap::S),
                );
            }
            row_body = row_body.push(
                row![
                    button(text("Copy fingerprint").size(type_scale::LABEL))
                        .padding(gap::XS)
                        .style(theme::ambient::tool_button)
                        .on_press(Message::CopySignatureFingerprint(index)),
                    button(text("Copy report").size(type_scale::LABEL))
                        .padding(gap::XS)
                        .style(theme::ambient::tool_button)
                        .on_press(Message::CopySignatureReport(index)),
                ]
                .spacing(gap::S),
            );
        }
        body = body.push(row_body);
        body = body.push(rule());
    }

    panel(body, Some(Message::ToggleSignaturePanel))
}

/// §31.3, A9: the append-only offer, shown the moment a document that
/// already carries a signature is opened, before anything can mutate it.
fn append_only_offer_dialog() -> Element<'static, Message> {
    let body = column![
        text("Keep this document append-only?").size(type_scale::TITLE),
        text(crate::signing::APPEND_ONLY_OFFER).size(type_scale::BODY),
        row![
            button(text("Edit anyway").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::alert_button)
                .on_press(Message::EditAnyway),
            button(text("Append-only mode").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::selected_button)
                .on_press(Message::AcceptAppendOnly),
        ]
        .spacing(gap::S),
    ]
    .spacing(gap::M);

    // No way out but an answer: mutating a signed document by accident is
    // exactly what this dialog exists to prevent.
    panel(body, None)
}

/// The Sign flow (SPEC-signing.md §31.1), one dialog per step.
fn sign_dialog(flow: &crate::signing::SigningFlow, on_page_zero: bool) -> Element<'_, Message> {
    use crate::signing::SigningFlow;
    use pulpit_render::sign::SigningProfile;

    let cancel = || {
        button(text("Cancel").size(type_scale::LABEL))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::Sign(crate::signing::SignMsg::Cancel))
    };

    match flow {
        SigningFlow::SavingFirst => {
            let body = column![
                text("Sign").size(type_scale::TITLE),
                text("Saving your edits before signing…").size(type_scale::BODY),
            ]
            .spacing(gap::M);
            panel(body, None)
        }
        SigningFlow::ChooseCredential => {
            let body = column![
                text("Sign").size(type_scale::TITLE),
                text("Choose the PKCS#12 (.p12/.pfx) credential to sign with.")
                    .size(type_scale::BODY),
                dialog_footer(
                    button(text("Choose file…").size(type_scale::LABEL))
                        .padding(gap::S)
                        .style(theme::ambient::selected_button)
                        .on_press(Message::Sign(crate::signing::SignMsg::ChooseCredentialFile)),
                ),
            ]
            .spacing(gap::M);
            panel(body, Some(Message::Sign(crate::signing::SignMsg::Cancel)))
        }
        SigningFlow::EnterPassphrase {
            passphrase, error, ..
        } => {
            let mut body = column![
                text("Sign").size(type_scale::TITLE),
                text("Enter the credential's passphrase.").size(type_scale::BODY),
                text_input("Passphrase", passphrase)
                    .secure(true)
                    .on_input(|typed| {
                        Message::Sign(crate::signing::SignMsg::PassphraseChanged(typed))
                    })
                    .on_submit(Message::Sign(crate::signing::SignMsg::PassphraseSubmit))
                    .style(theme::ambient::text_field)
                    .padding(gap::S),
            ]
            .spacing(gap::M);
            if let Some(error) = error {
                body = body.push(
                    text(error.clone())
                        .size(type_scale::CAPTION)
                        .color(theme::ambient::alert()),
                );
            }
            body = body.push(
                row![
                    cancel(),
                    button(text("Continue").size(type_scale::LABEL))
                        .padding(gap::S)
                        .style(theme::ambient::selected_button)
                        .on_press(Message::Sign(crate::signing::SignMsg::PassphraseSubmit)),
                ]
                .spacing(gap::S),
            );
            panel(body, Some(Message::Sign(crate::signing::SignMsg::Cancel)))
        }
        SigningFlow::LoadingCredential { .. } => {
            let body = column![
                text("Sign").size(type_scale::TITLE),
                text("Reading the credential…").size(type_scale::BODY),
            ]
            .spacing(gap::M);
            panel(body, None)
        }
        SigningFlow::CredentialSummary {
            credential, info, ..
        } => {
            let summary = &info.summary;
            let mut body = column![
                text("Sign").size(type_scale::TITLE),
                dialog_section(
                    "Subject",
                    text(summary.subject.clone()).size(type_scale::BODY)
                ),
                dialog_section(
                    "Issuer",
                    text(summary.issuer.clone()).size(type_scale::BODY)
                ),
                dialog_section(
                    "Valid",
                    text(format!("{} — {}", summary.not_before, summary.not_after))
                        .size(type_scale::BODY),
                ),
                dialog_section(
                    "SHA-256 fingerprint",
                    text(summary.sha256_fingerprint.clone()).size(type_scale::BODY),
                ),
                dialog_section(
                    "Key",
                    text(match summary.key_bits {
                        Some(bits) => format!("{} ({bits} bits)", summary.key_algorithm),
                        None => summary.key_algorithm.clone(),
                    })
                    .size(type_scale::BODY),
                ),
            ]
            .spacing(gap::M);
            let _ = credential; // key material itself is never shown
            if info.expired || info.not_yet_valid {
                let warning = if info.expired {
                    "This certificate has expired."
                } else {
                    "This certificate is not yet valid."
                };
                body = body.push(
                    text(warning)
                        .size(type_scale::BODY)
                        .color(theme::ambient::alert()),
                );
                if !info.override_validity {
                    body = body.push(
                        button(text("Sign anyway").size(type_scale::LABEL))
                            .padding(gap::S)
                            .style(theme::ambient::alert_button)
                            .on_press(Message::Sign(crate::signing::SignMsg::OverrideValidity)),
                    );
                }
            }
            let mut actions = row![cancel()].spacing(gap::S);
            if info.may_proceed() {
                actions = actions.push(
                    button(text("Continue").size(type_scale::LABEL))
                        .padding(gap::S)
                        .style(theme::ambient::selected_button)
                        .on_press(Message::Sign(crate::signing::SignMsg::ContinueToOptions)),
                );
            }
            body = body.push(actions);
            panel(body, Some(Message::Sign(crate::signing::SignMsg::Cancel)))
        }
        SigningFlow::Options {
            options,
            candidates,
            ..
        } => {
            use crate::signing::{PlacementPosition, PlacementSize, SignMsg, TargetChoice};

            let mut body = column![
                text("Sign").size(type_scale::TITLE),
                dialog_section(
                    "Reason (optional)",
                    text_input("Reason", &options.reason)
                        .on_input(|v| Message::Sign(SignMsg::ReasonChanged(v)))
                        .style(theme::ambient::text_field)
                        .padding(gap::S),
                ),
                dialog_section(
                    "Location (optional)",
                    text_input("Location", &options.location)
                        .on_input(|v| Message::Sign(SignMsg::LocationChanged(v)))
                        .style(theme::ambient::text_field)
                        .padding(gap::S),
                ),
                dialog_section(
                    "Contact (optional)",
                    text_input("Contact", &options.contact)
                        .on_input(|v| Message::Sign(SignMsg::ContactChanged(v)))
                        .style(theme::ambient::text_field)
                        .padding(gap::S),
                ),
            ]
            .spacing(gap::M);

            // §31.1 step 5's target picker: shown only when preflight found
            // more than one candidate empty field to countersign into. The
            // single-candidate case (a fresh field, or the one unambiguous
            // empty field) is already selected and needs no picker.
            if candidates.len() > 1 {
                let mut picker = column![].spacing(gap::S);
                for candidate in candidates {
                    let label = match candidate {
                        TargetChoice::NewField => "New field".to_string(),
                        TargetChoice::ExistingField(name) => name.clone(),
                    };
                    let selected = options.target.as_ref() == Some(candidate);
                    picker = picker.push(
                        button(text(label).size(type_scale::LABEL))
                            .padding(gap::S)
                            .style(if selected {
                                theme::ambient::selected_button
                            } else {
                                theme::ambient::tool_button
                            })
                            .on_press(Message::Sign(SignMsg::TargetChosen(candidate.clone()))),
                    );
                }
                body = body.push(dialog_section(
                    "This document has several empty signature fields — choose one",
                    picker,
                ));
            }

            // §25.5: Visible places a text appearance via position/size
            // presets on page 0 (the only page_index pulpit-render accepts);
            // see `crate::signing`'s module doc comment for why placement is
            // a preset picker rather than a drawn box in v1. Off page 0 the
            // choice is disabled with the reason shown, rather than letting
            // the user build a placement Sign would then refuse.
            let mut appearance_section = column![row![
                button(text("Invisible").size(type_scale::LABEL))
                    .padding(gap::S)
                    .style(if options.visible_requested {
                        theme::ambient::tool_button
                    } else {
                        theme::ambient::selected_button
                    })
                    .on_press(Message::Sign(SignMsg::VisibleChanged(false))),
                if on_page_zero {
                    button(text("Visible").size(type_scale::LABEL))
                        .padding(gap::S)
                        .style(if options.visible_requested {
                            theme::ambient::selected_button
                        } else {
                            theme::ambient::tool_button
                        })
                        .on_press(Message::Sign(SignMsg::VisibleChanged(true)))
                } else {
                    button(text("Visible").size(type_scale::LABEL)).padding(gap::S)
                },
            ]
            .spacing(gap::S)]
            .spacing(gap::S);
            if !on_page_zero {
                appearance_section = appearance_section.push(
                    text(
                        "Visible signatures can only be placed on the first page in this \
                         build; go to page 1 to place one.",
                    )
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::muted()),
                );
            }
            if let Some(placement) = options.placement {
                let mut positions = row![].spacing(gap::S);
                for position in PlacementPosition::ALL {
                    let selected = placement.position == position;
                    positions = positions.push(
                        button(text(position.label()).size(type_scale::CAPTION))
                            .padding(gap::XS)
                            .style(if selected {
                                theme::ambient::selected_button
                            } else {
                                theme::ambient::tool_button
                            })
                            .on_press(Message::Sign(SignMsg::PlacementPositionChosen(position))),
                    );
                }
                let mut sizes = row![].spacing(gap::S);
                for size in PlacementSize::ALL {
                    let selected = placement.size == size;
                    sizes = sizes.push(
                        button(text(size.label()).size(type_scale::CAPTION))
                            .padding(gap::XS)
                            .style(if selected {
                                theme::ambient::selected_button
                            } else {
                                theme::ambient::tool_button
                            })
                            .on_press(Message::Sign(SignMsg::PlacementSizeChosen(size))),
                    );
                }
                appearance_section = appearance_section
                    .push(dialog_section("Position", positions))
                    .push(dialog_section("Size", sizes));
            }
            body = body.push(dialog_section("Appearance", appearance_section));

            body = body.push(
                row![
                    cancel(),
                    button(text("Continue").size(type_scale::LABEL))
                        .padding(gap::S)
                        .style(theme::ambient::selected_button)
                        .on_press(Message::Sign(SignMsg::ContinueToConfirm)),
                ]
                .spacing(gap::S),
            );
            panel(body, Some(Message::Sign(crate::signing::SignMsg::Cancel)))
        }
        SigningFlow::Confirm { info, options, .. } => {
            let mut body = column![
                text("Confirm signature").size(type_scale::TITLE),
                text(format!(
                    "Signing as {} (fingerprint {}), profile {}.",
                    info.summary.subject,
                    info.summary.sha256_fingerprint,
                    match SigningProfile::AdbePkcs7Detached {
                        SigningProfile::AdbePkcs7Detached => "B-B",
                        SigningProfile::EtsiCadesDetached => "B-B (PAdES)",
                    }
                ))
                .size(type_scale::BODY),
                text(crate::signing::IDENTITY_DISCLOSURE)
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::muted()),
            ]
            .spacing(gap::M);
            if options
                .target
                .as_ref()
                .map(crate::signing::TargetChoice::is_countersign)
                .unwrap_or(false)
            {
                body = body.push(
                    text(crate::signing::COUNTERSIGN_DISCLOSURE)
                        .size(type_scale::CAPTION)
                        .color(theme::ambient::muted()),
                );
            }
            body = body.push(
                row![
                    button(text("Back").size(type_scale::LABEL))
                        .padding(gap::S)
                        .style(theme::ambient::tool_button)
                        .on_press(Message::Sign(crate::signing::SignMsg::BackToOptions)),
                    cancel(),
                    button(text("Sign").size(type_scale::LABEL))
                        .padding(gap::S)
                        .style(theme::ambient::selected_button)
                        .on_press(Message::Sign(crate::signing::SignMsg::Confirm)),
                ]
                .spacing(gap::S),
            );
            // Non-dismissable except its own two buttons (§31.1 step 7):
            // no ground-behind press and no close glyph.
            panel(body, None)
        }
        SigningFlow::Signing { destination, .. } => {
            let body = column![
                text("Sign").size(type_scale::TITLE),
                text(format!("Signing to {}…", destination.display())).size(type_scale::BODY),
            ]
            .spacing(gap::M);
            panel(body, None)
        }
        SigningFlow::Result {
            report,
            destination,
            verification,
        } => {
            let mut body = column![
                text("Signed").size(type_scale::TITLE),
                text(format!(
                    "Wrote {} ({} signature{} now present).",
                    destination.display(),
                    report.signature_count,
                    if report.signature_count == 1 { "" } else { "s" }
                ))
                .size(type_scale::BODY),
            ]
            .spacing(gap::M);
            for entry in verification {
                let line = crate::signing::signature_line_for_verification(entry);
                body = body.push(text(line.summary_text()).size(type_scale::BODY));
            }
            body = body.push(
                button(text("Done").size(type_scale::LABEL))
                    .padding(gap::S)
                    .style(theme::ambient::selected_button)
                    .on_press(Message::Sign(crate::signing::SignMsg::Done)),
            );
            panel(body, Some(Message::Sign(crate::signing::SignMsg::Done)))
        }
        SigningFlow::Failed { detail } => {
            let body = column![
                text("Signing failed").size(type_scale::TITLE),
                text(detail.clone())
                    .size(type_scale::BODY)
                    .color(theme::ambient::alert()),
                text("The source document is unchanged.")
                    .size(type_scale::CAPTION)
                    .color(theme::ambient::muted()),
                button(text("Close").size(type_scale::LABEL))
                    .padding(gap::S)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::Sign(crate::signing::SignMsg::Done)),
            ]
            .spacing(gap::M);
            panel(body, Some(Message::Sign(crate::signing::SignMsg::Done)))
        }
    }
}

/// The offer to recover an interrupted talk.
///
/// Deliberately a question with two answers and no default: restoring puts a
/// slide in front of an audience and may move windows between displays, so it
/// happens on a press and never on a timeout or a stray key.
fn restore_session_dialog(plan: &crate::session::RestorePlan) -> Element<'static, Message> {
    let body = column![
        text("Restore the interrupted session?").size(type_scale::TITLE),
        text(match plan.saved_ago_now() {
            Some(ago) => format!("Pulpit did not shut down cleanly — {ago}."),
            None => "Pulpit did not shut down cleanly last time.".to_string(),
        })
        .size(type_scale::BODY),
        text(plan.summary()).size(type_scale::BODY),
        text("Nothing is shown to the audience until you choose.").size(type_scale::LABEL),
        row![
            button(text("Start fresh").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::DiscardSession),
            button(text("Restore").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::alert_button)
                .on_press(Message::RestoreSession),
        ]
        .spacing(gap::S),
    ]
    .spacing(gap::M);

    // No way out but an answer: restoring puts a slide in front of an
    // audience, and a stray press on the ground behind must not decide that
    // either way.
    panel(body, None)
}

/// Offer back the edits a previous run did not save (§11.4).
///
/// The wording is deliberately careful about what pulpit does not know: the
/// journal records what was edited, not whether the user saved a copy
/// elsewhere afterwards, so it says what it has rather than promising that
/// applying it is safe.
fn restore_edits_dialog(
    journal: &crate::reader_journal::RecoveredJournal,
) -> Element<'static, Message> {
    let body = column![
        text("Put back the unsaved edits?").size(type_scale::TITLE),
        text(
            "Pulpit did not shut down cleanly, and this document had edits that had \
              not been saved to a copy."
        )
        .size(type_scale::BODY),
        text(journal.summary()).size(type_scale::BODY),
        text(
            "They are applied to the document as it is now. If you already saved a \
              copy with them, start fresh."
        )
        .size(type_scale::LABEL),
        row![
            button(text("Start fresh").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::DiscardReaderEdits),
            button(text("Put them back").size(type_scale::LABEL))
                .padding(gap::S)
                .style(theme::ambient::alert_button)
                .on_press(Message::RestoreReaderEdits),
        ]
        .spacing(gap::S),
    ]
    .spacing(gap::M);

    panel(body, None)
}

fn section<'a>(title: &'a str, content: Element<'a, Message>) -> Element<'a, Message> {
    // No box: the page is a document. A panel inside a panel inside a page is
    // three borders saying nothing.
    column![
        text(title)
            .size(type_scale::HEADING)
            .color(theme::ambient::text()),
        content,
    ]
    .spacing(gap::S)
    .width(Length::Fill)
    .into()
}

fn mappings(app: &App) -> Element<'_, Message> {
    // The two split entries are gone: a doubled page announces itself and is
    // detected on open, so the only split decision left to a presenter is
    // which half is which, and that is the swap below.
    let options = [
        ("Slides only", NotesMapping::SlidesOnly),
        (
            "Alternating: slide, notes",
            NotesMapping::PairedPages(PairedRule::Alternating { notes_first: false }),
        ),
        (
            "Two ranges: slides then notes",
            NotesMapping::PairedPages(PairedRule::TwoRanges { notes_first: false }),
        ),
    ];
    let mut buttons = row![text("Notes mapping:").size(type_scale::LABEL)].spacing(gap::XS);
    for (label, mapping) in options {
        let selected = app.state.mapping() == &mapping;
        buttons = buttons.push(
            selectable(button(text(label).size(type_scale::CAPTION)), selected)
                .on_press(Message::SetMapping(mapping)),
        );
    }
    let current = app.state.mapping();
    if let NotesMapping::SplitPage { slide, .. } = current {
        let notes_side = if slide.x > 0.0 { "left" } else { "right" };
        buttons =
            buttons.push(text(format!("split page, notes {notes_side}")).size(type_scale::CAPTION));
        buttons = buttons.push(
            button(text("Swap halves").size(type_scale::CAPTION))
                .on_press(Message::SetMapping(current.swapped())),
        );
    }
    buttons.wrap().into()
}

/// The raw-scancode fallback, surfaced as a prompt.
fn unbound_key(app: &App) -> Option<Element<'_, Message>> {
    let (name, code) = app.unbound_key.as_ref()?;
    let described = match name {
        Some(name) if name != "unidentified" => format!("“{name}” (scancode {code})"),
        _ => format!("an unidentified key (scancode {code})"),
    };
    let bindable = [
        Action::Next,
        Action::Previous,
        Action::First,
        Action::Last,
        Action::Blank,
        Action::ToggleTimer,
        Action::CommitPreview,
    ];
    let mut buttons =
        row![text(format!("{described} is not bound. Use it for:")).size(type_scale::LABEL)]
            .spacing(gap::S)
            .align_y(Alignment::Center);
    for action in bindable {
        buttons = buttons.push(
            button(text(action.label()).size(type_scale::CAPTION))
                .padding(gap::XS)
                .style(theme::ambient::tool_button)
                .on_press(Message::BindUnboundKey(action)),
        );
    }
    buttons = buttons.push(
        button(text("ignore").size(type_scale::CAPTION))
            .padding(gap::XS)
            .style(theme::ambient::tool_button)
            .on_press(Message::ForgetUnboundKey),
    );
    // On its own panel: a bare row of buttons floating over the layout reads
    // as part of the layout, and the transparent gaps between them show the
    // slide through.
    Some(
        container(
            container(buttons.wrap())
                .padding(gap::S)
                .style(theme::ambient::dialog),
        )
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .padding(gap::S)
        .into(),
    )
}

// ----------------------------------------------------------- layout pages

fn library_page(app: &App) -> Element<'_, Message> {
    let page = designer_view::library(&app.layouts, Some(&app.active_layout.id));
    match &app.layout_dialog {
        None => page,
        Some(dialog) => stack![page, layout_dialog(dialog)].into(),
    }
}

fn layout_dialog(dialog: &LayoutDialog) -> Element<'_, Message> {
    let body: Element<'_, Message> = match dialog {
        LayoutDialog::ConfirmDelete { name, .. } => column![
            text(format!("Delete “{name}”?")).size(type_scale::HEADING),
            text("This cannot be undone.")
                .size(type_scale::LABEL)
                .color(theme::ambient::muted()),
            row![
                button(text("Delete").size(type_scale::BODY))
                    .padding(gap::S)
                    .style(theme::ambient::alert_button)
                    .on_press(Message::ConfirmLayoutDialog),
                button(text("Cancel").size(type_scale::BODY))
                    .padding(gap::S)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::CancelLayoutDialog),
            ]
            .spacing(gap::S),
        ]
        .spacing(gap::M)
        .into(),
    };

    // Backing out is what the ground and the glyph mean here: the dialog
    // already offers Cancel, and this is the same answer by another route.
    panel(body, Some(Message::CancelLayoutDialog))
}

fn editor_page(app: &App) -> Element<'_, Message> {
    let Some(designer) = &app.designer else {
        return library_page(app);
    };

    // The canvas draws the real widgets with sample content, which is the
    // whole point of designing on it: what you see is what you present.
    let frame = |_slide: usize,
                 _kind: FrameKind,
                 _width: u32|
     -> Option<iced::widget::image::Handle> { None };
    let context = app.render_context(
        crate::widgets::Mode::Editing,
        &frame,
        crate::widgets::sample::NOTES,
    );
    designer_view::editor(designer, &context)
}

/// Seconds since local midnight, for the clock widget.
pub fn seconds_of_day() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    ((now as i64 + local_offset_seconds()).rem_euclid(86_400)) as u32
}

/// Offset from UTC in seconds, read once from `date +%z`. Falls back to UTC,
/// which is honest rather than wrong by an unknown amount.
fn local_offset_seconds() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|output| {
                let text = String::from_utf8(output.stdout).ok()?;
                let text = text.trim();
                let sign = if text.starts_with('-') { -1 } else { 1 };
                let hours: i64 = text.get(1..3)?.parse().ok()?;
                let minutes: i64 = text.get(3..5)?.parse().ok()?;
                Some(sign * (hours * 3600 + minutes * 60))
            })
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_is_within_a_day() {
        assert!(seconds_of_day() < 86_400);
    }
}

#[cfg(test)]
mod grid_tests {
    use super::{grid_plan, MAX_CELL_WIDTH, MIN_CELL_WIDTH};
    use iced::Size;

    const WIDE: Size = Size {
        width: 1600.0,
        height: 900.0,
    };

    #[test]
    fn a_short_deck_fills_the_space_it_is_given() {
        let plan = grid_plan(6, WIDE, 16.0 / 9.0);
        assert!(
            plan.cell_width > MIN_CELL_WIDTH * 1.5,
            "six slides in a large window should be drawn large, got {}",
            plan.cell_width
        );
        assert!(plan.cell_width <= MAX_CELL_WIDTH);
        let total = plan.rows as f32 * plan.cell_height;
        assert!(total <= WIDE.height, "and still fit without scrolling");
    }

    #[test]
    fn a_single_slide_is_not_drawn_the_size_of_the_window() {
        // Upscaling a small render to fill a 1600pt window looks like a
        // fault. It stops at the size it was rendered for.
        let plan = grid_plan(1, WIDE, 16.0 / 9.0);
        assert_eq!(plan.cell_width, MAX_CELL_WIDTH);
    }

    #[test]
    fn a_long_deck_stops_shrinking_at_the_readable_size() {
        let plan = grid_plan(500, WIDE, 16.0 / 9.0);
        assert!(
            plan.cell_width >= MIN_CELL_WIDTH,
            "readability is the floor, not a target to shrink past: got {}",
            plan.cell_width
        );
        let total = plan.rows as f32 * plan.cell_height;
        assert!(
            total > WIDE.height,
            "which means it scrolls, and that is the intended trade"
        );
    }

    #[test]
    fn cells_never_overflow_the_width_they_were_given() {
        for count in [1, 2, 3, 7, 12, 40, 200, 500] {
            for width in [640.0, 900.0, 1600.0, 2560.0] {
                let size = Size {
                    width,
                    height: 900.0,
                };
                let plan = grid_plan(count, size, 16.0 / 9.0);
                let used = plan.columns as f32 * plan.cell_width
                    + super::gap::S * (plan.columns.saturating_sub(1)) as f32;
                assert!(
                    used <= width + 0.5,
                    "{count} slides at {width}pt: {} columns of {} overflow",
                    plan.columns,
                    plan.cell_width
                );
            }
        }
    }

    #[test]
    fn every_page_has_a_place_in_the_grid() {
        for count in [1, 5, 13, 100] {
            let plan = grid_plan(count, WIDE, 16.0 / 9.0);
            assert!(
                plan.rows * plan.columns >= count,
                "{count} slides do not all fit in {}x{}",
                plan.rows,
                plan.columns
            );
        }
    }

    #[test]
    fn the_cells_match_the_shape_of_the_pages() {
        // A 4:3 deck gets 4:3 cells, so nothing is letterboxed inside them.
        let plan = grid_plan(20, WIDE, 4.0 / 3.0);
        let ratio = plan.cell_width / plan.picture_height;
        assert!(
            (ratio - 4.0 / 3.0).abs() < 0.01,
            "expected 4:3 cells, got {ratio}"
        );
    }

    #[test]
    fn a_nonsense_aspect_does_not_produce_a_nonsense_grid() {
        for aspect in [0.0, -3.0, f32::NAN, f32::INFINITY] {
            let plan = grid_plan(10, WIDE, aspect);
            assert!(plan.picture_height.is_finite() && plan.picture_height > 0.0);
            assert!(plan.cell_width >= MIN_CELL_WIDTH);
        }
    }
}
