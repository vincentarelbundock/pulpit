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
    button, canvas, checkbox, column, container, image, mouse_area, opaque, pick_list, radio,
    responsive, row, scrollable, space, stack, text, text_input, tooltip, Column, Row,
};
use iced::{window, Alignment, Color, ContentFit, Element, Length};

use crate::settings::Action;
use pulpit_core::{Blank, NotesMapping, PairedRule, Region};
use pulpit_display::{Role, RoleTarget};
use pulpit_render::cache::FrameKind;

use crate::platform::Appearance;

use crate::app::{App, LayoutDialog, Message, DOCUMENTATION_URL};
use crate::designer::Page;
use crate::designer_view;
use crate::panel::panel;
use crate::theme;
use crate::theme::{space as gap, target, type_scale};
use crate::toast::Intent;

pub fn view(app: &App, window: window::Id) -> Element<'_, Message> {
    let started = std::time::Instant::now();
    // One palette for the whole pass. The audience window is deliberately
    // exempt from theming: its colours are output, not chrome.
    theme::ambient::set(app.theme.palette);

    if Some(window) == app.audience_window {
        let view = audience(app);
        app.view_meter.record(started.elapsed());
        return view;
    }

    // Named the widget tree's shape before building it, so `on_read_command`
    // can tell a surface's first report apart from an ordinary scroll. See
    // `document_surface_fingerprint`.
    app.reader_surface_shape.set(
        app.reader_surface_shape
            .get()
            .observe(document_surface_fingerprint(app)),
    );

    // Startup and the `?` reference are one surface, not two versions of the
    // same information. When there is work behind it, the page adds only a
    // dismissal control; the reference itself remains identical.
    let mut page = if app.shortcuts_open {
        shortcut_reference_page(app, app.state.document().is_some())
    } else {
        match app.page {
            Page::Presenter => presenter(app),
            Page::Library => library_page(app),
            Page::Editor => editor_page(app),
            Page::Settings => settings_page(app),
        }
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
    // Every layer below is always present in the stack, blank when its own
    // condition is false — never mounted-and-unmounted with it. See `blank`
    // and the scrub layer's note in `presenter` for why: a layer that came
    // and went would change the shape of this stack the moment it toggled,
    // and Iced discards all descendant widget state on a shape change.
    page = stack![
        page,
        // The empty-start surface passes through `presenter`, which owns
        // the menu overlay there. An explicitly opened shortcut reference
        // bypasses it, so mount the same menu at this level when its
        // hamburger is used.
        layer(app.shortcuts_open && app.menu_open, || menu(app)),
        match alert {
            Some(alpha) => alarm_flash(alpha),
            None => blank(),
        },
        // Toasts float above whatever page is showing, and never on the
        // audience window.
        match toasts(app) {
            Some(overlay) => overlay,
            None => blank(),
        },
        layer(app.confirm_reset_colors, reset_colors_dialog),
        match app.signature_profile_editor.as_ref() {
            Some(editor) => signature_profile_editor(app, editor),
            None => blank(),
        },
        match app.signature_profile_removal.as_ref() {
            Some(removal) => signature_profile_removal(app, removal),
            None => blank(),
        },
        // The document turned out to be in a language with no installed
        // voice. Asked rather than papered over: reading Polish aloud in an
        // American accent is a worse answer than a question.
        match app.speech.prompt.as_ref() {
            Some(prompt) => missing_voice_dialog(prompt),
            None => blank(),
        },
        // What the open document asked to do to the reader's place in it.
        // Above the page, because it is a question about the page.
        match app.pending_form_goto.as_ref() {
            Some(request) => form_navigation_dialog(request),
            None => blank(),
        },
        // What a save would leave empty, asked before the file is written.
        // A small always-present affordance for the signature panel
        // (§31.4), whenever the open document has at least one signature to
        // report on. Not folded into the reader toolbar's generic `Message`
        // widget (see `widgets::document::view::tools`'s note) because the
        // panel is `App` state, not `ReadCommand` state.
        layer(
            !app.document_signatures.is_empty() && !app.signature_panel_open,
            || signature_panel_toggle(app),
        ),
        layer(app.signature_panel_open, || signature_panel(app)),
        // §31.3, A9: offered before anything can mutate a document that
        // already carries a signature. Above the page, non-dismissable
        // except its own two buttons — declining silently would leave the
        // reader guessing which mode they are in.
        layer(app.pending_append_only_offer, append_only_offer_dialog),
        // The Sign flow (SPEC-signing.md §31.1), one dialog for whichever
        // step it is on.
        match app.signing.as_ref() {
            Some(flow) => sign_dialog(app, flow),
            None => blank(),
        },
        match app.pending_save_review.as_ref() {
            Some(review) => save_review_dialog(review),
            None => blank(),
        },
        layer(app.print_dialog.is_some(), || print_dialog(app)),
        // The alarm popup is a top-level overlay rather than something
        // drawn inside the clock's pane: a clock can be a narrow cell in a
        // strip, and a popup anchored there would be clipped by its own
        // widget.
        layer(app.alarm_controls.open, || alarms_dialog(app)),
        layer(app.about_open, about_overlay),
        // What the open document is. A dialog, not a rail view: one question
        // about the whole file, asked once and closed.
        layer(app.properties_open, || document_properties_dialog(app)),
        // The timer menu is the same kind of overlay, for the same reason.
        layer(app.timer_controls.open, || timer_dialog(app)),
        // The same rule for a document: what a previous run left unsaved is
        // offered, never applied, and the offer has no way out but an
        // answer.
        match app.reader_recovery.as_ref() {
            Some(journal) => restore_edits_dialog(journal),
            None => blank(),
        },
    ]
    .into();
    // Its own renderer, its own atlas, its own residency — exactly as the
    // projector has, and for the same reason. A slide panel's picture is well
    // over the two mebibytes at which Iced stops uploading inline, so without
    // this a panel draws nothing on the pass a new frame first reaches it.
    let view =
        crate::residency::resident(page, app.presenter_resident_handles(), app.upload_meter());
    app.view_meter.record(started.elapsed());
    view
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
    let mut context = app.render_context(
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
            // Fullscreen removes the layout cell that normally paints the
            // page's mount. Use the same absolute-black surround as a slide:
            // any space left around the sheet should disappear into the
            // screen rather than inheriting the reader theme's light canvas.
            return container(crate::layout_renderer::widget(
                page,
                &context,
                app.compose_buffer(),
                interaction,
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::ambient::slide_letterbox)
            .into();
        }
    }
    let has_outline_panel = app
        .active_layout
        .widgets()
        .iter()
        .any(|widget| widget.kind() == crate::widgets::WidgetKind::DocumentOutline);
    let compact_sidebar = has_outline_panel && app.compact_document_sidebar();
    let sidebar_reveal = context.reader.outline_reveal.max(context.search_reveal);
    if compact_sidebar {
        // Remove the layout-owned rail. Its contents are mounted again below
        // as a drawer, so there remains exactly one outline/search surface.
        context.reader.outline_reveal = 0.0;
        context.search_reveal = 0.0;
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
    let body = if compact_sidebar {
        let sidebar = if app.search_pane.is_open() {
            search_workspace(app)
        } else {
            let outline = app
                .active_layout
                .widgets()
                .into_iter()
                .find(|widget| widget.kind() == crate::widgets::WidgetKind::DocumentOutline)
                .expect("a compact document sidebar has an outline widget");
            crate::layout_renderer::widget(outline, &context, app.compose_buffer(), interaction)
        };
        crate::layout_renderer::overlay_side_panel(sidebar, framed, 300.0, sidebar_reveal)
    } else if has_outline_panel {
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

    // Empty startup is its own mode-neutral surface. It must not mount a
    // Reader or Presenter layout: opening a document starts in the Reader
    // unless that exact file carries an explicit remembered choice.
    if app.state.document().is_none() {
        page = shortcut_reference_page(app, false);
    }

    page = stack![
        page,
        layer(app.menu_open, || menu(app)),
        layer(app.audience_start_menu_open, || audience_start_menu(app)),
        // The scrub layer is *always* stacked, empty when idle. Stacking it
        // only while scrubbing changed the shape of the widget tree the
        // moment the first thumbnail arrived, which threw away the
        // slider's drag state mid-drag: the handle froze as soon as the
        // preview appeared.
        scrub_layer(app),
        layer(app.overview, || overview(app)),
    ]
    .into();
    page
}

/// Search is a transient rail beside the working surface.
fn search_workspace(app: &App) -> Element<'_, Message> {
    let search = crate::widgets::context::SearchData {
        state: &app.search,
        input_focus: app.search_input_focused(),
        results_focus: app.search_results_focused(),
        scroll: app.search_scroll,
        viewport: app.search_viewport.clone(),
    };
    // A transient rail beside a presenter layout draws no tab row, so whether
    // the document can carry marks does not reach it.
    crate::widgets::search::view::pane(search, true, false, false, false, interaction)
}

/// A blank, transparent, click-through filler for one slot in a fixed
/// stack.
///
/// Iced diffs the widget tree positionally. An overlay that is stacked only
/// while its condition holds changes the *shape* of the tree the instant
/// the condition flips, and Iced discards all descendant widget state on a
/// shape change — scroll offsets, drag state, focus, all of it. The scrub
/// layer below learned this the hard way: it stays stacked always, blank
/// when idle, rather than appearing only while scrubbing. Every conditional
/// overlay in `view` and `presenter` follows the same rule, and this is
/// what they render in the slot when their condition is false.
fn blank<'a>() -> Element<'a, Message> {
    container(space::horizontal())
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Mount `build()` when `open`, else the blank filler, so the enclosing
/// stack's shape never depends on `open`. See `blank`.
fn layer<'a>(open: bool, build: impl FnOnce() -> Element<'a, Message>) -> Element<'a, Message> {
    if open {
        build()
    } else {
        blank()
    }
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
                theme::typography::body(label.clone()).color(theme::ambient::text()),
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

        crate::widgets::scroll::thumbed(
            crate::widgets::scroll::vertical(content)
                .id(overview_scrollable())
                .height(Length::Fill)
                .on_scroll(|viewport| Message::OverviewScrolled(viewport.absolute_offset().y)),
            app.overview_scroll,
            Message::OverviewThumbDragged,
        )
    });

    // How far along the warming is, but only while there is something to
    // say: a bare count of pages is noise once they have all arrived.
    let progress: Element<'_, Message> = if ready < count {
        theme::typography::caption(format!("{ready} of {count} pages ready")).into()
    } else {
        theme::typography::caption(format!("{count} pages")).into()
    };

    let body = column![
        row![
            theme::typography::title("Page overview"),
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
        None => container(theme::typography::note(format!("{}", slide + 1)))
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

/// Every branch that decides the shape of the widget tree between the window
/// root and the document scrollable, folded into one number.
///
/// Iced diffs the tree positionally: whichever of these flips — a page swap,
/// the shortcuts reference page, a layout change, fullscreen, the
/// compact-sidebar threshold, the toolbar strip appearing or not, a document
/// opening or closing — tears down everything below the change, the document
/// scrollable's own offset included, and mounts a fresh one that is born at
/// zero and dutifully reports that zero. [`App::reader_surface_shape`] turns
/// a change here into a generation the report-side gate in
/// `App::on_read_command` compares its own record against, so that gate is
/// only ever as reliable a witness to "this is a genuine remount" as this
/// function is complete. Any new conditional wrapper anywhere along that
/// path has to fold its condition in here too, or the remount it causes will
/// report a false scroll to zero and the reader will lose its place.
fn document_surface_fingerprint(app: &App) -> u64 {
    use std::hash::{Hash, Hasher};

    let page_kind: u8 = match app.page {
        Page::Presenter => 0,
        Page::Library => 1,
        Page::Editor => 2,
        Page::Settings => 3,
    };
    let has_outline_panel = app
        .active_layout
        .widgets()
        .iter()
        .any(|widget| widget.kind() == crate::widgets::WidgetKind::DocumentOutline);
    // Mirrors `presenter_toolbar`'s own "is there anything to show" check,
    // without building the strip: the fingerprint only needs to know whether
    // its presence would change, not what is in it.
    let presenting =
        crate::layout::PrimaryViewer::of(&app.active_layout) == crate::layout::PrimaryViewer::Slide;
    let toolbar_present = !placed(app, crate::widgets::WidgetKind::MainMenu)
        || (presenting && !placed(app, crate::widgets::WidgetKind::AudienceControls));

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    app.shortcuts_open.hash(&mut hasher);
    page_kind.hash(&mut hasher);
    app.state.document().is_none().hash(&mut hasher);
    app.active_layout.id.hash(&mut hasher);
    app.reader_fullscreen.hash(&mut hasher);
    has_outline_panel.hash(&mut hasher);
    app.compact_document_sidebar().hash(&mut hasher);
    app.search_pane.is_open().hash(&mut hasher);
    toolbar_present.hash(&mut hasher);
    // The document widget's own gate (`widgets::document::view::page_surface`):
    // closed or open, and a page count of zero draws the same "nothing to
    // read" stand-in an unopened document does.
    app.reader.is_open().hash(&mut hasher);
    (app.reader.page_count() == 0).hash(&mut hasher);
    hasher.finish()
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

/// The same Start/Stop pair the placeable widget draws, padded into the strip
/// rather than centred in a layout cell.
fn audience_lifecycle_controls(app: &App) -> Element<'_, Message> {
    // The strip only exists on the live presenter screen, so its controls are
    // always live; the ambient palette was set to `app.theme.palette` at the
    // top of this view pass.
    container(
        crate::widgets::chrome::view::lifecycle_row(app.audience_started, true, interaction)
            .spacing(gap::XS),
    )
    .padding(iced::Padding::from([gap::S, gap::S]))
    .into()
}

/// Display choices and alternate Start actions. Choosing a display both saves
/// it as the new default and starts the audience immediately.
fn audience_start_menu(app: &App) -> Element<'_, Message> {
    const PANEL_WIDTH: f32 = 320.0;
    let palette = app.theme.palette;
    let option = |label: String, selected: bool, message| {
        button(theme::typography::label(label))
            .width(Length::Fill)
            .height(Length::Fixed(theme::controls::MENU_ITEM_HEIGHT))
            .padding(iced::Padding::from([0.0, gap::L]))
            .style(theme::controls::selectable(palette, selected))
            .on_press(message)
    };
    let mut items = Column::new()
        .spacing(gap::XS)
        .width(Length::Fixed(PANEL_WIDTH))
        .push(theme::typography::label("Start audience on").color(theme::ambient::muted()));

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
const RECENT_MENU_LIMIT: usize = 5;

fn recent_menu_documents(
    recent: &std::collections::VecDeque<std::path::PathBuf>,
) -> impl Iterator<Item = &std::path::Path> {
    recent
        .iter()
        .take(RECENT_MENU_LIMIT)
        .map(std::path::PathBuf::as_path)
}

fn recent_menu_label(path: &std::path::Path) -> std::borrow::Cow<'_, str> {
    path.file_name()
        .map(std::ffi::OsStr::to_string_lossy)
        .unwrap_or_else(|| path.as_os_str().to_string_lossy())
}

/// The main menu: the handful of commands that are not on the layout.
fn menu(app: &App) -> Element<'_, Message> {
    let entry = |label: &'static str, shortcut: Option<String>, message: Message| {
        let mut row = Row::new()
            .spacing(gap::M)
            .align_y(Alignment::Center)
            .push(theme::typography::body(label));
        if let Some(shortcut) = shortcut {
            row = row
                .push(space::horizontal())
                .push(theme::typography::caption(shortcut));
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
        container(theme::typography::caption(label)).padding(iced::Padding {
            top: gap::S,
            left: gap::S,
            ..iced::Padding::from(0.0)
        })
    };
    let recent_entry = |path: &std::path::Path| {
        let label = recent_menu_label(path).into_owned();
        let control = button(
            container(
                theme::typography::body(label.clone()).wrapping(iced::widget::text::Wrapping::None),
            )
            .width(Length::Fill)
            .center_y(Length::Fill)
            .clip(true)
            .padding(iced::Padding::from([0.0, gap::S])),
        )
        .width(Length::Fill)
        .height(Length::Fixed(MENU_ROW))
        .style(theme::ambient::tool_button)
        .on_press(Message::MenuAction(Box::new(Message::Opened(Some(
            path.to_path_buf(),
        )))));
        tooltip(
            control,
            container(text(label).size(type_scale::CAPTION))
                .padding(gap::S)
                .style(theme::ambient::dialog),
            tooltip::Position::Right,
        )
    };

    let mut items = Column::new()
        .spacing(gap::XS)
        .width(Length::Fixed(MENU_WIDTH));
    items = items.push(
        container(theme::typography::label("pulpit").color(theme::ambient::muted()))
            .height(Length::Fixed(MENU_HEADER)),
    );
    if app.recent_menu_open {
        let recent_back = button(
            container(
                row![
                    theme::icon::icon(theme::Icon::ArrowLeft, type_scale::LABEL),
                    theme::typography::body("Open recent"),
                ]
                .spacing(gap::S)
                .align_y(Alignment::Center),
            )
            .center_y(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fixed(MENU_ROW))
        .padding(iced::Padding::from([0.0, gap::S]))
        .style(theme::ambient::tool_button)
        .on_press(Message::ToggleRecentMenu);
        items = items.push(recent_back);
        if app.settings.recent.is_empty() {
            items = items.push(
                container(theme::typography::note("No recent files"))
                    .height(Length::Fixed(MENU_ROW))
                    .padding(iced::Padding::from([gap::S, gap::S])),
            );
        } else {
            for path in recent_menu_documents(&app.settings.recent) {
                items = items.push(recent_entry(path));
            }
        }
    } else {
        items = items.push(heading("File"));
        items = items.push(entry(
            "Open…",
            shortcut(Action::OpenDocument),
            Message::OpenDialog,
        ));
        let recent_toggle = button(
            container(
                row![
                    theme::typography::body("Open recent…"),
                    space::horizontal(),
                    theme::icon::icon(theme::Icon::ChevronRight, type_scale::LABEL),
                ]
                .align_y(Alignment::Center),
            )
            .center_y(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fixed(MENU_ROW))
        .padding(iced::Padding::from([0.0, gap::S]))
        .style(theme::ambient::tool_button)
        .on_press(Message::ToggleRecentMenu);
        items = items.push(recent_toggle);
        items = items.push(entry(
            "Reload",
            shortcut(Action::ReloadDocument),
            Message::Do(Action::ReloadDocument),
        ));
        if app.state.document().is_some() {
            // Beside Reload and Show in file manager: what a document *is* is
            // a question about the file that is open, not about the view of it.
            items = items.push(entry("Properties…", None, Message::ShowDocumentProperties));
        }
        if app.state.document().is_some() && app.platform.capabilities.native_dialogs {
            items = items.push(entry("Show in file manager", None, Message::RevealDocument));
        }
        items = items.push(heading("View"));
        if app.state.document().is_some() {
            items = items.push(entry(
                "Jump to page…",
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
            fullscreen_action_label(
                crate::layout::PrimaryViewer::of(&app.active_layout)
                    == crate::layout::PrimaryViewer::Document,
                app.reader_fullscreen,
                app.coordinator.roles.audience_fullscreen,
            ),
            shortcut(Action::ToggleAudienceFullscreen),
            Message::Do(Action::ToggleAudienceFullscreen),
        ));
        // Speech. In the menu as well as on a key, because a feature nobody
        // knows exists is one nobody uses — and because the reader most
        // likely to want it is the least likely to be hunting for an
        // unlabelled keystroke. What it offers depends on what this session
        // can actually do, which is the whole point of the tri-state.
        {
            use crate::platform::capabilities::Speech as Cap;
            use pulpit_core::speech::{Scope, SpeechState};

            items = items.push(heading("Read aloud"));
            match &app.platform.capabilities.speech {
                Cap::Unavailable { .. } => {
                    // Not a disabled row with no explanation: the settings
                    // page says why, and this points at it.
                    items = items.push(entry(
                        "Not available in this session…",
                        None,
                        Message::ShowSettings,
                    ));
                }
                Cap::Downloadable { .. } => {
                    items = items.push(entry("Download a voice…", None, Message::ShowSettings));
                }
                Cap::Ready { .. } => {
                    let state = app.speech.state();
                    let reading = app.speech.scope();
                    // One row per scope, each naming what its key will do
                    // *now* — so the menu answers "what happens if I press
                    // this" rather than making the reader infer it from a
                    // label that never changes.
                    let label =
                        |scopes: &[Scope], idle: &'static str| match (state.clone(), reading) {
                            (SpeechState::Idle, _) => idle,
                            (_, Some(active)) if !scopes.contains(&active) => idle,
                            (SpeechState::Paused, _) => "Resume",
                            _ => "Pause",
                        };
                    items = items.push(entry(
                        label(&[Scope::Document], "Read the whole document"),
                        shortcut(Action::SpeakToggle),
                        Message::SpeakToggleScope(Scope::Document),
                    ));
                    // One row for the page and the selection both, because
                    // they share one key: with text selected it reads the
                    // selection, otherwise the page — and while either is
                    // being read, this is the row that pauses it.
                    items = items.push(entry(
                        label(&[Scope::Page, Scope::Selection], "Read page or selection"),
                        shortcut(Action::SpeakPageToggle),
                        Message::SpeakToggleScope(Scope::Page),
                    ));
                    if state != SpeechState::Idle {
                        items = items.push(entry(
                            "Stop reading",
                            shortcut(Action::SpeakStop),
                            Message::SpeakStop,
                        ));
                    }
                    items = items.push(entry("Speech settings…", None, Message::ShowSettings));
                }
            }
        }

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
        // No "Diagnostics…" of its own: it sent `ShowSettings`, exactly as
        // "Settings…" above does, and what it promised is a section of that
        // page. Two entries for one destination is a menu that has to be read
        // twice to find out they are the same place.
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
    }

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

    // The menu hangs below the button's strip. The shortcut reference lays its
    // menu button over the page in the same corner the toolbar uses, so the
    // flyout hangs from exactly the same height there as everywhere else.
    let above = if app.shortcuts_open || app.state.document().is_none() {
        theme::controls::BUTTON_HEIGHT + gap::S * 2.0
    } else {
        flyout_top(app, crate::widgets::WidgetKind::MainMenu)
    };
    Row::new()
        .push(
            column![spacer(above), panel, rest()]
                .width(Length::Fixed(MENU_WIDTH + gap::M * 2.0 + gap::S)),
        )
        .push(beside)
        .into()
}

fn shortcut_hint<'a>(app: &'a App, action: Action) -> Element<'a, Message> {
    let (primary, alternate) = app.action_shortcut_parts(action);
    let mut hint = Row::new().spacing(2.0).align_y(Alignment::Center);
    // Every binding is a key in its own right. Standard bindings stay first,
    // but Vim/Zathura alternatives have the same visual weight instead of
    // being demoted to parenthetical prose.
    for key in shortcut_labels(primary, alternate) {
        hint = hint.push(shortcut_keycap(key));
    }
    hint.wrap().into()
}

