//! Drawing the search pane.
//!
//! One pane, whatever is behind it. It asks the model what to say and never
//! looks at a document, a deck or a worker: slides and reader put the same
//! [`pulpit_core::search::SearchState`] in front of it, which is what makes
//! "search in every view" one widget rather than two.

use iced::keyboard::{key::Named, Key};
use iced::widget::{button, container, responsive, rich_text, row, space, span, text, Column};
use iced::{Alignment, Element, Length};

use crate::theme;
use crate::theme::Icon;
use crate::widgets::context::SearchData;
use crate::widgets::event::FindCommand;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetEvent};

use super::model::{row_label, row_parts, summary};

pub const RESULT_ROW_HEIGHT: f32 = 40.0;

/// The search box's input, so a key binding can put the caret in it. One
/// search at a time, so one id.
pub fn input_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-search-query")
}

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    widget: &Widget,
) -> Element<'a, Message> {
    let search = ctx.context.search.clone();
    pane(
        search,
        ctx.context.mode.interactive(),
        widget.kind() == crate::widgets::WidgetKind::DocumentOutline,
        ctx.on_event,
    )
}

pub fn pane<Message: Clone + 'static>(
    search: SearchData<'_>,
    live: bool,
    shares_outline_sidebar: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let state = search.state;
    let input_focus = search.input_focus;
    let results_focus = search.results_focus;
    let document_focus = !input_focus && !results_focus;

    let mut field = iced::widget::TextInput::new("Find in document", state.query().text())
        .id(input_id())
        .size(theme::type_scale::BODY)
        .padding(theme::space::S)
        .style(theme::ambient::text_field)
        .width(Length::Fill);
    if live {
        field = field
            .on_input(move |typed| on_event(WidgetEvent::Find(FindCommand::Type(typed))))
            // Enter means "the next one", not "search again": the scan is
            // already running by the time the key is pressed.
            .on_submit(on_event(WidgetEvent::Find(FindCommand::Next)));
    }

    let step = |glyph: Icon, command: FindCommand| {
        let mut control = button(theme::icon::icon(glyph, theme::type_scale::BODY))
            .padding(theme::space::XS)
            .style(theme::ambient::tool_button);
        // Nothing found is nothing to step through, and a dead button says
        // that better than one that does nothing when pressed.
        if live && !state.hits().is_empty() {
            control = control.on_press(on_event(WidgetEvent::Find(command)));
        }
        control
    };

    let toggle = |label: &'static str, on: bool, command: FindCommand| {
        let mut control = button(text(label).size(theme::type_scale::LABEL))
            .padding(theme::space::XS)
            .style(if on {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            });
        if live {
            control = control.on_press(on_event(WidgetEvent::Find(command)));
        }
        control
    };

    let mut clear = button(theme::icon::icon(Icon::Close, theme::type_scale::BODY))
        .padding(theme::space::XS)
        .style(theme::ambient::tool_button);
    // Nothing typed is nothing to clear, and the overlays on the page go with
    // the query rather than needing a second control of their own.
    if live && !state.query().is_empty() {
        clear = clear.on_press(on_event(WidgetEvent::Find(FindCommand::Clear)));
    }

    // The query gets the whole rail width. The compact tool row below it is
    // easier to scan and does not squeeze the input as options are added.
    let controls = row![
        step(Icon::ChevronLeft, FindCommand::Previous),
        step(Icon::ChevronRight, FindCommand::Next),
        toggle(
            "Aa",
            state.query().case_sensitive,
            FindCommand::ToggleCaseSensitive
        ),
        toggle(
            "|ab|",
            state.query().whole_word,
            FindCommand::ToggleWholeWord
        ),
        toggle(".*", state.query().regex, FindCommand::ToggleRegex),
        clear,
    ]
    .spacing(theme::space::XS)
    .align_y(Alignment::Center);

    let said = summary(state);
    let line =
        text(said.clone())
            .size(theme::type_scale::LABEL)
            .color(if state.problem().is_some() {
                theme::ambient::alert()
            } else {
                theme::ambient::muted()
            });

    let mut pane = Column::new().spacing(theme::space::XS);
    if shares_outline_sidebar {
        pane = pane.push(crate::widgets::document::view::sidebar_tabs(
            true, live, on_event,
        ));
    }
    pane = pane
        .push(text("Search").size(theme::type_scale::TITLE))
        .push(field)
        .push(controls);
    if !said.is_empty() {
        pane = pane.push(line);
    }
    pane = pane.push(results(search, live, on_event));

    let panel: Element<'static, Message> = container(pane)
        .padding(theme::space::S)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();

    crate::widgets::panel::on_key(panel, move |key, modifiers| {
        if !live || modifiers.control() || modifiers.alt() || modifiers.logo() {
            return None;
        }
        let panel_event = |command| Some(on_event(WidgetEvent::Panel(command)));
        if document_focus {
            return matches!(key, Key::Named(Named::Tab)).then(|| {
                on_event(WidgetEvent::Panel(
                    crate::widgets::event::PanelCommand::FocusSidebar,
                ))
            });
        }
        if input_focus {
            return match key {
                Key::Named(Named::ArrowDown) | Key::Named(Named::Enter) => {
                    Some(on_event(WidgetEvent::Find(FindCommand::Next)))
                }
                Key::Named(Named::ArrowUp) => {
                    Some(on_event(WidgetEvent::Find(FindCommand::Previous)))
                }
                Key::Named(Named::Escape) | Key::Named(Named::Tab) => {
                    panel_event(crate::widgets::event::PanelCommand::FocusDocument)
                }
                _ => None,
            };
        }
        if results_focus {
            return match key {
                Key::Named(Named::ArrowDown) => {
                    Some(on_event(WidgetEvent::Find(FindCommand::Next)))
                }
                Key::Named(Named::ArrowUp) => {
                    Some(on_event(WidgetEvent::Find(FindCommand::Previous)))
                }
                Key::Named(Named::Enter) => {
                    Some(on_event(WidgetEvent::Find(FindCommand::ActivateCurrent)))
                }
                Key::Named(Named::Escape) | Key::Named(Named::Tab) => {
                    panel_event(crate::widgets::event::PanelCommand::FocusDocument)
                }
                Key::Character(value) if value.eq_ignore_ascii_case("n") => {
                    Some(on_event(WidgetEvent::Find(if modifiers.shift() {
                        FindCommand::Previous
                    } else {
                        FindCommand::Next
                    })))
                }
                _ => None,
            };
        }
        None
    })
}

