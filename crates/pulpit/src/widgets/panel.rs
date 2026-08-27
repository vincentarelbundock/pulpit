//! Shared mechanics for keyboard-owned side panels.
//!
//! Panels decide which keys mean something inside them and this wrapper
//! captures only those presses. Application-wide shortcuts therefore see
//! precisely the keys the mounted panel declined, rather than reconstructing
//! widget focus from a global event subscription.

use iced::advanced::widget::{self, Widget};

use crate::widgets::forward_to_child;
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::keyboard::{Key, Modifiers};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

pub fn on_key<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    handler: impl Fn(&Key, Modifiers) -> Option<Message> + 'a,
) -> Element<'a, Message> {
    Element::new(KeyScope {
        content: content.into(),
        handler: Box::new(handler),
    })
}

type KeyHandler<'a, Message> = dyn Fn(&Key, Modifiers) -> Option<Message> + 'a;

struct KeyScope<'a, Message> {
    content: Element<'a, Message>,
    handler: Box<KeyHandler<'a, Message>>,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for KeyScope<'_, Message> {
    forward_to_child!(content: children, diff, size, size_hint, layout, mouse_interaction, operate);

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) = event {
            if let Some(message) = (self.handler)(key, *modifiers) {
                shell.publish(message);
                shell.capture_event();
                return;
            }
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut widget::Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