fn shortcut_labels(primary: Vec<String>, alternate: Vec<String>) -> Vec<String> {
    primary.into_iter().chain(alternate).collect()
}

fn shortcut_keycap(label: String) -> Element<'static, Message> {
    container(text(label).size(type_scale::CAPTION))
        .padding(iced::Padding::from([2.0, gap::XS]))
        .style(theme::ambient::keycap)
        .into()
}

/// The fullscreen menu item names the state pressing it will enter.
fn fullscreen_action_label(
    document_viewer: bool,
    reader_fullscreen: bool,
    audience_fullscreen: bool,
) -> &'static str {
    let fullscreen = if document_viewer {
        reader_fullscreen
    } else {
        audience_fullscreen
    };
    if fullscreen {
        "Windowed"
    } else {
        "Fullscreen"
    }
}

fn shortcut_entry<'a>(app: &'a App, action: Action) -> Element<'a, Message> {
    container(
        row![
            container(theme::typography::label(action.label())).width(Length::FillPortion(2)),
            container(shortcut_hint(app, action))
                .width(Length::FillPortion(3))
                .align_x(Alignment::Start),
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center),
    )
    .padding(iced::Padding::from([gap::XS, gap::S]))
    .into()
}

fn shortcut_group<'a>(
    app: &'a App,
    title: &'static str,
    actions: &'static [Action],
) -> Element<'a, Message> {
    let mut content = Column::new().spacing(0).push(
        container(theme::typography::label(title).color(theme::ambient::accent()))
            .width(Length::Fill)
            .padding(iced::Padding::from([gap::XS, gap::S]))
            .style(theme::ambient::empty_cell),
    );
    for action in actions {
        content = content.push(shortcut_entry(app, *action));
    }
    content.into()
}

