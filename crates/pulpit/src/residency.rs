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
//! the picture.
//!
//! The two lists are not two priorities of the same thing. Everything in
//! `on_screen` is drawn *this pass*, so all of it is uploaded before layout
//! returns: a picture left for the next pass is a picture the prepare pass
//! skips, which is the flash, and no ordering within one pass can rescue the
//! second item. `ahead` is what the next page turn will want and nothing draws
//! yet, so at most one of those is taken per pass — spreading the cost over the
//! passes the window would otherwise spend idle.
//!
//! Uploading everything on screen was not the original rule, and the case that
//! changed it is a form commit: committing a radio button or a drop-down makes
//! a new document snapshot, which re-renders *every visible page* at a new
//! generation at once. One upload per pass then meant every page but the first
//! drew as bare sheet for a pass or more — the whole-page flash — where a page
//! turn had been survivable only because prefetch had uploaded its one new
//! picture while the window was idle.
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

use crate::widgets::forward_to_child;
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::widget::image::{Allocation, Handle};
use iced::{Element, Event, Length, Rectangle, Size};

/// An upload long enough to be worth naming in the log. A frame at sixty
/// hertz is about sixteen milliseconds, so anything at or above this cost the
/// window a frame it could otherwise have drawn.
const SLOW_UPLOAD: Duration = Duration::from_millis(8);

/// Draw `content`, keeping its pictures uploaded in this window's renderer.
///
/// `on_screen` is what this pass draws and is uploaded in full before the pass
/// goes on; `ahead` is what the next navigation will want, taken one per pass.
pub fn resident<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    wanted: Wanted,
    meter: crate::latency::UploadMeter,
) -> Element<'a, Message> {
    Element::new(Resident {
        content: content.into(),
        wanted,
        meter,
    })
}

/// The pictures a window wants, split by whether anything draws them yet.
#[derive(Debug, Default, Clone)]
pub struct Wanted {
    /// Drawn this pass. All of it is uploaded before layout returns.
    pub on_screen: Vec<Handle>,
    /// Wanted by the next page turn. One is uploaded per pass.
    pub ahead: Vec<Handle>,
}

impl Wanted {
    fn all(&self) -> impl Iterator<Item = &Handle> {
        self.on_screen.iter().chain(self.ahead.iter())
    }
}

struct Resident<'a, Message> {
    content: Element<'a, Message>,
    wanted: Wanted,
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
    /// Drop what is no longer wanted, then upload everything drawn this pass
    /// and at most one picture drawn by no-one yet.
    ///
    /// The split is the whole rule. `load_image` blocks until the upload has
    /// landed, so taking the `ahead` list one at a time is what keeps a
    /// prefetch from trading a flash for a stall; the application redraws on
    /// its tick, so the rest follow within a few frames, long before a page
    /// turn needs them. `on_screen` gets no such grace: whatever is not
    /// uploaded when this returns is skipped by the prepare pass and paints as
    /// background, so all of it goes now, however many that is.
    fn sync(
        &self,
        wanted: &Wanted,
        renderer: &iced::Renderer,
        meter: &crate::latency::UploadMeter,
    ) {
        let mut held = self.allocations.borrow_mut();
        let mut refused = self.refused.borrow_mut();
        held.retain(|(id, _)| wanted.all().any(|handle| handle.id() == *id));
        // A refusal is only remembered while its picture is still wanted, so a
        // page that comes round again after the pressure has passed gets a
        // fresh attempt rather than a permanent black mark.
        refused.retain(|id| wanted.all().any(|handle| handle.id() == *id));

        for handle in &wanted.on_screen {
            if settled(&held, &refused, handle.id()) {
                continue;
            }
            upload(handle, &mut held, &mut refused, renderer, meter);
        }

        let mut resident: Vec<Id> = held.iter().map(|(id, _)| *id).collect();
        resident.extend(refused.iter().copied());
        if let Some(missing) = next_missing(&wanted.ahead, &resident) {
            upload(&missing, &mut held, &mut refused, renderer, meter);
        }
    }
}

/// Whether this window has already settled the question for one picture,
/// either by holding it or by being refused it.
fn settled(held: &[(Id, Allocation)], refused: &[Id], id: Id) -> bool {
    held.iter().any(|(held, _)| *held == id) || refused.contains(&id)
}

/// Put one picture on this window's GPU, and remember which way it went.
fn upload(
    handle: &Handle,
    held: &mut Vec<(Id, Allocation)>,
    refused: &mut Vec<Id>,
    renderer: &iced::Renderer,
    meter: &crate::latency::UploadMeter,
) {
    // Timed because this blocks: it is the one place a page turn can stop
    // on a GPU upload, and it is invisible from the application, which
    // owns no part of a window's residency. A slow one is reported rather
    // than inferred.
    let start = std::time::Instant::now();
    let outcome = renderer.load_image(handle);
    let elapsed = start.elapsed();
    meter.record(elapsed);
    if elapsed >= SLOW_UPLOAD {
        tracing::debug!(
            millis = elapsed.as_millis(),
            "a picture blocked the event loop while it uploaded"
        );
    }
    match outcome {
        Ok(allocation) => held.push((handle.id(), allocation)),
        // Nothing to do but move on to the next picture: the frame is
        // still the best thing this window has, and refusing to draw it
        // would be the blank this module exists to prevent.
        Err(error) => {
            tracing::warn!(%error, "image upload refused");
            refused.push(handle.id());
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
    forward_to_child!(
        content: children,
        diff,
        size,
        size_hint,
        mouse_interaction,
        operate,
        update,
        overlay
    );

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<Held>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(Held::default())
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
}

#[cfg(test)]
mod tests {
    use super::{next_missing, settled};
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

    /// A picture this renderer refused is not asked for again while it is
    /// still wanted, and is not mistaken for one nothing has been decided
    /// about: either way there is nothing left to do about it this pass.
    #[test]
    fn a_refusal_settles_a_picture_as_surely_as_an_upload_does() {
        let one = handle(1);
        assert!(settled(&[], &[one.id()], one.id()));
        assert!(!settled(&[], &[], one.id()));
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
