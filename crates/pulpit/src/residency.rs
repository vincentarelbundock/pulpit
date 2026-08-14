//! Keeping a window's pictures resident in *that window's* renderer.
//!
//! Residency is per window, not per application. Iced 0.14 gives every window
//! its own image cache and its own atlas, and the explicit `image::allocate`
//! task reaches exactly one of them: the runtime services the action with
//! `window_manager.iter_mut().next()`, the lowest-numbered window, under a
//! standing upstream TODO about a shared cache in the compositor. An
//! application-wide "this frame is uploaded" gate is therefore not a fact
//! about the window that has to draw it.
//!
//! The gap is visible rather than theoretical. `iced_wgpu` uploads any image
//! of two mebibytes or more on a worker thread, and while that upload is in
//! flight the renderer's prepare pass *skips the image entirely* — the window
//! paints its background where the picture should be. Both slide panels
//! (megabytes) and audience frames (tens of megabytes) are over that line, so
//! a page turn drew a black rectangle for a pass on whichever window had not
//! been told about the picture.
//!
//! [`resident`] wraps a window's view and holds an `Allocation` obtained from
//! the renderer that will actually draw it, for exactly the pictures that
//! window shows. Loading is synchronous — that is the whole point — and runs
//! at layout, comfortably ahead of the prepare pass that would otherwise skip
//! the picture. At most one picture is loaded per pass, wanted-first: what is
//! on screen this pass, then what the next page turn will want, which is
//! uploaded while the window is otherwise idle.
//!
//! Holding the allocation carries the entry through the renderer's trim pass,
//! so a picture is uploaded once rather than once per appearance. Holding it
//! for *anything else* is the expensive mistake: an atlas grows to fit
//! whatever is kept resident, and growing it copies every layer already in it,
//! so a window must be asked to keep only what it draws.

use std::cell::RefCell;
use std::time::Duration;

use iced::advanced::image::Id;
use iced::advanced::image::Renderer as _;
use iced::advanced::widget::{self, Widget};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::widget::image::{Allocation, Handle};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

/// An upload long enough to be worth naming in the log. A frame at sixty
/// hertz is about sixteen milliseconds, so anything at or above this cost the
/// window a frame it could otherwise have drawn.
const SLOW_UPLOAD: Duration = Duration::from_millis(8);

/// Draw `content`, keeping `wanted` uploaded in this window's renderer.
///
/// The first handle is the one on screen; the rest are what the next
/// navigation will want.
pub fn resident<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    wanted: Vec<Handle>,
    meter: crate::latency::UploadMeter,
) -> Element<'a, Message> {
    Element::new(Resident {
        content: content.into(),
        wanted,
        meter,
    })
}

struct Resident<'a, Message> {
    content: Element<'a, Message>,
    wanted: Vec<Handle>,
    /// Where the blocking uploads below are reported, so the time this widget
    /// takes off the event loop appears in the same report as the time
    /// everything else does.
    meter: crate::latency::UploadMeter,
}

/// The allocations this window holds, in the order they were asked for.
///
/// Widget state rather than application state because an allocation belongs to
/// one renderer: the same frame is a different residency in each window, and
/// only the widget knows which window drew it.
///
/// Keyed by [`Id`], never by the handle itself. Two handles compare equal by
/// comparing their *pixels*, so asking "do I already hold this one?" with `==`
/// memcmps tens of megabytes several times per draw pass — which reads as a
/// sluggish application everywhere, not as a slow projector.
#[derive(Default)]
struct Held {
    allocations: RefCell<Vec<(Id, Allocation)>>,
    /// Pictures this renderer refused. A refusal is usually the atlas being
    /// out of room, which the next pass will not fix, and retrying the same
    /// one for ever would stand in front of the pictures behind it — which
    /// may be perfectly uploadable.
    refused: RefCell<Vec<Id>>,
}