const SHORTCUT_TABLE_ALL: &[usize] = &[0, 1, 2, 3, 4, 5];
// Balanced by row count rather than by subject: the annotation group grew when
// presentation gained document mode's tools, again when the text selection
// and its copy chord joined it, and autoadvance joined the page-movement
// group, each time moving where an even split falls. A group is indivisible,
// so the constants name the evenest split there is rather than a tidy subject
// grouping — and the test holds them to exactly that. Where the counts allow
// a choice, the split follows what the keys are *for*: running a talk on the
// left, working through a document on the right.
const SHORTCUT_TABLE_LEFT: &[usize] = &[0, 2, 3, 6];
const SHORTCUT_TABLE_RIGHT: &[usize] = &[1, 4, 5];
const SHORTCUT_TABLE_WIDTH: f32 = 480.0;

fn split_shortcut_tables(width: f32) -> bool {
    width >= 1_100.0
}

fn shortcut_table<'a>(app: &'a App, groups: &[usize]) -> Element<'a, Message> {
    use crate::settings::keys::SHORTCUT_GROUPS;

    let mut table = Column::new().spacing(0);
    for index in groups {
        let group = SHORTCUT_GROUPS[*index];
        table = table.push(shortcut_group(app, group.title, group.actions));
    }
    container(table).width(Length::Fill).into()
}

fn shortcut_table_separator() -> Element<'static, Message> {
    let line = container(space::horizontal())
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(theme::ambient::separator);
    container(line)
        .width(Length::Fixed(gap::XXL))
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .into()
}

/// The one mode-neutral welcome and shortcut-reference surface.
///
/// If a document is already open, `can_close` exposes it again without
/// making a second, subtly different help layout.
fn shortcut_reference_page(app: &App, can_close: bool) -> Element<'_, Message> {
    let guide = responsive(move |size| {
        if split_shortcut_tables(size.width) {
            container(
                row![
                    container(
                        container(shortcut_table(app, SHORTCUT_TABLE_LEFT))
                            .width(Length::Fill)
                            .max_width(SHORTCUT_TABLE_WIDTH),
                    )
                    // Each half hugs the rule between them rather than
                    // centring in its own share of the window: a wide window
                    // should widen the margins, not the gutter.
                    .width(Length::FillPortion(1))
                    .align_x(Alignment::End),
                    shortcut_table_separator(),
                    container(
                        container(shortcut_table(app, SHORTCUT_TABLE_RIGHT))
                            .width(Length::Fill)
                            .max_width(SHORTCUT_TABLE_WIDTH),
                    )
                    .width(Length::FillPortion(1))
                    .align_x(Alignment::Start),
                ]
                .width(Length::Fill)
                .align_y(Alignment::Start),
            )
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .into()
        } else {
            container(
                container(shortcut_table(app, SHORTCUT_TABLE_ALL))
                    .width(Length::Fill)
                    .max_width(SHORTCUT_TABLE_WIDTH),
            )
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .into()
        }
    });

    let documentation =
        button(theme::typography::label(DOCUMENTATION_URL).color(theme::ambient::accent()))
            .style(theme::ambient::tool_button)
            .on_press(Message::OpenDocumentation);
    let brand = container(
        column![theme::typography::title("Pulpit"), documentation]
            .spacing(2.0)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .align_x(Alignment::Center);
    let content = column![brand, guide].spacing(gap::M).width(Length::Fill);

    // The centring has to happen *inside* the scrollable. A vertical
    // scrollable hands its child a full-width, infinite-height box, so the
    // outer container has nothing narrower than itself to centre and the
    // capped-width column just sits against the left edge.
    let compact = container(content).width(Length::Fill).max_width(1_600.0);
    let centred = container(compact)
        .width(Length::Fill)
        .align_x(Alignment::Center)
        .padding(gap::L);
    let surface = container(scrollable(centred).style(theme::ambient::scrollbar))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::ambient::surface);

    // The menu button sits in the window's own top-left corner, exactly where
    // the toolbar puts it on every other surface: it is the same control, so
    // it must not move when the welcome page stands in for a layout. Laying it
    // over the page rather than inside the centred column is what keeps it
    // there whatever the window's width does to that column.
    let mut corner = Row::new()
        .align_y(Alignment::Center)
        .push(menu_button(app))
        .push(space::horizontal());
    if can_close {
        let close = button(theme::icon::icon(theme::Icon::Close, type_scale::BODY))
            .padding(gap::XS)
            .style(theme::ambient::tool_button)
            .on_press(Message::CloseShortcuts);
        corner = corner.push(container(close).padding(gap::S));
    }
    stack![surface, corner.width(Length::Fill)].into()
}

/// What the open document *is*: its own description of itself.
///
/// A dialog rather than a rail view. The rail holds per-page navigation and is
/// read while moving through a document; this is one question about the whole
/// file, asked once and closed.
///
/// Every string in here was written by whoever produced the file. They are
/// drawn as text and nothing else — no markup, no links, no layout of their
/// own — and they arrive already bounded and flattened by
/// `pulpit_render::document::InfoText`, which is where that guarantee is made
/// rather than here.
fn document_properties_dialog(app: &App) -> Element<'_, Message> {
    use pulpit_render::document::PageSizes;

    let dismiss = Some(Message::CloseDocumentProperties);
    let mut body = Column::new()
        .spacing(gap::M)
        .push(theme::typography::title("Document properties"));

    // The file itself, which pulpit knows whether or not a worker answers.
    if let Some(path) = app.documents.active().map(|document| &document.path) {
        body = body.push(properties_row(
            "File",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        ));
    }

    let Some(properties) = app.document_properties.as_deref() else {
        // Two honest states, and neither of them is a table of blanks: the
        // answer is on its way, or it is not coming and this says why.
        body = body.push(match &app.document_properties_failed {
            Some(reason) => theme::typography::note(reason.clone()),
            None => theme::typography::note("Reading…"),
        });
        return panel(body, dismiss);
    };

    // What the document says it is. Absent keys are left out entirely: an
    // empty row would read as a document that filled the key in with nothing.
    let mut described = Column::new().spacing(gap::S);
    let mut said_anything = false;
    for (label, value) in [
        ("Title", properties.title.as_ref()),
        ("Author", properties.author.as_ref()),
        ("Subject", properties.subject.as_ref()),
        ("Keywords", properties.keywords.as_ref()),
    ] {
        if let Some(value) = value {
            said_anything = true;
            described = described.push(properties_row(label, value.to_string()));
        }
    }
    if !said_anything {
        described = described.push(theme::typography::note(
            "This document does not say what it is.",
        ));
    }
    body = body.push(dialog_section("Description", described));
    body = body.push(rule());

    let mut made = Column::new().spacing(gap::S);
    for (label, value) in [
        ("Created with", properties.creator.as_ref()),
        ("Converted by", properties.producer.as_ref()),
    ] {
        if let Some(value) = value {
            made = made.push(properties_row(label, value.to_string()));
        }
    }
    for (label, value) in [
        ("Created", properties.created.as_ref()),
        ("Modified", properties.modified.as_ref()),
    ] {
        if let Some(value) = value {
            made = made.push(properties_row(label, value.to_string()));
        }
    }
    if let Some(version) = properties.version {
        made = made.push(properties_row("PDF version", version.to_string()));
    }
    if made_is_empty(properties) {
        made = made.push(theme::typography::note(
            "This document does not say where it came from.",
        ));
    }
    body = body.push(dialog_section("Origin", made));
    body = body.push(rule());

    let mut pages = Column::new()
        .spacing(gap::S)
        .push(properties_row("Pages", properties.page_count.to_string()));
    let size = pulpit_render::document::describe_page_size(&properties.first_page);
    pages = pages.push(properties_row(
        "Page size",
        match properties.page_sizes {
            PageSizes::Uniform => size,
            // Named for what it is rather than hidden: a deck that mixes
            // orientations is the thing a presenter most wants warning of.
            PageSizes::Mixed => format!("{size} (first page; others differ)"),
            PageSizes::Unmeasured => format!("{size} (first page)"),
        },
    ));
    body = body.push(dialog_section("Pages", pages));
    body = body.push(rule());

    // The permissions, which decide whether editing and printing are refused.
    let mut access = Column::new().spacing(gap::S);
    match &properties.encryption {
        Some(encryption) => {
            access = access.push(properties_row(
                "Encryption",
                format!(
                    "{} (security handler revision {})",
                    encryption.label(),
                    encryption.revision
                ),
            ));
            for (label, allowed) in properties.permissions.each() {
                access = access.push(
                    row![
                        container(theme::typography::caption(label).color(theme::ambient::muted()))
                            .width(Length::Fixed(PROPERTIES_LABEL_WIDTH)),
                        // A word, not a colour alone: "allowed" and "refused"
                        // are legible to a reader who cannot tell the two
                        // apart by hue.
                        theme::typography::body(if allowed { "Allowed" } else { "Refused" }).color(
                            if allowed {
                                theme::ambient::text()
                            } else {
                                theme::ambient::alert()
                            }
                        ),
                    ]
                    .spacing(gap::M),
                );
            }
        }
        None => {
            access = access.push(theme::typography::body("Not encrypted."));
            access = access.push(theme::typography::note(
                "A document without an encryption dictionary declares no permissions, \
                 so nothing here is refused by the file itself.",
            ));
        }
    }
    body = body.push(dialog_section("Encryption and permissions", access));
    body = body.push(rule());

    // What pulpit will do with it, which is a fact about the document as much
    // as the pages are: a presenter would rather learn before the talk that a
    // deck's transitions will be cut than during it.
    let mut handling = Column::new()
        .spacing(gap::S)
        .push(properties_row("Support", properties.level.label().into()));
    for warning in &properties.warnings {
        handling = handling.push(theme::typography::note(warning.message()));
    }
    if let Some(findings) = app
        .documents
        .active()
        .and_then(|document| app.capabilities.get(&document.id.0))
    {
        for finding in &findings.findings {
            handling = handling.push(theme::typography::note(finding.describe()));
        }
    }
    body = body.push(dialog_section("What pulpit will do with it", handling));

    panel(body, dismiss)
}

/// Whether the Origin section has anything in it, asked before the section is
/// built so an empty one can say so rather than be a heading over nothing.
fn made_is_empty(properties: &pulpit_render::document::DocumentProperties) -> bool {
    properties.creator.is_none()
        && properties.producer.is_none()
        && properties.created.is_none()
        && properties.modified.is_none()
        && properties.version.is_none()
}

/// The width of the label column in the properties dialog. Fixed, so the
/// values line up into a column a reader can scan rather than being pushed
/// around by the length of the label beside them.
const PROPERTIES_LABEL_WIDTH: f32 = 140.0;

/// One `label: value` line of the properties dialog.
///
/// The value wraps rather than being cut: a producer string is the document's
/// own words, and half of one is worse than three lines of it.
fn properties_row<'a>(label: &'static str, value: String) -> Element<'a, Message> {
    row![
        container(theme::typography::caption(label).color(theme::ambient::muted()))
            .width(Length::Fixed(PROPERTIES_LABEL_WIDTH)),
        theme::typography::body(value).width(Length::Fill),
    ]
    .spacing(gap::M)
    .into()
}