/// One row per hit, in document order, with the current one marked.
fn results<Message: Clone + 'static>(
    search: SearchData<'_>,
    live: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let hits = search.state.hits().to_vec();
    let current = search.state.current().map(pulpit_core::search::Hit::key);
    if hits.is_empty() {
        return space::vertical().height(Length::Fill).into();
    }
    let scroll = search.scroll;
    let keyboard_focus = search.results_focus;
    let measured_viewport = search.viewport.clone();
    responsive(move |size| {
        measured_viewport.set(size.height);
        let window = crate::widgets::scroll::virtual_window(
            hits.len(),
            RESULT_ROW_HEIGHT,
            scroll,
            size.height,
        );
        let mut rows = Column::new();
        if window.before > 0.0 {
            rows = rows.push(space::vertical().height(window.before));
        }
        for hit in &hits[window.rows.clone()] {
            let (before, matched, after) = row_parts(hit);
            let mut highlight = theme::ambient::accent();
            highlight.a = 0.18;
            // These are spans in one text flow, not adjacent layout widgets: the
            // page number stays beside the excerpt and the whole line wraps as a
            // sentence when the panel is narrow.
            let body = rich_text::<(), Message, iced::Theme, iced::Renderer>([
                span(row_label(hit)).color(theme::ambient::muted()),
                span("  "),
                span(before).color(theme::ambient::muted()),
                span(matched)
                    .color(theme::ambient::text())
                    .background(highlight)
                    .padding([0.0, 2.0]),
                span(after).color(theme::ambient::muted()),
            ])
            .size(theme::type_scale::LABEL)
            .width(Length::Fill);

            let focused = keyboard_focus && Some(hit.key()) == current;
            let marker: Element<'static, Message> = if focused {
                container(space::vertical().width(3.0).height(Length::Fill))
                    .style(theme::ambient::accent_rule)
                    .into()
            } else {
                space::horizontal().width(3.0).into()
            };
            let mut control = button(row![marker, body].spacing(theme::space::XS))
                .padding(theme::space::XS)
                .width(Length::Fill)
                .height(Length::Fixed(RESULT_ROW_HEIGHT))
                .style(if focused {
                    theme::ambient::focus_button
                } else {
                    theme::ambient::tool_button
                });
            if live {
                control =
                    control.on_press(on_event(WidgetEvent::Find(FindCommand::Focus(hit.key()))));
            }
            rows = rows.push(control);
        }
        if window.after > 0.0 {
            rows = rows.push(space::vertical().height(window.after));
        }
        crate::widgets::scroll::vertical(rows)
            .id(results_id())
            .width(Length::Fill)
            .height(Length::Fill)
            .on_scroll(move |viewport| {
                on_event(WidgetEvent::Find(FindCommand::Scrolled {
                    offset: viewport.absolute_offset().y.max(0.0).round() as u32,
                    viewport: viewport.bounds().height.max(0.0).round() as u32,
                }))
            })
            .into()
    })
    .into()
}

pub fn results_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-search-results")
}
