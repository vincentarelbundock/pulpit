//! A small controlled popover anchored above a widget.
//!
//! Iced's tooltip already solves the positioning problem, but its open state
//! is hover-driven. Tool options are opened by a click or context click, so
//! this wrapper exposes the same kind of overlay with application-owned state
//! and forwards events to the interactive controls inside it.
//!
//! It lives here rather than beside the presenter palette because the Reader's
//! toolbar hangs the same panels off the same kind of button: one popover, so
//! a panel behaves the same wherever it is opened.

use iced::advanced::widget::{self, Widget};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

pub struct Popover<'a, Message> {
    content: Element<'a, Message>,
    /// `Some` only while open. The panel is a dozen-plus widgets, and every
    /// closed popover used to build, tree and diff its panel on every view
    /// pass anyway; a closed popover now carries nothing but its trigger.
    popup: Option<Element<'a, Message>>,
    /// What to send when a press lands outside the panel. A panel that can
    /// only be closed by finding its close button again is a panel in the
    /// way: clicking off it is what every other menu here already means.
    on_dismiss: Option<Message>,
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
            on_dismiss: None,
            gap: 6.0,
        }
    }

    /// Close on a press outside the panel.
    ///
    /// The trigger is deliberately not part of "outside": it toggles the
    /// panel itself, and a click that both toggled and dismissed would land
    /// on a panel that never opens.
    pub fn on_dismiss(mut self, message: Message) -> Self {
        self.on_dismiss = Some(message);
        self
    }
}

impl<Message: Clone> Widget<Message, iced::Theme, iced::Renderer> for Popover<'_, Message> {
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
        let on_dismiss = self.on_dismiss.as_ref();
        let gap = self.gap;
        let popup = self.popup.as_mut().map(|popup| {
            overlay::Element::new(Box::new(PopupOverlay {
                anchor: layout.bounds() + translation,
                popup,
                tree: children.next().expect("popup state"),
                on_dismiss,
                gap,
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

impl<'a, Message: Clone + 'a> From<Popover<'a, Message>> for Element<'a, Message> {
    fn from(popover: Popover<'a, Message>) -> Self {
        Element::new(popover)
    }
}

struct PopupOverlay<'a, 'b, Message> {
    anchor: Rectangle,
    popup: &'b mut Element<'a, Message>,
    tree: &'b mut widget::Tree,
    on_dismiss: Option<&'b Message>,
    gap: f32,
}

impl<Message: Clone> overlay::Overlay<Message, iced::Theme, iced::Renderer>
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

        // A press anywhere else closes the panel. The controls inside it have
        // already seen the event above, so what reaches here is a press that
        // was aimed at neither the panel nor the button that opens it.
        let Some(dismiss) = self.on_dismiss else {
            return;
        };
        // A click into another window is the same gesture as a click off the
        // panel, but it arrives as a focus change rather than as a press this
        // window can see.
        if matches!(event, Event::Window(iced::window::Event::Unfocused)) {
            shell.publish(dismiss.clone());
            return;
        }
        if !matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_))
                | Event::Touch(iced::touch::Event::FingerPressed { .. })
        ) {
            return;
        }
        let Some(position) = cursor.position() else {
            return;
        };
        if layout.bounds().contains(position) || self.anchor.contains(position) {
            return;
        }
        shell.publish(dismiss.clone());
        // The click is spent closing the panel: a press that dismisses must
        // not also land on the page underneath and start a stroke there.
        shell.capture_event();
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

    // Without this the panel is a dead end for overlays of its own: the trait
    // defaults to `None`, and every tooltip on a control inside the panel was
    // silently swallowed — hover said nothing, and nothing said why.
    fn overlay<'a>(
        &'a mut self,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.popup.as_widget_mut().overlay(
            self.tree,
            layout,
            renderer,
            &layout.bounds(),
            Vector::ZERO,
        )
    }
}