fn about_overlay() -> Element<'static, Message> {
    let card = container(
        column![
            theme::typography::title("Pulpit"),
            theme::typography::body(format!("Version {}", env!("CARGO_PKG_VERSION"))),
            theme::typography::note(
                "A PDF presenter built for unreliable, changing display topologies.",
            ),
            button(theme::typography::label("Close"))
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
                theme::typography::tag(toast.intent.label(), intent),
                space::horizontal(),
                // Sized to the tag beside it, not to the caption below.
                button(theme::icon::icon(theme::Icon::Close, type_scale::LABEL))
                    .padding(gap::XS)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::DismissToast(toast.id)),
            ]
            .spacing(gap::S)
            .align_y(Alignment::Center),
        );
        // Borrowed, not cloned: the element already lives no longer than
        // `app`, and a toast redraws twenty times a second while shown.
        body = body.push(theme::typography::body(toast.message.as_str()));
        if let Some(action) = &toast.action {
            body = body.push(theme::typography::caption(action.as_str()));
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
            button(theme::typography::label(label))
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
        crate::widgets::WidgetEvent::ResetTimer => Message::Nav(Nav::ResetTimer),
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
    // Full-window secondary pages use the same top-right dismissal as the
    // shortcut reference and the document sidebar.
    let header = row![
        theme::typography::title("Settings"),
        space::horizontal(),
        button(theme::icon::icon(theme::Icon::Close, type_scale::BODY))
            .padding(gap::XS)
            .style(theme::ambient::tool_button)
            .on_press(Message::ShowPresenter),
    ]
    .align_y(Alignment::Center);

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
            button(theme::typography::label(appearance.label()).center())
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
        theme_section = theme_section.push(theme::typography::caption(
            "This system does not expose a light/dark preference, so the dark palette is in use.",
        ));
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
                button(theme::typography::label(setting.label())),
                app.settings.appearance.motion == setting,
            )
            .on_press(Message::SetMotion(setting)),
        );
    }
    body = body.push(section(
        "Motion",
        column![
            motions,
            theme::typography::caption(format!(
                "Currently {}. Reducing motion stops animated slide content \
                 from starting on its own; it can still be played from the \
                 presenter controls.",
                if app.motion.is_reduced() {
                    "reduced"
                } else {
                    "unrestricted"
                }
            )),
        ]
        .spacing(gap::S)
        .into(),
    ));

    // Blanking. One key does it, so the only thing left to say is which
    // colour, and that is a property of the room rather than the deck: black
    // vanishes in a dark hall, white reads as deliberate under bright house
    // lights. Two colours is one exclusive choice, so it is a radio group.
    let blank_color = app.settings.display.blank_color;
    let mut colours = Row::new().spacing(gap::M);
    for (colour, label) in [
        (crate::settings::BlankColor::Black, "Black"),
        (crate::settings::BlankColor::White, "White"),
    ] {
        colours = colours.push(
            radio(label, colour, Some(blank_color), Message::SetBlankColor)
                .size(type_scale::BODY)
                .text_size(type_scale::BODY),
        );
    }
    body = body.push(section(
        "Blank screen",
        column![
            colours,
            theme::typography::caption("What the blank key turns the audience screen into."),
        ]
        .spacing(gap::S)
        .into(),
    ));

    // Displays are chosen from the menu, where they are reachable mid-talk;
    // repeating them here would be a second place to keep in step.

    // What this desktop can and cannot do.
    let mut limitations = Column::new().spacing(gap::XS);
    for line in app.platform.capabilities.report() {
        limitations = limitations.push(theme::typography::caption(line));
    }
    body = body.push(section("This session", limitations.into()));

    // Autoadvance. The dwell is the only number pulpit can answer for the
    // room: how long a page stays up is a property of the screen it is left
    // on — a lobby loop and a poster in a corridor want different seconds —
    // and it has to be here rather than on a flag, because the person who
    // sets a screen up is not the person who launched it.
    let autoadvance = &app.settings.autoadvance;
    let seconds = column![
        row![
            text_input("5", &app.autoadvance_interval_draft)
                .on_input(Message::TypeAutoadvanceInterval)
                .style(theme::ambient::text_field)
                .padding(gap::S)
                .width(Length::Fixed(70.0)),
            theme::typography::body("seconds a page"),
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center),
        checkbox(autoadvance.wrap_at_end)
            .label("Start again at the first page")
            .size(type_scale::BODY)
            .text_size(type_scale::BODY)
            .on_toggle(Message::SetAutoadvanceWrap),
        checkbox(autoadvance.pause_on_interaction)
            .label("Hold when I take the controls")
            .size(type_scale::BODY)
            .text_size(type_scale::BODY)
            .on_toggle(Message::SetAutoadvancePause),
        theme::typography::caption(
            "Turns the page on its own, in whatever is open — a deck, a book, a scan, a \
             folder of images — presenting or reading, fullscreen or not. Press P to start \
             and stop it. Without wrapping it stops at the last page; holding means a key, \
             a click or the wheel puts it aside until you press P again.",
        ),
    ]
    .spacing(gap::S);
    body = body.push(section("Autoadvance", seconds.into()));

    body = body.push(section("Notes mapping", mappings(app)));

    body = body.push(section("Speech", speech_settings(app)));

    body = body.push(section("Signatures", signature_profiles_settings(app)));

    // Diagnostics. Rebuilt at most once a second: the report is a multi-KB
    // string whose paragraph iced re-shapes whenever its content changes,
    // and building it per view pass re-shaped it twenty times a second for
    // the whole time the page was open.
    let report = app.diagnostics_report();
    // The report scrolls against its own right edge — padding goes on the
    // text, not the container, so there is no dead strip beside the bar — and
    // the copy button sits in the corner of the box rather than above it.
    let copy = container(
        button(theme::typography::label("Copy"))
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

/// The Speech section (issue #20).
///
/// One place for the whole feature: what this session can do, the controls
/// that decide how it sounds, and the voice library it is downloaded from.
/// Deliberately not split between a settings page and some other dialog —
/// choosing a voice and getting a voice are the same errand, and a reader who
/// discovers they have none should be one click from fixing it.
fn speech_settings(app: &App) -> Element<'_, Message> {
    use crate::platform::capabilities::Speech as Cap;

    let mut body = Column::new().spacing(gap::M);

    // 1. What this session can do, in words. Never a greyed-out control with
    //    no explanation: "cannot" and "not yet" are different, and the reader
    //    can act on exactly one of them.
    match &app.platform.capabilities.speech {
        Cap::Unavailable { why } => {
            body = body.push(theme::typography::note(format!(
                "This session cannot read documents aloud: {why}. \
                 Downloading a voice would not change that."
            )));
            // Nothing below would do anything, so nothing below is drawn.
            return body.into();
        }
        Cap::Downloadable {
            bytes,
            needs_engine,
        } => {
            let what = if *needs_engine {
                "A voice and the speech engine need to be downloaded"
            } else {
                "A voice needs to be downloaded"
            };
            body = body.push(theme::typography::note(format!(
                "{what} before pulpit can read aloud — about {}. \
                 Choose a language below.",
                pulpit_media::speech::human_bytes(*bytes)
            )));
        }
        Cap::Ready { voices } => {
            let current = app
                .speech
                .current_voice()
                .map(|voice| voice.label())
                .unwrap_or_else(|| "none chosen".into());
            body = body.push(theme::typography::caption(format!(
                "{voices} voice{} installed. Reading with {current}.",
                if *voices == 1 { "" } else { "s" }
            )));
            // What it is doing right now, when it is doing anything. The
            // settings page is a plausible place to be while speech runs —
            // adjusting the speed is exactly what brings a reader here — so
            // it should not be the one page that cannot see the state.
            if let Some(scope) = app.speech.scope() {
                let state = match app.speech.state() {
                    pulpit_core::speech::SpeechState::Paused => "Paused",
                    pulpit_core::speech::SpeechState::AwaitingText(_) => "Loading",
                    _ => "Reading",
                };
                body = body.push(theme::typography::caption(format!(
                    "{state} {}.",
                    scope.label()
                )));
            }
        }
    }

    // 2. A download in progress, or one that has just ended.
    if let Some(download) = app.speech.download() {
        body = body.push(download_panel(app, download));
    }

    // 3. Speed. A slider rather than presets: the useful range is wide and
    //    the right value is personal — a screen-reader user may want three
    //    times what anyone else can follow.
    let rate = app.settings.speech.rate;
    body = body.push(
        column![
            row![
                theme::typography::label("Speed"),
                space::horizontal(),
                theme::typography::label(rate.label()),
            ]
            .align_y(Alignment::Center),
            iced::widget::slider(
                pulpit_core::speech::SpeechRate::SLOWEST..=pulpit_core::speech::SpeechRate::FASTEST,
                rate.get(),
                Message::SetSpeechRate,
            )
            .step(0.05_f32),
            theme::typography::caption(
                "Takes effect at the next sentence, so a change while reading \
                 is heard at a natural break rather than cutting a word in half."
            ),
        ]
        .spacing(gap::S),
    );

    // There is deliberately no scope control here. Each scope has its own
    // key — `r` reads the document, `Shift+R` this page — so a persisted
    // preference would be a row nothing consults; a control that changes
    // nothing teaches the reader the page is broken.

    // 4. Language. `Auto` must always show what it resolved to — an opaque
    //    "Auto" that picks wrong is worse than a wrong explicit choice,
    //    because the reader cannot see what happened or why.
    body = body.push(language_setting(app));

    // 6. The voice library, which is also where downloads happen.
    body = body.push(voice_library(app));

    body.into()
}

/// The `Auto` row, its resolved value, and the pinned-language picker.
fn language_setting(app: &App) -> Element<'_, Message> {
    use pulpit_core::speech::LanguageSetting;

    let is_auto = matches!(app.settings.speech.language, LanguageSetting::Auto);
    let mut choices = Row::new().spacing(gap::S).push(
        selectable(button(theme::typography::label("Auto")), is_auto)
            .on_press(Message::SetSpeechLanguage(None)),
    );

    // Only languages with something installed can be pinned: offering the
    // other forty would be offering a choice that cannot be honoured.
    let installed = app.speech.installed();
    let mut seen: Vec<pulpit_core::speech::LanguageTag> = Vec::new();
    for voice in &installed {
        let bare = voice.language.without_region();
        if seen.contains(&bare) {
            continue;
        }
        seen.push(bare.clone());
        let selected = matches!(
            &app.settings.speech.language,
            LanguageSetting::Explicit(tag) if tag.same_language(&bare)
        );
        choices = choices.push(
            selectable(
                button(theme::typography::label(voice.language_name.clone())),
                selected,
            )
            .on_press(Message::SetSpeechLanguage(Some(bare))),
        );
    }

    let explanation = if is_auto {
        match app.speech.current_language() {
            Some(language) => format!(
                "Following the document — currently {language}. A page has to \
                 be clearly in another language before the voice changes, so \
                 one quoted line will not switch it."
            ),
            None => "Following the document. The language is decided from the \
                     first page that has enough text to be sure."
                .to_string(),
        }
    } else {
        "Every page is read in this language, whatever the document says.".to_string()
    };

    let mut body = column![
        theme::typography::label("Language"),
        choices,
        theme::typography::caption(explanation),
    ]
    .spacing(gap::S);

    // A way back from "don't ask me again", which is otherwise a decision
    // with no undo.
    if !app.settings.speech.declined.is_empty() {
        let declined = app
            .settings
            .speech
            .declined
            .iter()
            .map(|tag| tag.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        body = body.push(
            row![
                theme::typography::caption(format!("Not offering downloads for: {declined}.")),
                space::horizontal(),
                button(theme::typography::label("Offer again"))
                    .padding(gap::XS)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::ForgetDeclinedLanguages),
            ]
            .align_y(Alignment::Center),
        );
    }

    body.into()
}

/// Progress, or the result, of the running download.
fn download_panel<'a>(
    _app: &'a App,
    download: &'a crate::speech::DownloadState,
) -> Element<'a, Message> {
    let mut body = Column::new().spacing(gap::S);
    match &download.outcome {
        None => {
            let fraction = download.progress.fraction();
            body = body
                .push(theme::typography::label(format!(
                    "Downloading {}…",
                    download.what
                )))
                .push(iced::widget::progress_bar(0.0..=1.0, fraction))
                .push(
                    row![
                        theme::typography::caption(match &download.progress {
                            pulpit_media::speech::Progress::Advanced { done, total } => format!(
                                "{} of {}",
                                pulpit_media::speech::human_bytes(*done),
                                pulpit_media::speech::human_bytes(*total)
                            ),
                            pulpit_media::speech::Progress::Finishing =>
                                "Checking and unpacking…".to_string(),
                        }),
                        space::horizontal(),
                        button(theme::typography::label("Cancel"))
                            .padding(gap::XS)
                            .style(theme::ambient::tool_button)
                            .on_press(Message::CancelVoiceDownload),
                    ]
                    .align_y(Alignment::Center),
                );
        }
        Some(Ok(())) => {
            body = body.push(
                row![
                    theme::typography::label(format!("{} is installed.", download.what)),
                    space::horizontal(),
                    button(theme::typography::label("Dismiss"))
                        .padding(gap::XS)
                        .style(theme::ambient::tool_button)
                        .on_press(Message::ClearVoiceDownload),
                ]
                .align_y(Alignment::Center),
            );
        }
        Some(Err(reason)) => {
            body = body
                .push(theme::typography::label("The download did not finish."))
                .push(theme::typography::caption(reason.clone()))
                .push(
                    row![
                        space::horizontal(),
                        button(theme::typography::label("Dismiss"))
                            .padding(gap::XS)
                            .style(theme::ambient::tool_button)
                            .on_press(Message::ClearVoiceDownload),
                    ]
                    .align_y(Alignment::Center),
                );
        }
    }
    container(body)
        .padding(gap::M)
        .width(Length::Fill)
        .style(theme::ambient::surface)
        .into()
}

