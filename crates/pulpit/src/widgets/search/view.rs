//! Drawing the search pane.
//!
//! One pane, whatever is behind it. It asks the model what to say and never
//! looks at a document, a deck or a worker: slides and reader put the same
//! [`pulpit_core::search::SearchState`] in front of it, which is what makes
//! "search in every view" one widget rather than two.

use iced::widget::{button, column, container, rich_text, row, space, span, text, text_input};
use iced::{Alignment, Element, Length};

use crate::theme;
use crate::widgets::context::SearchData;
use crate::widgets::event::FindCommand;
use crate::widgets::view_context::WidgetViewContext;
use crate::widgets::{Widget, WidgetEvent};

use super::model::{row_label, row_parts, summary};

/// The search box's input, so a key binding can put the caret in it. One
/// search at a time, so one id.
pub fn input_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-search-query")
}

pub fn view<'ctx, 'a, Message: Clone + 'static>(
    ctx: &WidgetViewContext<'ctx, 'a, Message>,
    _widget: &Widget,
) -> Element<'a, Message> {
    let search = &ctx.context.search;
    let mode = ctx.context.mode;
    let on_event = ctx.on_event;
    let live = mode.interactive();
    let state = search.state;

    let mut field = text_input("Find in document", state.query().text())
        .id(input_id())
        .size(theme::type_scale::BODY)
        .width(Length::Fill);
    if live {
        field = field
            .on_input(move |typed| on_event(WidgetEvent::Find(FindCommand::Type(typed))))
            // Enter means "the next one", not "search again": the scan is
            // already running by the time the key is pressed.
            .on_submit(on_event(WidgetEvent::Find(FindCommand::Next)));
    }

    let step = |label: &'static str, command: FindCommand| {
        let mut control = button(text(label).size(theme::type_scale::LABEL))
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

    let mut clear = button(text("✕").size(theme::type_scale::LABEL))
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
        step("‹", FindCommand::Previous),
        step("›", FindCommand::Next),
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

    let mut pane = column![
        text("Search").size(theme::type_scale::TITLE),
        field,
        controls
    ]
    .spacing(theme::space::XS);
    if !said.is_empty() {
        pane = pane.push(line);
    }
    pane = pane.push(results(search, live, on_event));

    container(pane)
        .padding(theme::space::S)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// One row per hit, in document order, with the current one marked.
fn results<Message: Clone + 'static>(
    search: &SearchData<'_>,
    live: bool,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
    let state = search.state;
    let current = state.position().map(|at| at - 1);
    let mut rows = column![].spacing(2);
    for (index, hit) in state.hits().iter().enumerate() {
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

        let focused = search.keyboard_focus && Some(index) == current;
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
            .style(if focused {
                theme::ambient::selected_button
            } else {
                theme::ambient::tool_button
            });
        if live {
            control = control.on_press(on_event(WidgetEvent::Find(FindCommand::Focus(index))));
        }
        rows = rows.push(control);
    }
    if state.hits().is_empty() {
        return space::vertical().height(Length::Fill).into();
    }
    crate::widgets::scroll::vertical(rows)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
