//! Drawing the search pane.
//!
//! One pane, whatever is behind it. It asks the model what to say and never
//! looks at a document, a deck or a worker: slides and reader put the same
//! [`pulpit_core::search::SearchState`] in front of it, which is what makes
//! "search in every view" one widget rather than two.

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Length};

use crate::theme;
use crate::widgets::context::{Mode, SearchData};
use crate::widgets::event::FindCommand;
use crate::widgets::{Widget, WidgetEvent};

use super::model::{row_label, row_parts, summary};

/// The search box's input, so a key binding can put the caret in it. One
/// search at a time, so one id.
pub fn input_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("pulpit-search-query")
}

pub fn view<Message: Clone + 'static>(
    _widget: &Widget,
    search: &SearchData<'_>,
    mode: Mode,
    on_event: fn(WidgetEvent) -> Message,
) -> Element<'static, Message> {
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

    let controls = row![
        field,
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

    let mut pane = column![controls].spacing(theme::space::XS);
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
        let body = row![
            text(row_label(hit))
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::muted())
                .width(Length::Fixed(72.0)),
            // The match itself is the accent; its surroundings are context and
            // are drawn as such, so a list of twenty rows can be scanned for
            // the one whose sentence is the right one.
            text(before)
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::muted()),
            text(matched)
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::accent()),
            text(after)
                .size(theme::type_scale::LABEL)
                .color(theme::ambient::muted()),
        ]
        .spacing(theme::space::XS)
        .align_y(Alignment::Center);

        let mut control = button(body)
            .padding(theme::space::XS)
            .style(if Some(index) == current {
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
    scrollable(rows)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