/// Voices by language: installed first, one language expanded at a time.
fn voice_library(app: &App) -> Element<'_, Message> {
    let groups = crate::speech::browsable(app.speech.catalog(), app.speech.store());
    let busy = app
        .speech
        .download()
        .is_some_and(crate::speech::DownloadState::is_running);

    let mut list = Column::new().spacing(gap::XS);
    for (name, tag, voices) in groups {
        let installed_here = voices.iter().filter(|voice| voice.installed).count();
        let expanded = app.expanded_speech_language.as_ref() == Some(&tag);

        let summary = if installed_here > 0 {
            format!("{installed_here} installed")
        } else {
            format!("{} available", voices.len())
        };
        list = list.push(
            button(
                row![
                    theme::typography::label(name.clone()),
                    space::horizontal(),
                    theme::typography::caption(summary),
                ]
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::ToggleSpeechLanguage(tag.clone())),
        );

        if !expanded {
            continue;
        }
        for voice in voices {
            let chosen = app.settings.speech.voice.as_deref() == Some(voice.id.as_str());
            let mut controls = Row::new().spacing(gap::S).align_y(Alignment::Center);
            if voice.installed {
                controls = controls.push(
                    selectable(button(theme::typography::label("Use")), chosen)
                        .on_press(Message::SetSpeechVoice(voice.id.clone())),
                );
                controls = controls.push(
                    button(theme::typography::label("Remove"))
                        .padding(gap::S)
                        .style(theme::ambient::tool_button)
                        .on_press(Message::RemoveVoice(voice.id.clone())),
                );
            } else {
                // The size is on the button, not in a tooltip: a reader on a
                // conference network is entitled to know what they are about
                // to spend before they spend it.
                let label = format!(
                    "Download — {}",
                    pulpit_media::speech::human_bytes(voice.bytes)
                );
                let mut download = button(theme::typography::label(label))
                    .padding(gap::S)
                    .style(theme::ambient::tool_button);
                if !busy {
                    download = download.on_press(Message::DownloadVoice(voice.id.clone()));
                }
                controls = controls.push(download);
            }

            list = list.push(
                container(
                    row![
                        column![
                            theme::typography::label(voice.label.clone()),
                            theme::typography::caption(format!("{} Hz", voice.sample_rate)),
                        ]
                        .spacing(gap::XS),
                        space::horizontal(),
                        controls,
                    ]
                    .align_y(Alignment::Center),
                )
                .padding(iced::Padding {
                    left: gap::L,
                    ..iced::Padding::from(gap::S)
                })
                .width(Length::Fill),
            );
        }
    }

    column![
        theme::typography::label("Voices"),
        theme::typography::caption(format!(
            "Voices are downloaded once and kept in {}. Each is checked \
             against a published checksum before it is used, and discarded if \
             it does not match.",
            crate::speech::store_location(&crate::platform::Directories::detect().data).display()
        )),
        container(
            scrollable(list)
                .height(Length::Fixed(300.0))
                .style(theme::ambient::scrollbar)
        )
        .style(theme::ambient::surface)
        .width(Length::Fill),
    ]
    .spacing(gap::S)
    .into()
}

fn signature_profiles_settings(app: &App) -> Element<'_, Message> {
    use crate::signature_profiles::ProfileMsg;

    let mut profiles = Column::new().spacing(gap::S);
    if app.settings.signatures.profiles.is_empty() {
        profiles = profiles.push(theme::typography::note(
            "No signature profiles have been created.",
        ));
    }
    for profile in &app.settings.signatures.profiles {
        let is_default =
            app.settings.signatures.default_profile.as_deref() == Some(profile.id.as_str());
        let credential_kind = match profile.credential {
            crate::settings::StoredCredential::Managed => "Managed credential",
            crate::settings::StoredCredential::External { .. } => "External credential",
        };
        let mut actions = Row::new().spacing(gap::S).align_y(Alignment::Center);
        if is_default {
            actions = actions.push(theme::typography::caption("Default"));
        } else {
            actions = actions.push(
                button(theme::typography::label("Make default"))
                    .padding(gap::XS)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::SignatureProfile(ProfileMsg::SetDefault(
                        profile.id.clone(),
                    ))),
            );
        }
        actions = actions
            .push(
                button(theme::typography::label("Edit"))
                    .padding(gap::XS)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::SignatureProfile(ProfileMsg::StartEdit(
                        profile.id.clone(),
                    ))),
            )
            .push(
                button(theme::typography::label("Remove…"))
                    .padding(gap::XS)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::SignatureProfile(ProfileMsg::AskRemove(
                        profile.id.clone(),
                    ))),
            );

        let fingerprint = &profile.identity.sha256_fingerprint;
        let short_fingerprint = if fingerprint.len() > 16 {
            format!(
                "{}…{}",
                &fingerprint[..8],
                &fingerprint[fingerprint.len() - 8..]
            )
        } else {
            fingerprint.clone()
        };
        profiles = profiles.push(
            container(
                row![
                    column![
                        theme::typography::body(profile.name.clone()),
                        theme::typography::caption(format!(
                            "{} · {} · {}",
                            crate::signature_profiles::common_name(&profile.identity.subject),
                            profile.identity.key_algorithm,
                            short_fingerprint
                        )),
                        theme::typography::caption(credential_kind),
                    ]
                    .spacing(gap::XS)
                    .width(Length::Fill),
                    actions,
                ]
                .spacing(gap::M)
                .align_y(Alignment::Center),
            )
            .padding(gap::M)
            .width(Length::Fill)
            .style(theme::ambient::surface),
        );
    }

    column![
        theme::typography::caption(
            "Signing identities and their visible appearances. Passphrases are never stored."
        ),
        profiles,
        button(theme::typography::label("Add signature profile…"))
            .padding(gap::S)
            .style(theme::ambient::selected_button)
            .on_press(Message::SignatureProfile(ProfileMsg::StartAdd)),
    ]
    .spacing(gap::M)
    .into()
}

fn signature_profile_editor<'a>(
    app: &'a App,
    editor: &'a crate::signature_profiles::ProfileEditor,
) -> Element<'a, Message> {
    use crate::settings::{StoredSignatureContent, StoredSignaturePosition, StoredSignatureSize};
    use crate::signature_profiles::{ProfileMsg, ProfileSource, SignaturePad};

    let title = if editor.is_editing() {
        "Edit signature profile"
    } else {
        "Add signature profile"
    };
    let mut body = Column::new()
        .spacing(gap::M)
        .push(theme::typography::title(title))
        .push(dialog_section(
            "Profile name",
            text_input("Personal, Work…", &editor.name)
                .on_input(|value| Message::SignatureProfile(ProfileMsg::NameChanged(value)))
                .style(theme::ambient::text_field)
                .padding(gap::S),
        ));

    if editor.is_editing() {
        if let Some(identity) = editor.identity.as_ref() {
            body = body.push(dialog_section(
                "Signing identity",
                column![
                    theme::typography::body(identity.subject.clone()),
                    theme::typography::caption(format!(
                        "{} · valid {} — {}",
                        identity.key_algorithm, identity.not_before, identity.not_after
                    )),
                    theme::typography::caption(format!("SHA-256 {}", identity.sha256_fingerprint)),
                    theme::typography::caption("Changing the certificate creates a new profile."),
                ]
                .spacing(gap::XS),
            ));
        }
    } else {
        let mut sources = Row::new().spacing(gap::S);
        for source in ProfileSource::ALL {
            sources = sources.push(
                selectable(
                    button(theme::typography::label(source.label())),
                    editor.source == source,
                )
                .on_press(Message::SignatureProfile(ProfileMsg::SourceChanged(source))),
            );
        }
        body = body.push(dialog_section("Credential", sources));

        match editor.source {
            ProfileSource::Create => {
                body = body
                    .push(dialog_section(
                        "Full name",
                        text_input("Name in the certificate", &editor.full_name)
                            .on_input(|value| {
                                Message::SignatureProfile(ProfileMsg::FullNameChanged(value))
                            })
                            .style(theme::ambient::text_field)
                            .padding(gap::S),
                    ))
                    .push(dialog_section(
                        "Organization (optional)",
                        text_input("Organization", &editor.organization)
                            .on_input(|value| {
                                Message::SignatureProfile(ProfileMsg::OrganizationChanged(value))
                            })
                            .style(theme::ambient::text_field)
                            .padding(gap::S),
                    ))
                    .push(dialog_section(
                        "Email (optional)",
                        text_input("name@example.com", &editor.email)
                            .on_input(|value| {
                                Message::SignatureProfile(ProfileMsg::EmailChanged(value))
                            })
                            .style(theme::ambient::text_field)
                            .padding(gap::S),
                    ))
                    .push(
                        theme::typography::caption(
                            "Pulpit will create an encrypted, self-signed ECDSA P-256 credential. Its signatures prove integrity, but other software will not automatically trust the identity.",
                        ),
                    );
            }
            ProfileSource::Existing => {
                let path = editor.external_path.as_ref().map_or_else(
                    || "No credential selected".to_string(),
                    |path| path.display().to_string(),
                );
                body = body.push(dialog_section(
                    "Credential file",
                    column![
                        theme::typography::caption(path),
                        button(theme::typography::label("Choose .p12 or .pfx…"))
                            .padding(gap::S)
                            .style(theme::ambient::tool_button)
                            .on_press(Message::SignatureProfile(ProfileMsg::ChooseExternal)),
                    ]
                    .spacing(gap::S),
                ));
            }
        }

        let mut passphrases = column![text_input("Passphrase", &editor.passphrase)
            .secure(true)
            .on_input(|value| { Message::SignatureProfile(ProfileMsg::PassphraseChanged(value)) })
            .style(theme::ambient::text_field)
            .padding(gap::S),]
        .spacing(gap::S);
        if editor.source == ProfileSource::Create {
            passphrases = passphrases.push(
                text_input("Confirm passphrase", &editor.confirm_passphrase)
                    .secure(true)
                    .on_input(|value| {
                        Message::SignatureProfile(ProfileMsg::ConfirmPassphraseChanged(value))
                    })
                    .style(theme::ambient::text_field)
                    .padding(gap::S),
            );
        }
        passphrases = passphrases.push(theme::typography::caption(
            "The passphrase cannot be recovered and is never stored by Pulpit.",
        ));
        body = body.push(dialog_section("Passphrase", passphrases));
    }

    let mut content_choices = Row::new().spacing(gap::S);
    for content in StoredSignatureContent::ALL {
        content_choices = content_choices.push(
            selectable(
                button(theme::typography::label(content.label())),
                editor.appearance.content == content,
            )
            .on_press(Message::SignatureProfile(ProfileMsg::ContentChanged(
                content,
            ))),
        );
    }
    body = body.push(dialog_section("Appearance", content_choices));

    if editor.appearance.content.uses_ink() {
        let pad = canvas(SignaturePad {
            strokes: &editor.appearance.strokes,
            stroke_width: editor.appearance.stroke_width,
            palette: app.theme.palette,
        })
        .width(Length::Fill)
        .height(Length::Fixed(180.0));
        body = body.push(dialog_section(
            "Draw signature",
            column![
                container(pad)
                    .width(Length::Fill)
                    .style(theme::ambient::canvas),
                button(theme::typography::label("Clear ink"))
                    .padding(gap::XS)
                    .style(theme::ambient::tool_button)
                    .on_press_maybe(
                        (!editor.appearance.strokes.is_empty())
                            .then_some(Message::SignatureProfile(ProfileMsg::ClearInk),)
                    ),
            ]
            .spacing(gap::S),
        ));
    }

    // The one place visibility is decided. The Sign flow does not ask —
    // see `crate::signing`'s module doc — so this tick box is what stands
    // between a profile and a signature nobody can see on the page.
    body = body.push(dialog_section(
        "Visibility",
        column![
            checkbox(editor.appearance.visible)
                .label("Draw the signature on the page")
                .size(type_scale::BODY)
                .text_size(type_scale::BODY)
                .on_toggle(|value| Message::SignatureProfile(ProfileMsg::VisibleChanged(value))),
            theme::typography::caption(
                "Off means an invisible signature: cryptographically identical, with no mark \
                 drawn. Signing into a field the sender drew a box for is always visible, \
                 whatever this says.",
            ),
        ]
        .spacing(gap::XS),
    ));

    if editor.appearance.visible {
        let mut positions = Row::new().spacing(gap::XS);
        for position in StoredSignaturePosition::ALL {
            positions = positions.push(
                selectable(
                    button(theme::typography::label(position.label())),
                    editor.appearance.position == position,
                )
                .on_press(Message::SignatureProfile(ProfileMsg::PositionChanged(
                    position,
                ))),
            );
        }
        let mut sizes = Row::new().spacing(gap::XS);
        for size in StoredSignatureSize::ALL {
            sizes = sizes.push(
                selectable(
                    button(theme::typography::label(size.label())),
                    editor.appearance.size == size,
                )
                .on_press(Message::SignatureProfile(ProfileMsg::SizeChanged(size))),
            );
        }
        body = body
            .push(dialog_section("Default position", positions.wrap()))
            .push(dialog_section("Default size", sizes));
    }

    if let Some(error) = editor.error.as_ref() {
        body = body.push(theme::typography::body(error.clone()).color(theme::ambient::alert()));
    }
    let primary_label = if editor.busy {
        "Creating credential…"
    } else if editor.is_editing() {
        "Save changes"
    } else {
        "Create profile"
    };
    body = body.push(
        row![
            button(theme::typography::label("Cancel"))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press_maybe(
                    (!editor.busy).then_some(Message::SignatureProfile(ProfileMsg::CancelEdit,))
                ),
            button(theme::typography::label(primary_label))
                .padding(gap::S)
                .style(theme::ambient::selected_button)
                .on_press_maybe(
                    (!editor.busy).then_some(Message::SignatureProfile(ProfileMsg::Save,))
                ),
        ]
        .spacing(gap::S),
    );

    panel(
        scrollable(body)
            .height(Length::FillPortion(9))
            .style(theme::ambient::scrollbar),
        (!editor.busy).then_some(Message::SignatureProfile(ProfileMsg::CancelEdit)),
    )
}

