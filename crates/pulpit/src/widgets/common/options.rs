//! One tool options panel, wherever it is opened from.
//!
//! The Reader's toolbar and the presenter's annotation palette both hang a
//! panel off a tool button, and both answer the same shape of question: what
//! colour, how big, which mode. What each *offers* differs — the palette has
//! the pointer's dot-or-spotlight, the Reader has type size — but the frame
//! around it should not, or the same panel reads as two panels depending on
//! which toolbar reached it.
//!
//! So the chrome lives here: the title that names the tool, the close button,
//! the caption above each control, the spacing between them and the surface
//! they sit on. A caller supplies rows; the order they are pushed in is the
//! order they appear, and colour comes first because it is what a hand
//! reaches for most often.

use iced::widget::{button, column, container, row, space, text, Column};
use iced::{Alignment, Element, Length};

use crate::theme;

/// A tool's options, assembled row by row.
pub struct Options<Message> {
    title: String,
    on_close: Option<Message>,
    width: Option<f32>,
    rows: Vec<(Option<String>, Element<'static, Message>)>,
}

impl<Message: Clone + 'static> Options<Message> {
    /// A panel headed by the tool it belongs to.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            on_close: None,
            width: None,
            rows: Vec::new(),
        }
    }

    /// The close button in the header. `None` draws it inert rather than
    /// dropping it, so a panel does not change shape with the mode.
    pub fn on_close(mut self, message: Option<Message>) -> Self {
        self.on_close = message;
        self
    }

    /// A fixed width, for a toolbar whose panels should not resize as their
    /// contents change.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// One control, under the caption that names it. The caption carries the
    /// value where a toolbar shows one, so it is a string rather than a name.
    pub fn row(
        mut self,
        label: impl Into<String>,
        control: impl Into<Element<'static, Message>>,
    ) -> Self {
        self.rows.push((Some(label.into()), control.into()));
        self
    }

    /// One control that says what it is without a caption.
    pub fn bare_row(mut self, control: impl Into<Element<'static, Message>>) -> Self {
        self.rows.push((None, control.into()));
        self
    }
}

impl<Message: Clone + 'static> From<Options<Message>> for Element<'static, Message> {
    fn from(options: Options<Message>) -> Self {
        let header = row![
            text(options.title).size(theme::type_scale::LABEL),
            space::horizontal().width(Length::Fill),
            button(theme::icon::icon(
                theme::Icon::Close,
                theme::type_scale::BODY
            ))
            .padding(2)
            .style(theme::ambient::tool_button)
            .on_press_maybe(options.on_close),
        ]
        .align_y(Alignment::Center);

        let mut panel: Column<'static, Message> = column![header].spacing(theme::space::S);
        for (label, control) in options.rows {
            if let Some(label) = label {
                panel = panel.push(text(label).size(theme::type_scale::CAPTION));
            }
            panel = panel.push(control);
        }

        let panel = container(panel)
            .padding(theme::space::M)
            .style(theme::ambient::surface);
        match options.width {
            Some(width) => panel.width(Length::Fixed(width)),
            None => panel,
        }
        .into()
    }
}
