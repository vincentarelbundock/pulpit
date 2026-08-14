//! A small controlled popover anchored above a widget.
//!
//! Iced's tooltip already solves the positioning problem, but its open state
//! is hover-driven. Annotation options are opened by a click or context click,
//! so this wrapper exposes the same kind of overlay with application-owned
//! state and forwards events to the interactive controls inside it.

use iced::advanced::widget::{self, Widget};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

pub struct Popover<'a, Message> {
    content: Element<'a, Message>,
    /// `Some` only while open. The panel is a dozen-plus widgets, and every
    /// closed popover used to build, tree and diff its panel on every view
    /// pass anyway; a closed popover now carries nothing but its trigger.
    popup: Option<Element<'a, Message>>,
    gap: f32,
}

impl<'a, Message> Popover<'a, Message> {
    pub fn new(
        content: impl Into<Element<'a, Message>>,
        popup: Option<Element<'a, Message>>,
    ) -> Self {
        Self {
            content: content.into(),
            popup,
            gap: 6.0,
        }
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Popover<'_, Message> {
    fn children(&self) -> Vec<widget::Tree> {
        match &self.popup {
            Some(popup) => vec![widget::Tree::new(&self.content), widget::Tree::new(popup)],
            None => vec![widget::Tree::new(&self.content)],
        }
    }

    fn diff(&self, tree: &mut widget::Tree) {
        match &self.popup {
            Some(popup) => {
                tree.diff_children(&[self.content.as_widget(), popup.as_widget()]);
            }
            None => tree.diff_children(&[self.content.as_widget()]),
        }
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

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

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
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
        let mut children = tree.children.iter_mut();
        let content = self.content.as_widget_mut().overlay(
            children.next().expect("content state"),
            layout,
            renderer,
            viewport,
            translation,
        );
        let popup = self.popup.as_mut().map(|popup| {
            overlay::Element::new(Box::new(PopupOverlay {
                anchor: layout.bounds() + translation,
                popup,
                tree: children.next().expect("popup state"),
                gap: self.gap,
            }))
        });

        if content.is_some() || popup.is_some() {
            Some(
                overlay::Group::with_children(content.into_iter().chain(popup).collect()).overlay(),
            )
        } else {
            None
        }
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                operation,
            );
        });
    }
}

impl<'a, Message: 'a> From<Popover<'a, Message>> for Element<'a, Message> {
    fn from(popover: Popover<'a, Message>) -> Self {
        Element::new(popover)
    }
}

struct PopupOverlay<'a, 'b, Message> {
    anchor: Rectangle,
    popup: &'b mut Element<'a, Message>,
    tree: &'b mut widget::Tree,
    gap: f32,
}

impl<Message> overlay::Overlay<Message, iced::Theme, iced::Renderer>
    for PopupOverlay<'_, '_, Message>
{
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let popup = self.popup.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );
        let popup_bounds = popup.bounds();
        let mut x = self.anchor.center_x() - popup_bounds.width / 2.0;
        let mut y = self.anchor.y - popup_bounds.height - self.gap;
        x = x.clamp(0.0, (bounds.width - popup_bounds.width).max(0.0));
        if y < 0.0 {
            y = (self.anchor.y + self.anchor.height + self.gap)
                .min((bounds.height - popup_bounds.height).max(0.0));
        }
        popup.translate(Vector::new(x, y))
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.popup.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &Rectangle::with_size(Size::INFINITE),
        );
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self.popup.as_widget_mut().update(
            self.tree,
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.popup.as_widget().mouse_interaction(
            self.tree,
            layout,
            cursor,
            &layout.bounds(),
            renderer,
        )
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.popup
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }
}