fn signature_profile_removal<'a>(
    app: &'a App,
    removal: &'a crate::signature_profiles::ProfileRemoval,
) -> Element<'a, Message> {
    use crate::settings::StoredCredential;
    use crate::signature_profiles::ProfileMsg;

    let Some(profile) = app.settings.signatures.profile(&removal.id) else {
        return space().into();
    };
    let managed = matches!(profile.credential, StoredCredential::Managed);
    let mut actions = row![
        button(theme::typography::label("Cancel"))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::SignatureProfile(ProfileMsg::CancelRemove)),
        button(theme::typography::label("Remove profile only"))
            .padding(gap::S)
            .style(if managed {
                theme::ambient::tool_button
            } else {
                theme::ambient::alert_button
            })
            .on_press(Message::SignatureProfile(ProfileMsg::ConfirmRemove {
                delete_credential: false,
            })),
    ]
    .spacing(gap::S);
    if managed {
        actions = actions.push(
            button(theme::typography::label("Remove and delete credential"))
                .padding(gap::S)
                .style(theme::ambient::alert_button)
                .on_press(Message::SignatureProfile(ProfileMsg::ConfirmRemove {
                    delete_credential: true,
                })),
        );
    }
    let mut body = column![
        theme::typography::title(format!("Remove “{}”?", profile.name)),
        theme::typography::body(if managed {
            "Removing the profile can leave its encrypted credential file in place, or delete both. Documents already signed with it are unaffected."
        } else {
            "Pulpit will forget this profile. The external credential file will not be deleted."
        }),
        actions,
    ]
    .spacing(gap::M);
    if let Some(error) = removal.error.as_ref() {
        body = body.push(theme::typography::body(error.clone()).color(theme::ambient::alert()));
    }
    panel(
        body,
        Some(Message::SignatureProfile(ProfileMsg::CancelRemove)),
    )
}

/// "This page is in German. Download a German voice?"
///
/// The dialog that makes `Auto` honest. Declining is remembered for the rest
/// of the session so a bilingual document asks once rather than at every page
/// turn, and the settings page has the way back.
fn missing_voice_dialog(prompt: &crate::speech::MissingVoicePrompt) -> Element<'_, Message> {
    let actions = row![
        button(theme::typography::label("Not for this language"))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::AnswerVoicePrompt(false)),
        button(theme::typography::label(format!(
            "Download — {}",
            pulpit_media::speech::human_bytes(prompt.bytes)
        )))
        .padding(gap::S)
        .style(theme::ambient::selected_button)
        .on_press(Message::AnswerVoicePrompt(true)),
    ]
    .spacing(gap::S);

    let body = column![
        theme::typography::title(format!("This page is in {}", prompt.language_name)),
        theme::typography::body(format!(
            "No {} voice is installed, so pulpit cannot read this page properly. \
             {} can be downloaded now.",
            prompt.language_name, prompt.voice_label
        )),
        theme::typography::caption(
            "Declining keeps the current voice and stops asking about this \
             language. Settings ▸ Speech can offer it again later."
        ),
        actions,
    ]
    .spacing(gap::M);

    panel(body, Some(Message::AnswerVoicePrompt(false)))
}