impl Held {
    /// Drop what is no longer wanted, then upload at most one missing picture.
    ///
    /// One per pass because `load_image` blocks until the upload has landed:
    /// several tens of mebibytes at once would trade a flash for a stall. The
    /// application redraws on its tick, so the rest follow within a few frames
    /// — long before a page turn needs them.
    fn sync(
        &self,
        wanted: &[Handle],
        renderer: &iced::Renderer,
        meter: &crate::latency::UploadMeter,
    ) {
        let mut held = self.allocations.borrow_mut();
        let mut refused = self.refused.borrow_mut();
        held.retain(|(id, _)| wanted.iter().any(|handle| handle.id() == *id));
        // A refusal is only remembered while its picture is still wanted, so a
        // page that comes round again after the pressure has passed gets a
        // fresh attempt rather than a permanent black mark.
        refused.retain(|id| wanted.iter().any(|handle| handle.id() == *id));

        let mut resident: Vec<Id> = held.iter().map(|(id, _)| *id).collect();
        resident.extend(refused.iter().copied());
        let Some(missing) = next_missing(wanted, &resident) else {
            return;
        };
        // Timed because this blocks: it is the one place a page turn can stop
        // on a GPU upload, and it is invisible from the application, which
        // owns no part of a window's residency. A slow one is reported rather
        // than inferred.
        let start = std::time::Instant::now();
        let outcome = renderer.load_image(&missing);
        let elapsed = start.elapsed();
        meter.record(elapsed);
        if elapsed >= SLOW_UPLOAD {
            tracing::debug!(
                millis = elapsed.as_millis(),
                "a picture blocked the event loop while it uploaded"
            );
        }
        match outcome {
            Ok(allocation) => held.push((missing.id(), allocation)),
            // Nothing to do but move on to the next picture: the frame is
            // still the best thing this window has, and refusing to draw it
            // would be the blank this module exists to prevent.
            Err(error) => {
                tracing::warn!(%error, "image upload refused");
                refused.push(missing.id());
            }
        }
    }
}

/// The first wanted picture this window has neither uploaded nor been refused.
fn next_missing(wanted: &[Handle], settled: &[Id]) -> Option<Handle> {
    wanted
        .iter()
        .find(|handle| !settled.contains(&handle.id()))
        .cloned()
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Resident<'_, Message> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<Held>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(Held::default())
    }

    fn children(&self) -> Vec<widget::Tree> {
        vec![widget::Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut widget::Tree) {
        tree.diff_children(&[self.content.as_widget()]);
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
        // The earliest point in a pass where this window's renderer is in
        // hand. What has to be true is only that the upload precedes the
        // prepare pass — which is where an unuploaded picture is silently
        // skipped — but doing it here keeps the whole pass, measurement
        // included, working from a picture that is really there.
        tree.state
            .downcast_ref::<Held>()
            .sync(&self.wanted, renderer, &self.meter);
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
        // A backstop for a redraw that reuses the laid-out interface: the
        // steady state is an identifier scan and no upload at all.
        tree.state
            .downcast_ref::<Held>()
            .sync(&self.wanted, renderer, &self.meter);
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

#[cfg(test)]
mod tests {
    use super::next_missing;
    use iced::widget::image::Handle;

    fn handle(seed: u8) -> Handle {
        Handle::from_rgba(1, 1, vec![seed, 0, 0, 255])
    }

    #[test]
    fn the_picture_on_screen_is_uploaded_before_the_ones_a_step_away() {
        let on_screen = handle(1);
        let next = handle(2);
        assert_eq!(
            next_missing(&[on_screen.clone(), next], &[]),
            Some(on_screen)
        );
    }

    #[test]
    fn nothing_is_uploaded_twice() {
        let held = handle(1);
        assert_eq!(
            next_missing(std::slice::from_ref(&held), &[held.id()]),
            None
        );
    }

    #[test]
    fn a_step_away_frame_follows_once_the_screen_is_covered() {
        let on_screen = handle(1);
        let next = handle(2);
        assert_eq!(
            next_missing(&[on_screen.clone(), next.clone()], &[on_screen.id()]),
            Some(next)
        );
    }

    /// Two pictures of the same size are told apart by identity, never by
    /// their pixels: comparing handles compares the whole bitmap, and doing
    /// that on every draw pass is what a stalled application is made of.
    #[test]
    fn identical_pixels_are_still_two_pictures() {
        let one = Handle::from_rgba(1, 1, vec![7, 7, 7, 255]);
        let other = Handle::from_rgba(1, 1, vec![7, 7, 7, 255]);
        assert_eq!(
            next_missing(std::slice::from_ref(&other), &[one.id()]),
            Some(other)
        );
    }
}