fn color_editor(app: &App) -> Element<'_, Message> {
    let scheme = app.editing_colors;
    let colors = &app.settings.appearance.colors;
    let palette = colors.palette(scheme);

    // An ordinary control, drawn like the ordinary controls around it: the
    // press only opens a question, and the red belongs on the answer to it.
    let reset = button(theme::typography::label("Reset to Pulpit defaults…"))
        .padding(gap::S)
        .style(theme::ambient::tool_button)
        .on_press_maybe(
            (colors.has_overrides() || !app.color_drafts.is_empty())
                .then_some(Message::AskResetColors),
        );

    // Two roles to a row rather than seven in a stack: each cell is the
    // role's name over its swatch and hex field, and the sentence that used
    // to sit under every name rides on the name as a hover hint instead. The
    // stack scrolled past a screen to say what the grid says at a glance.
    let mut roles = Column::new().spacing(gap::M);
    for pair in crate::theme::ColorRole::ALL.chunks(2) {
        let mut grid_row = Row::new().spacing(gap::L);
        for &role in pair {
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
            let name = tooltip(
                theme::typography::body(role.label()),
                container(theme::typography::caption(role.description()))
                    .padding(gap::S)
                    .max_width(280)
                    .style(theme::ambient::dialog),
                tooltip::Position::Top,
            );
            let mut cell = column![
                name,
                row![
                    // The swatch is the wheel's handle. Typing `#RRGGBB`
                    // stays the reproducible path; the wheel is the choosing
                    // path.
                    role_swatch(app, role, swatch),
                    field,
                ]
                .spacing(gap::S)
                .align_y(Alignment::Center),
            ]
            .spacing(gap::XS)
            .width(Length::Fixed(190.0));
            if parsed.is_none() {
                cell = cell.push(
                    theme::typography::caption("Use a six-digit HEX color such as #C9CCD4.")
                        .color(theme::ambient::alert()),
                );
            } else if let Some(warning) = contrast_warning(palette, role) {
                cell =
                    cell.push(theme::typography::caption(warning).color(theme::ambient::alert()));
            }
            grid_row = grid_row.push(cell);
        }
        roles = roles.push(grid_row);
    }

    column![
        column![
            theme::typography::field("Edit palette"),
            row![
                pick_list(
                    crate::settings::ColorScheme::ALL,
                    Some(scheme),
                    Message::EditColorScheme,
                )
                .width(Length::Fixed(140.0))
                .style(theme::ambient::drop_down)
                .menu_style(theme::ambient::drop_down_menu),
                reset,
            ]
            .spacing(gap::M)
            .align_y(Alignment::Center),
        ]
        .spacing(gap::S),
        theme::typography::caption("Every view and widget inherits these roles. High contrast remains controlled by the system."),
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
        button(theme::typography::label(label))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::Alarm(AlarmCommand::DraftFromNow(minutes * 60)))
    };

    // What is already set, each with the way to take it off again.
    let mut list = column![].spacing(gap::S);
    if controls.alarms.is_empty() {
        list = list.push(theme::typography::note("No alarms set."));
    }
    for alarm in &controls.alarms {
        let passed = alarm.at < crate::view::seconds_of_day();
        list = list.push(
            row![
                theme::typography::body(options.format_alarm(alarm.at))
                    // A cue that has gone by is dimmed rather than removed:
                    // seeing that 14:20 has passed is worth a line.
                    .color(if passed {
                        theme::ambient::muted()
                    } else {
                        theme::ambient::text()
                    }),
                space::horizontal(),
                button(theme::typography::label("Remove"))
                    .padding(gap::S)
                    .style(theme::ambient::tool_button)
                    .on_press(Message::Alarm(AlarmCommand::Remove(alarm.at))),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(gap::S),
        );
    }

    let entered = controls.entered();
    let mut add = button(theme::typography::label("Add"))
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
        let mut control = button(theme::typography::label(label).color(if ambiguous {
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
        entry_row =
            entry_row.push(theme::typography::body("not a time").color(theme::ambient::alert()));
    }

    // Three questions, in the order they are asked: set one, see what is set,
    // say what happens when one goes off.
    let body = column![
        theme::typography::title("Alarms"),
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
        theme::typography::caption(if controls.is_full() {
            "That is as many alarms as pulpit will hold."
        } else {
            "Escape or a press outside closes this. A cue that goes off is dismissed with Escape too."
        }),
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
/// One labelled group inside a dialog: a field name, then the control or
/// controls it names.
///
/// The page-level counterpart is [`section`], whose header is set in
/// `HEADING` because it introduces a whole region of a page. A dialog group
/// names one field, so it takes the field role instead — which is still a
/// weight above, and never a size below, the controls it governs.
fn dialog_section<'a>(
    title: &'static str,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![theme::typography::field(title), body.into()]
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
        button(theme::typography::label(label))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(message)
    };
    row![
        theme::typography::note(format!("Snooze for {minutes}m")),
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
        button(theme::typography::label(label))
            .padding(gap::S)
            .style(if chosen {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            })
            .on_press(Message::Timer(TimerCommand::SetCountDown(count_down)))
    };
    let step = |label: &'static str, delta: i32| {
        button(theme::typography::label(label))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::Timer(TimerCommand::NudgeTarget(delta)))
    };
    let preset = |label: &'static str, minutes: u32| {
        button(theme::typography::label(label))
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
    let mut set = button(theme::typography::label("Set"))
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
        theme::typography::title("Timer"),
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
                    theme::typography::note(length),
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
        theme::typography::caption("A countdown needs a length to count to; asking for one without a target sets 20 minutes."),
        // Clearing the target is the one press here that is not a way out.
        dialog_footer(
            button(theme::typography::label("No target"))
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
        theme::typography::title("Reset colors?"),
        theme::typography::body(
            "This will replace your custom Light and Dark colors with the default Pulpit theme."
        ),
        row![
            button(theme::typography::label("Cancel"))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::CancelResetColors),
            button(theme::typography::label("Reset colors"))
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
        theme::typography::title("Follow this document's request?"),
        theme::typography::body(format!("This form asked to {}.", request.what)),
        theme::typography::note("Nothing moves until you choose."),
        row![
            button(theme::typography::label("Stay here"))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::DeclineFormNavigation),
            button(theme::typography::label("Go"))
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

/// What goes on the paper.
///
/// As little as the session allows, because everything a print dialog usually
/// asks — paper size, duplex, trays, colour — belongs to the platform's own
/// dialog and its drivers, and pulpit hands the file over rather than
/// answering them a second time.
///
/// Where the desktop has a print dialog of its own this is down to a single
/// question: whether the paper carries the reader's marks and form entries.
/// That one is here because no system dialog can ask it — the marks are not
/// in the file yet, and the dialog is being handed a file. Everything else
/// comes next, from the desktop.
///
/// Where there is no system dialog, pulpit asks the ones its spooler will
/// honour, because otherwise nobody asks and the reader finds out at the
/// printer.
fn print_dialog(app: &App) -> Element<'_, Message> {
    use crate::printing::{Marks, PageChoice};
    let Some(dialog) = app.print_dialog.as_ref() else {
        return blank();
    };
    let close = Message::Print(crate::app::PrintMsg::Close);
    let page_count = app.reader.page_count();
    let current = app.reader.current_page();

    let mut body = column![theme::typography::title("Print")].spacing(gap::M);

    // What the document itself asks for, when it asks for anything. Shown
    // before the choices rather than after them: it may change what the
    // reader does next.
    if let Some(caution) = dialog
        .permission
        .and_then(crate::printing::Permission::caution)
    {
        let mut notice = column![theme::typography::body(caution)].spacing(gap::S);
        if dialog
            .permission
            .is_some_and(crate::printing::Permission::needs_an_answer)
            && !dialog.permission_answered
        {
            notice = notice.push(
                button(theme::typography::label("Print it anyway"))
                    .padding(gap::S)
                    .style(theme::ambient::alert_button)
                    .on_press(Message::Print(crate::app::PrintMsg::AcceptPermission)),
            );
        }
        body = body.push(notice);
    }

    // Which pages, but only where nothing else is going to ask. The range box
    // is always there rather than appearing when "Pages" is chosen: a box that
    // appears under the pointer as it arrives is a box that gets missed.
    if dialog.asks_particulars {
        let mut pages = row![].spacing(gap::S).align_y(Alignment::Center);
        for choice in [PageChoice::All, PageChoice::Current, PageChoice::Custom] {
            let chosen = dialog.choice == choice;
            pages = pages.push(
                button(theme::typography::label(choice.label()))
                    .padding(gap::S)
                    .style(if chosen {
                        theme::ambient::selected_button
                    } else {
                        theme::ambient::tool_button
                    })
                    .on_press(Message::Print(crate::app::PrintMsg::ChoosePages(choice))),
            );
        }
        pages = pages.push(
            text_input("1-3, 7", &dialog.custom)
                .on_input(|value| Message::Print(crate::app::PrintMsg::TypeRange(value)))
                .style(theme::ambient::text_field)
                .padding(gap::S)
                .width(Length::Fixed(120.0)),
        );
        body = body.push(dialog_section("Pages", pages));
    }

    // What is on them.
    let mut marks = row![].spacing(gap::S);
    for kind in [Marks::AsMarkedUp, Marks::AsOnDisk] {
        let chosen = dialog.marks == kind;
        marks = marks.push(
            button(theme::typography::label(kind.label()))
                .padding(gap::S)
                .style(if chosen {
                    theme::ambient::selected_button
                } else {
                    theme::ambient::tool_button
                })
                .on_press(Message::Print(crate::app::PrintMsg::ChooseMarks(kind))),
        );
    }
    body = body.push(dialog_section("What to print", marks));
    if dialog.marks == Marks::AsMarkedUp {
        // Said plainly, because it is the one thing about this print that is
        // not obvious: a copy is written, and it is not the document.
        body = body.push(theme::typography::note(
            "pulpit writes a temporary copy carrying your marks and form entries, sends \
             that to the printer, and deletes it. The document you opened is not changed.",
        ));
    }

    // Copies and the queue. Three ways this can go, and the session decides
    // which: the desktop's own dialog is about to ask, or pulpit asks because
    // the spooler will honour the answers, or nobody can ask at all.
    if !dialog.asks_particulars && app.platform.capabilities.system_print_dialog {
        // Not silence: a reader who came here for a page range needs to know
        // where it went, or they will think pulpit lost it.
        body = body.push(theme::typography::note(
            "Your printer, which pages, how many copies and the paper come next, in this \
             desktop's own print dialog.",
        ));
    } else if app.platform.capabilities.print_options {
        let mut particulars = row![dialog_section(
            "Copies",
            text_input("1", &dialog.copies.to_string())
                .on_input(|value| Message::Print(crate::app::PrintMsg::TypeCopies(value)))
                .style(theme::ambient::text_field)
                .padding(gap::S)
                .width(Length::Fixed(70.0)),
        )]
        .spacing(gap::M)
        .align_y(Alignment::End);
        // Only when there is a choice to make. One queue, or none the
        // platform will name, is the platform's default and nothing to ask
        // about.
        if dialog.destinations.len() > 1 {
            let names: Vec<String> = dialog.destinations.clone();
            let chosen = dialog
                .destination
                .clone()
                .unwrap_or_else(|| names[0].clone());
            particulars = particulars.push(dialog_section(
                "Printer",
                pick_list(names, Some(chosen), |name| {
                    Message::Print(crate::app::PrintMsg::ChooseDestination(Some(name)))
                })
                .width(Length::Fixed(200.0))
                .style(theme::ambient::drop_down)
                .menu_style(theme::ambient::drop_down_menu),
            ));
        }
        body = body.push(particulars);
    } else {
        // The honest version of a dialog with no controls in it: this
        // session's spooler takes a file and nothing else.
        body = body.push(theme::typography::note(
            "This session prints to the default printer, and cannot be told a page range \
             or a number of copies from here.",
        ));
    }

    let blocked = dialog.blocked(current, page_count);
    // An ellipsis because there is more to answer: the desktop's dialog
    // opens next. Without it the button promises paper, and the reader who
    // gets a second dialog thinks something went wrong.
    let label = if dialog.asks_particulars {
        "Print"
    } else {
        "Print…"
    };
    let mut print = button(theme::typography::label(label))
        .padding(gap::S)
        .style(theme::ambient::alert_button);
    if blocked.is_none() {
        print = print.on_press(Message::Print(crate::app::PrintMsg::Send));
    }
    body = body.push(
        row![
            button(theme::typography::label("Cancel"))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(close.clone()),
            print,
            // What this is about to cost in paper, or what is stopping it.
            // One line, in the place the reader is already looking.
            theme::typography::note(match blocked.as_deref() {
                Some(reason) => reason.to_string(),
                None => dialog.summary(current, page_count),
            }),
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center),
    );

    // The ground behind cancels: printing nothing is the answer that costs
    // nothing.
    panel(body, Some(close))
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
        theme::typography::title("Save with required fields empty?"),
        theme::typography::body(review.headline()),
        theme::typography::body(review.listing()),
    ]
    .spacing(gap::M);

    let mut actions = row![button(theme::typography::label("Cancel"))
        .padding(gap::S)
        .style(theme::ambient::tool_button)
        .on_press(Message::CancelSaveReview),]
    .spacing(gap::S);
    // Only offered when there is somewhere to go: a field the producer left
    // unnamed cannot be focused, and a button that does nothing is worse than
    // no button.
    if review.first_named().is_some() {
        actions = actions.push(
            button(theme::typography::label("Review"))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::ReviewRequiredFields),
        );
    }
    actions = actions.push(
        button(theme::typography::label("Save anyway"))
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
        button(theme::typography::label(label))
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
    let mut body = column![
        row![theme::typography::title("Signatures"), space::horizontal(),]
            .align_y(Alignment::Center),
    ]
    .spacing(gap::M);

    for (index, entry) in app.document_signatures.iter().enumerate() {
        let line = crate::signing::signature_line_for_verification(entry);
        let mut row_body = column![row![
            theme::typography::body(line.summary_text()),
            space::horizontal(),
            button(theme::typography::label("Details"))
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
                        theme::typography::caption(format!("Field: {}", status.field_name)),
                        theme::typography::caption(format!(
                            "Subject: {}",
                            status.signer_cert.subject
                        )),
                        theme::typography::caption(format!(
                            "Issuer: {}",
                            status.signer_cert.issuer
                        )),
                        theme::typography::caption(format!(
                            "Serial: {}",
                            status.signer_cert.serial
                        )),
                        theme::typography::caption(format!(
                            "Fingerprint (SHA-256): {}",
                            status.signer_cert.sha256_fingerprint
                        )),
                        theme::typography::caption(
                            "Certificate chain: embedded, not validated (§20.3)"
                        ),
                        theme::typography::caption(format!("Coverage: {:?}", status.coverage)),
                        theme::typography::caption(format!(
                            "Signing time (claimed, not attested): {}",
                            status
                                .claimed_time
                                .map(|t| t.to_string())
                                .unwrap_or_else(|| "not stated".to_string())
                        )),
                        theme::typography::caption(format!(
                            "Digest: {}; signature: {}",
                            status.digest_algorithm, status.signature_algorithm
                        )),
                    ]
                    .spacing(gap::XS)
                    .padding(gap::S)
                    // Algorithm findings are the one thing here that is a
                    // judgement rather than a fact, so they are said in the
                    // alert colour and only when there are any. pulpit does no
                    // certificate path validation and so refuses nothing on
                    // this basis: a weak algorithm makes a signature worth
                    // less, not invalid, and saying so is the whole of what
                    // this crate can honestly offer the reader.
                    .extend(
                        crate::signing::signature_notes(status)
                            .into_iter()
                            .map(|note| {
                                theme::typography::caption(note)
                                    .color(theme::ambient::alert())
                                    .into()
                            }),
                    ),
                );
            }
            row_body = row_body.push(
                row![
                    button(theme::typography::label("Copy fingerprint"))
                        .padding(gap::XS)
                        .style(theme::ambient::tool_button)
                        .on_press(Message::CopySignatureFingerprint(index)),
                    button(theme::typography::label("Copy report"))
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

    // The answer given when the document was opened, and the way to change
    // it. The offer itself is shown once and never again, so without this the
    // refusal a drawing tool gives names a control that is no longer on
    // screen — and the reader has no way back but closing and reopening the
    // file. Offered in one direction only: turning editing back off would
    // not un-edit anything, so it would promise a safety it cannot deliver.
    if app
        .append_only
        .is_some_and(crate::signing::AppendOnlyMode::blocks_mutation)
    {
        body = body.push(rule());
        body = body.push(
            column![
                theme::typography::caption(crate::signing::APPEND_ONLY_CHOICE_DETAIL),
                button(theme::typography::label(crate::signing::EDIT_ANYWAY_CHOICE))
                    .padding(gap::S)
                    .style(theme::ambient::alert_button)
                    .on_press(Message::EditAnyway),
                theme::typography::caption(crate::signing::EDIT_ANYWAY_CHOICE_DETAIL),
            ]
            .spacing(gap::XS),
        );
    }

    // §31.2, verbatim, and §31.3 when it applies. The Sign flow used to
    // carry both on its confirmation dialog; with that dialog gone this
    // panel is where the claim is made, and it is the one place that
    // outlives the notice raised when a signature is written.
    body = body.push(theme::typography::caption(
        crate::signing::IDENTITY_DISCLOSURE,
    ));
    if app.document_signatures.len() > 1 {
        body = body.push(theme::typography::caption(
            crate::signing::COUNTERSIGN_DISCLOSURE,
        ));
    }

    panel(body, Some(Message::ToggleSignaturePanel))
}

/// §31.3, A9: the append-only offer, shown the moment a document that
/// already carries a signature is opened, before anything can mutate it.
fn append_only_offer_dialog() -> Element<'static, Message> {
    use crate::signing::{
        APPEND_ONLY_CHOICE, APPEND_ONLY_CHOICE_DETAIL, EDIT_ANYWAY_CHOICE,
        EDIT_ANYWAY_CHOICE_DETAIL,
    };

    // Each answer carries what it costs, under the button that takes it.
    // Two bare labels side by side made the reader guess which one was the
    // careful choice, and guessing wrong is permanent for the copy they save.
    let choice =
        |label: &'static str,
         detail: &'static str,
         style: fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style,
         message: Message| {
            column![
                button(theme::typography::label(label))
                    .padding(gap::S)
                    .width(Length::Fill)
                    .style(style)
                    .on_press(message),
                theme::typography::caption(detail),
            ]
            .spacing(gap::XS)
        };

    let body = column![
        // The title says the situation; the two answers below say what
        // follows from it. A paragraph between them would only be the same
        // thing again, in the place a reader is least likely to read it.
        theme::typography::title("This document is already signed"),
        choice(
            APPEND_ONLY_CHOICE,
            APPEND_ONLY_CHOICE_DETAIL,
            theme::ambient::selected_button,
            Message::AcceptAppendOnly,
        ),
        choice(
            EDIT_ANYWAY_CHOICE,
            EDIT_ANYWAY_CHOICE_DETAIL,
            theme::ambient::alert_button,
            Message::EditAnyway,
        ),
    ]
    .spacing(gap::M);

    // No way out but an answer: mutating a signed document by accident is
    // exactly what this dialog exists to prevent.
    panel(body, None)
}

/// How long §31.1 step 3's write may take before the panel over it says what
/// it is waiting for. Below this the wait is not a state anyone perceives,
/// and naming it costs more attention than it saves.
const SAVING_FIRST_EXPLAIN_AFTER: std::time::Duration = std::time::Duration::from_millis(250);

/// Whatever the Sign flow still has to ask, which in the common case is
/// nothing (SPEC-signing.md §31.1).
///
/// Three of the four steps draw a small panel; [`SigningFlow::Signing`] draws
/// none, because the platform's own save dialog is on screen during it and
/// the write that follows is short. What signing produced is reported in the
/// corner afterwards (`crate::signing::signed_notice`), not here.
fn sign_dialog<'a>(app: &'a App, flow: &'a crate::signing::SigningFlow) -> Element<'a, Message> {
    use crate::signing::{SignMsg, SigningFlow};

    let cancel = || {
        button(theme::typography::label("Cancel"))
            .padding(gap::S)
            .style(theme::ambient::tool_button)
            .on_press(Message::Sign(SignMsg::Cancel))
    };

    match flow {
        SigningFlow::SavingFirst => {
            // Blocked from the first millisecond, explained only once there
            // is something to explain. Nothing may edit the document while
            // the bytes the signature is made from are being written — a
            // stroke drawn now would be missing from the signed copy with
            // nothing saying so — but that write is usually over before a
            // sheet could be read, and a modal that flashes reads as a
            // glitch. So the ground goes down at once and stays invisible
            // until the wait is long enough to be worth a word.
            let waited = app
                .signing_saving_since
                .map(|since| app.now.saturating_duration_since(since))
                .unwrap_or_default();
            if waited < SAVING_FIRST_EXPLAIN_AFTER {
                return opaque(
                    container(space::vertical())
                        .width(Length::Fill)
                        .height(Length::Fill),
                );
            }
            let mut body = column![
                theme::typography::title("Sign"),
                // No file name in this: the copy being written is scratch,
                // deleted as soon as the signature has been made from it, and
                // naming it would invite the reader to look for it.
                theme::typography::body("Preparing your edits to be signed…"),
            ]
            .spacing(gap::M);
            if !app.document_signatures.is_empty() {
                // This goes through a full rewrite, not an append (§28.4), so
                // it does not carry the document's existing signatures
                // forward — and the signed copy is made from it.
                body = body.push(theme::typography::caption(
                    "Writing the edits rewrites the document, so its existing signatures will \
                         not carry over into the signed copy.",
                ));
            }
            body = body.push(cancel());
            panel(body, Some(Message::Sign(SignMsg::Cancel)))
        }
        SigningFlow::Unlock {
            profile_id,
            passphrase,
            error,
            busy,
        } => {
            let profiles = &app.settings.signatures.profiles;
            let selected = app.settings.signatures.profile(profile_id);
            let name = selected
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| profile_id.clone());
            // Unlocked already: this step is only up because there is more
            // than one profile to choose between, so there is no passphrase
            // to ask for and Continue is the whole interaction.
            let locked = !app.is_profile_unlocked(profile_id);

            let mut body = column![theme::typography::title("Sign")].spacing(gap::M);

            // The profile row earns its place only when there is a choice.
            // With one profile this step is never reached unlocked, and the
            // row would be a single button that changes nothing.
            if profiles.len() > 1 {
                let mut choices = Row::new().spacing(gap::S);
                for profile in profiles {
                    choices = choices.push(
                        selectable(
                            button(theme::typography::label(profile.name.clone())),
                            profile.id == *profile_id,
                        )
                        .on_press_maybe(
                            (!*busy)
                                .then(|| Message::Sign(SignMsg::ProfileChosen(profile.id.clone()))),
                        ),
                    );
                }
                body = body.push(dialog_section("Sign with", choices.wrap()));
            } else {
                body = body.push(theme::typography::body(format!("Signing with {name}.")));
            }

            if locked {
                body =
                    body.push(dialog_section(
                        "Passphrase",
                        text_input("Passphrase", passphrase)
                            .secure(true)
                            .on_input_maybe((!*busy).then_some(|typed| {
                                Message::Sign(SignMsg::PassphraseChanged(typed))
                            }))
                            .on_submit(Message::Sign(SignMsg::PassphraseSubmit))
                            .style(theme::ambient::text_field)
                            .padding(gap::S),
                    ));
            }
            if let Some(error) = error {
                body = body
                    .push(theme::typography::caption(error.clone()).color(theme::ambient::alert()));
            }
            // Named before it happens: the next thing on screen is a save
            // dialog, and nothing is written until it is answered.
            body = body.push(theme::typography::caption(
                "Next: choose where to save the signed copy. The document itself is not \
                 changed.",
            ));
            body = body.push(
                row![
                    cancel(),
                    button(theme::typography::label(if *busy {
                        "Reading the credential…"
                    } else {
                        "Continue"
                    }))
                    .padding(gap::S)
                    .style(theme::ambient::selected_button)
                    .on_press_maybe((!*busy).then_some(Message::Sign(SignMsg::PassphraseSubmit))),
                ]
                .spacing(gap::S),
            );
            panel(body, (!*busy).then_some(Message::Sign(SignMsg::Cancel)))
        }
        // §33's last paragraph: an expired or not-yet-valid certificate stops
        // the flow until it is overridden on purpose. Collapsing the dialogs
        // did not collapse this gate — it is the one thing left that is worth
        // stopping for.
        SigningFlow::ConfirmValidity { info, .. } => {
            let summary = &info.summary;
            let warning = if info.expired {
                "This certificate has expired."
            } else {
                "This certificate is not yet valid."
            };
            let body = column![
                theme::typography::title("Sign"),
                theme::typography::body(warning).color(theme::ambient::alert()),
                theme::typography::body(crate::signing::subject_common_name(&summary.subject)),
                theme::typography::caption(format!(
                    "Valid {} — {}",
                    summary.not_before, summary.not_after
                )),
                theme::typography::caption(format!("SHA-256 {}", summary.sha256_fingerprint)),
                theme::typography::caption(crate::signing::IDENTITY_DISCLOSURE),
                row![
                    cancel(),
                    button(theme::typography::label("Sign anyway"))
                        .padding(gap::S)
                        .style(theme::ambient::alert_button)
                        .on_press(Message::Sign(SignMsg::OverrideValidity)),
                ]
                .spacing(gap::S),
            ]
            .spacing(gap::M);
            // Non-dismissable except its own buttons: it is a warning being
            // answered, and a press on the ground behind it would read as
            // either answer.
            panel(body, None)
        }
        // The save picker is the platform's own window, and the write after
        // it is over in well under a second. A modal here would flash.
        SigningFlow::Signing { .. } => blank(),
    }
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
        theme::typography::title("Put back the unsaved edits?"),
        theme::typography::body(
            "Pulpit did not shut down cleanly, and this document had edits that had \
              not been saved to a copy."
        ),
        theme::typography::body(journal.summary()),
        theme::typography::note(
            "They are applied to the document as it is now. If you already saved a \
              copy with them, start fresh."
        ),
        row![
            button(theme::typography::label("Start fresh"))
                .padding(gap::S)
                .style(theme::ambient::tool_button)
                .on_press(Message::DiscardReaderEdits),
            button(theme::typography::label("Put them back"))
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
    column![theme::typography::heading(title), content,]
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
    let mut buttons = row![theme::typography::field("Notes mapping:")].spacing(gap::XS);
    for (label, mapping) in options {
        let selected = app.state.mapping() == &mapping;
        buttons = buttons.push(
            selectable(button(theme::typography::label(label)), selected)
                .on_press(Message::SetMapping(mapping)),
        );
    }
    let current = app.state.mapping();
    if let NotesMapping::SplitPage { slide, .. } = current {
        let notes_side = if slide.x > 0.0 { "left" } else { "right" };
        buttons = buttons.push(theme::typography::caption(format!(
            "split page, notes {notes_side}"
        )));
        buttons = buttons.push(
            button(theme::typography::label("Swap halves"))
                .on_press(Message::SetMapping(current.swapped())),
        );
    }
    buttons.wrap().into()
}

// ----------------------------------------------------------- layout pages

fn library_page(app: &App) -> Element<'_, Message> {
    let page = designer_view::library(&app.layouts, Some(&app.active_layout.id));
    stack![
        page,
        match &app.layout_dialog {
            Some(dialog) => layout_dialog(dialog),
            None => blank(),
        },
    ]
    .into()
}

fn layout_dialog(dialog: &LayoutDialog) -> Element<'_, Message> {
    let body: Element<'_, Message> = match dialog {
        LayoutDialog::ConfirmDelete { name, .. } => column![
            theme::typography::title(format!("Delete “{name}”?")),
            theme::typography::note("This cannot be undone."),
            row![
                button(theme::typography::label("Delete"))
                    .padding(gap::S)
                    .style(theme::ambient::alert_button)
                    .on_press(Message::ConfirmLayoutDialog),
                button(theme::typography::label("Cancel"))
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
    designer_view::editor(designer, &context, app.compact_editor())
}

/// Seconds since local midnight, for the clock widget.
pub fn seconds_of_day() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    ((now as i64 + local_offset_seconds()).rem_euclid(86_400)) as u32
}

/// Offset from UTC in seconds, primed once from `date +%z`. Falls back to
/// UTC until then, which is honest rather than wrong by an unknown amount.
fn local_offset_seconds() -> i64 {
    OFFSET.get().copied().unwrap_or(0)
}

static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();

/// Spawn `date +%z` and cache what it says. Called from a startup helper
/// thread, deliberately not from the first clock widget to draw: priming is
/// a `PATH` walk and a subprocess, which is nothing the first frame — or the
/// event loop at all — should be paying for.
pub fn prime_local_offset() {
    let _ = OFFSET.get_or_init(|| {
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_menu_keeps_the_five_newest_paths_in_order() {
        let recent = (0..7)
            .map(|index| std::path::PathBuf::from(format!("deck-{index}.pdf")))
            .collect();
        let shown: Vec<_> = recent_menu_documents(&recent).collect();
        assert_eq!(shown.len(), 5);
        assert_eq!(
            shown.first().copied(),
            Some(std::path::Path::new("deck-0.pdf"))
        );
        assert_eq!(
            shown.last().copied(),
            Some(std::path::Path::new("deck-4.pdf"))
        );
    }

    #[test]
    fn recent_menu_labels_show_only_the_filename() {
        let path = std::path::Path::new("/talks/quarterly-presentation.pdf");
        assert_eq!(recent_menu_label(path), "quarterly-presentation.pdf");
    }

    #[test]
    fn every_shortcut_remains_its_own_keycap_and_alternates_stand_beside_the_others() {
        let labels = shortcut_labels(
            vec!["\u{2192}".into(), "PgDn".into()],
            vec!["J".into(), "Space".into()],
        );
        assert_eq!(labels, ["\u{2192}", "PgDn", "J", "Space"]);
    }

    #[test]
    fn the_reference_splits_into_two_balanced_tables_only_when_they_have_room() {
        use crate::settings::keys::SHORTCUT_GROUPS;

        assert!(split_shortcut_tables(1_200.0));
        assert!(!split_shortcut_tables(1_000.0));

        let actions = |groups: &[usize]| {
            groups
                .iter()
                .map(|index| SHORTCUT_GROUPS[*index].actions.len())
                .sum::<usize>()
        };

        // Every group goes to exactly one side, and none is left out.
        let mut placed: Vec<usize> = SHORTCUT_TABLE_LEFT
            .iter()
            .chain(SHORTCUT_TABLE_RIGHT)
            .copied()
            .collect();
        placed.sort_unstable();
        assert_eq!(placed, (0..SHORTCUT_GROUPS.len()).collect::<Vec<_>>());

        // A group is indivisible, so perfect halves are not always reachable.
        // The standing invariant is that the chosen split is as even as any
        // split of these groups can be — which keeps the constants honest as
        // actions come and go without pretending a group can be cut in half.
        let total: usize = SHORTCUT_GROUPS.iter().map(|g| g.actions.len()).sum();
        let best = (0..1u32 << SHORTCUT_GROUPS.len())
            .map(|mask| {
                let left: usize = (0..SHORTCUT_GROUPS.len())
                    .filter(|index| mask & (1 << index) != 0)
                    .map(|index| SHORTCUT_GROUPS[index].actions.len())
                    .sum();
                left.abs_diff(total - left)
            })
            .min()
            .expect("there is at least one split");
        assert_eq!(
            actions(SHORTCUT_TABLE_LEFT).abs_diff(actions(SHORTCUT_TABLE_RIGHT)),
            best,
            "the two tables should be as balanced as whole groups allow"
        );
    }

    #[test]
    fn fullscreen_menu_item_names_the_action_not_the_current_state() {
        assert_eq!(fullscreen_action_label(true, false, false), "Fullscreen");
        assert_eq!(fullscreen_action_label(true, true, false), "Windowed");
        assert_eq!(fullscreen_action_label(false, false, false), "Fullscreen");
        assert_eq!(fullscreen_action_label(false, false, true), "Windowed");
    }

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
