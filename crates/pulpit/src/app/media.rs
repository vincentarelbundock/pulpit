//! Media overlays (§79.4): the deck's video/animation/interactive
//! overlays, the sessions that drive them, and the transport gesture a
//! press on one becomes.
//!
//! The media fields (`media`, `media_supervisor`, `media_wakeup`,
//! `attachments_requested`, `media_runtime_warned`, `input_router`,
//! `media_gesture`, `media_fullscreen`, `last_media_click`,
//! `overlay_declarations`, `overlays_dirty`, `pending_overlay_diagnostics`,
//! `pending_pointer_move`) stay on `App` in app.rs, the same shape as the
//! other `app::*` extractions.

use std::time::Instant;

use super::App;

const MEDIA_DRAIN_BUDGET: usize = 32;

impl App {
    /// The overlays the *audience* window composites.
    ///
    /// The same frames the presenter sees, from the same sessions: one
    /// authoritative session feeds both windows. What differs is that nothing
    /// here is ever marked interactive, because a focus ring is chrome and
    /// chrome never reaches the audience.
    pub(crate) fn audience_overlays(&self) -> Vec<crate::widgets::context::SlideOverlay> {
        // A blanked screen shows nothing at all: an overlay drawn over the
        // blank would undo it.
        if self.state.blank().is_blanked() {
            return Vec::new();
        }
        self.current_slide_overlays()
            .into_iter()
            .map(|overlay| crate::widgets::context::SlideOverlay {
                interactive: false,
                ..overlay
            })
            .collect()
    }

    /// The overlays to composite over the audience page, back to front.
    ///
    /// An overlay with no complete frame yet is simply absent, so the PDF
    /// page — or the poster the author drew inside the link rectangle —
    /// shows through instead of a hole.
    pub(super) fn current_slide_overlays(&self) -> Vec<crate::widgets::context::SlideOverlay> {
        let Some(source) = self.state.audience_source() else {
            return Vec::new();
        };
        let fullscreen = self.media_fullscreen_active();
        let mut overlays: Vec<crate::widgets::context::SlideOverlay> = self
            .media
            .index()
            .on_page(source.pdf_page)
            .into_iter()
            .filter_map(|overlay| {
                let frame = self.media.frame(overlay.id)?;
                Some(crate::widgets::context::SlideOverlay {
                    region: overlay.region,
                    handle: frame.handle.clone(),
                    interactive: overlay.is_interactive(),
                    fullscreen: fullscreen == Some(overlay.id),
                })
            })
            .collect();
        // The projected overlay is drawn above everything, whatever its
        // declared z-order was while it sat in its rectangle.
        if fullscreen.is_some() {
            overlays.sort_by_key(|overlay| overlay.fullscreen);
        }
        overlays
    }

    /// The overlay projected across the whole slide area right now, if the
    /// committed page still shows it. The stored choice outlives navigation
    /// so it is validated here, once, for every consumer.
    pub(super) fn media_fullscreen_active(&self) -> Option<pulpit_core::OverlayId> {
        let id = self.media_fullscreen?;
        let source = self.state.audience_source()?;
        self.media
            .index()
            .get(id)
            .filter(|overlay| overlay.covers_page(source.pdf_page))
            .map(|overlay| overlay.id)
    }

    /// The interactive overlay under the pointer, and where inside it.
    ///
    /// Worked out in *page* space rather than panel space: `slide_cursor` has
    /// already had the letterbox and the crop undone, so the overlay's own
    /// rectangle is all that remains to divide by. That also means the answer
    /// is the same whatever size the panel happens to be.
    pub(super) fn overlay_under_cursor(&self) -> Option<(pulpit_core::OverlayId, (f32, f32))> {
        // An armed annotation tool owns the slide; a press must not reach a
        // browser that would then be drawn over.
        if self.annotations.is_armed() {
            return None;
        }
        let (x, y) = self.cursor_on_page()?;
        let source = self.state.audience_source()?;
        // Projected media owns the whole slide: every press on the page is a
        // press on it, and the scrub range runs across the full visible page
        // rather than the rectangle it came from.
        if let Some(id) = self.media_fullscreen_active() {
            let crop = source.region;
            if crop.width > 0.0 && crop.height > 0.0 {
                return Some((id, ((x - crop.x) / crop.width, (y - crop.y) / crop.height)));
            }
        }
        let overlay = self.media.index().hit(source.pdf_page, x, y)?;
        let region = overlay.region;
        if region.width <= 0.0 || region.height <= 0.0 {
            return None;
        }
        Some((
            overlay.id,
            (
                (x - region.x) / region.width,
                (y - region.y) / region.height,
            ),
        ))
    }

    /// Send one routed event to the session that should receive it.
    ///
    /// Web content is told CSS pixels, which is what its own event handlers
    /// deal in; the fraction is scaled by the viewport the session was opened
    /// with so a click lands on the thing under the pointer.
    pub(super) fn deliver(&mut self, routed: crate::media::Routed) {
        use crate::media::Routed;
        let Routed::ToOverlay { overlay, event } = routed else {
            return;
        };
        let Some(session) = self.media.session(overlay) else {
            return;
        };
        let Some(media) = self.media_supervisor.as_mut() else {
            return;
        };
        let (css_width, css_height) = media
            .viewport_of(session)
            .map(|viewport| viewport.css_size())
            .unwrap_or((1280, 720));
        let scaled = match event {
            pulpit_media::InputEvent::PointerMoved { x, y } => {
                let (x, y) = crate::media::overlay::to_css_pixels((x, y), css_width, css_height);
                pulpit_media::InputEvent::PointerMoved { x, y }
            }
            pulpit_media::InputEvent::PointerPressed {
                x,
                y,
                button,
                click_count,
            } => {
                let (x, y) = crate::media::overlay::to_css_pixels((x, y), css_width, css_height);
                pulpit_media::InputEvent::PointerPressed {
                    x,
                    y,
                    button,
                    click_count,
                }
            }
            pulpit_media::InputEvent::PointerReleased {
                x,
                y,
                button,
                click_count,
            } => {
                let (x, y) = crate::media::overlay::to_css_pixels((x, y), css_width, css_height);
                pulpit_media::InputEvent::PointerReleased {
                    x,
                    y,
                    button,
                    click_count,
                }
            }
            other => other,
        };
        // Pointer moves arrive at device rate — up to a kilohertz — and only
        // the newest position means anything to the page. One is held back
        // and flushed on the tick, so the pipe carries at most twenty a
        // second; anything *other* than a move flushes the held move first,
        // so a press never arrives before the motion that led to it.
        if let pulpit_media::InputEvent::PointerMoved { .. } = scaled {
            self.pending_pointer_move = Some((session, scaled));
            return;
        }
        if let Some((held_session, held)) = self.pending_pointer_move.take() {
            media.input(held_session, held);
        }
        media.input(session, scaled);
    }

    /// Send the held pointer move, if there is one.
    pub(super) fn flush_pointer_move(&mut self) {
        let Some((session, event)) = self.pending_pointer_move.take() else {
            return;
        };
        if let Some(media) = self.media_supervisor.as_mut() {
            media.input(session, event);
        }
    }

    /// Route a pointer press at the overlay under the cursor.
    ///
    /// Returns true when an overlay took it, so the caller knows not to
    /// follow a PDF link underneath.
    ///
    /// A press on a video or an animation becomes a transport gesture —
    /// click toggles, drag scrubs — rather than a raw event: the runtimes
    /// have click-toggle parity of their own, so forwarding the press *and*
    /// interpreting it here would toggle twice. Raw input, and the keyboard
    /// focus that comes with it, stay the web overlays' alone.
    pub(super) fn press_overlay(&mut self) -> bool {
        let over = self.overlay_under_cursor();
        if let Some((overlay, (x, _))) = over {
            let kind = self.media.index().get(overlay).map(|o| o.content.kind());
            if let Some(
                kind @ (pulpit_core::overlay::ContentKind::Video
                | pulpit_core::overlay::ContentKind::AnimatedImage),
            ) = kind
            {
                let duration = self.media.progress(overlay).and_then(|p| p.duration);
                self.media_gesture = Some(crate::media::MediaGesture::press(
                    overlay, kind, duration, x,
                ));
                return true;
            }
        }
        let taken = over.is_some();
        let routed = self
            .input_router
            .pointer_pressed(over, pulpit_media::PointerButton::Left);
        self.deliver(routed);
        taken
    }

    /// Feed a pointer move to the media gesture in progress, if any, and
    /// forward whatever seek it decides on. Returns true while a gesture
    /// holds the pointer, so hover routing leaves the drag alone.
    pub(super) fn drag_media_gesture(&mut self) -> bool {
        let Some(overlay) = self.media_gesture.as_ref().map(|g| g.overlay()) else {
            return false;
        };
        // The horizontal fraction inside the overlay, from page coordinates:
        // the same mapping `overlay_under_cursor` uses, except a drag keeps
        // its grip outside the rectangle instead of letting go, exactly as
        // the transport widget's slider would. Projected media spans the
        // whole visible page, so its scrub range does too.
        let position = self.cursor_on_page();
        let region = if self.media_fullscreen_active() == Some(overlay) {
            self.state.audience_source().map(|source| source.region)
        } else {
            self.media.index().get(overlay).map(|o| o.region)
        };
        let command = match (position, region, self.media_gesture.as_mut()) {
            (Some((x, _)), Some(region), Some(gesture)) if region.width > 0.0 => {
                gesture.moved((x - region.x) / region.width)
            }
            _ => None,
        };
        if let (Some(command), Some(supervisor)) = (command, self.media_supervisor.as_mut()) {
            self.media.control(supervisor, overlay, command);
        }
        true
    }

    /// The button came up on a media gesture: send the click's toggle or the
    /// scrub's final position. Returns true when a gesture consumed the
    /// release, so the raw release is not also delivered to a session that
    /// never heard the press.
    pub(super) fn release_media_gesture(&mut self) -> bool {
        use crate::media::TransportCommand;
        /// Two clicks this close on one overlay are a double-click.
        const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);
        let Some(gesture) = self.media_gesture.take() else {
            return false;
        };
        let overlay = gesture.overlay();
        // Unknown state reads as paused, so the first click on a clip that
        // has not reported yet asks it to play rather than doing nothing.
        let paused = self
            .media
            .progress(overlay)
            .map(|progress| progress.paused)
            .unwrap_or(true);
        let command = gesture.release(paused);
        // A second click soon after the first toggles fullscreen, the way
        // every video player does. The play/pause of each click still goes:
        // the second undoes the first, so playback ends where it began.
        if let Some(TransportCommand::Play | TransportCommand::Pause) = command {
            let now = Instant::now();
            let double = self
                .last_media_click
                .take()
                .is_some_and(|(clicked, at)| clicked == overlay && now - at < DOUBLE_CLICK);
            if double {
                self.media_fullscreen = match self.media_fullscreen_active() {
                    Some(_) => None,
                    None => Some(overlay),
                };
            } else {
                self.last_media_click = Some((overlay, now));
            }
        }
        if let (Some(command), Some(supervisor)) = (command, self.media_supervisor.as_mut()) {
            self.media.control(supervisor, overlay, command);
        }
        true
    }

    /// Regroup every page's declarations into logical overlays.
    ///
    /// Page labels, when the producer emitted them, decide where a reveal
    /// sequence ends; without them the consecutive-page equality rule does.
    /// Rebuild the overlay index and restart media servicing, if any
    /// Overlays event has arrived since the last rebuild.
    pub(super) fn flush_overlay_rebuild(&mut self) {
        if !std::mem::take(&mut self.overlays_dirty) {
            return;
        }
        let diagnostics = std::mem::take(&mut self.pending_overlay_diagnostics);
        self.rebuild_overlays(diagnostics);
        self.service_media();
    }

    fn rebuild_overlays(&mut self, diagnostics: Vec<String>) {
        let generation = self.state.generation();
        self.input_router.reset();
        // A drag caught mid-rebuild would resolve against overlays that no
        // longer mean the same thing, and a projection would be of an overlay
        // that may no longer exist.
        self.media_gesture = None;
        self.media_fullscreen = None;
        // The coordinator forgets what it staged when the generation moves on,
        // so this must forget what it *asked for* at the same moment —
        // otherwise an attachment the coordinator no longer holds would never
        // be requested again.
        if generation != self.media.generation() {
            self.attachments_requested.clear();
        }
        self.media.rebuild(
            self.media_supervisor.as_mut(),
            generation,
            &self.overlay_declarations,
            &pulpit_core::overlay::PageLabels::default(),
            diagnostics,
        );
    }

    /// Stage whatever the overlays on the audience page still need, then
    /// open sessions for the ones that are ready.
    ///
    /// Nothing here can blank the audience: an overlay with no session shows
    /// its poster or the PDF page underneath, which is already on screen.
    pub(super) fn service_media(&mut self) {
        let Some(document) = self.state.document().cloned() else {
            return;
        };
        let Some(source) = self.state.audience_source() else {
            return;
        };
        let document_dir = document
            .path
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Ask the renderer for anything still missing. One request per
        // attachment, however many overlays reference it.
        let needs = self.media.needs(source.pdf_page, &document_dir);
        if let Some(supervisor) = self.supervisor.as_mut() {
            for (_, need) in &needs {
                if let crate::media::Need::Attachment(name) = need {
                    if self.attachments_requested.insert(name.clone()) {
                        supervisor.request_attachment(document.id.0, name);
                    }
                }
            }
        }

        // The overlay's size on screen decides the surface it is given, so a
        // browser renders at the size it will actually be shown at. The
        // viewports are computed up front for exactly the overlays on this
        // page — cloning the whole index to satisfy the borrow below cost an
        // index copy per call, on every page's overlay response.
        let panel = self.audience_size;
        let aspect = self.audience_aspect();
        let crop = source.region;
        let scale = self.audience_scale();
        let generation = self.state.generation();
        let reduce_motion = self.motion.is_reduced();
        let viewports: std::collections::HashMap<pulpit_core::OverlayId, pulpit_media::Viewport> =
            self.media
                .index()
                .on_page(source.pdf_page)
                .into_iter()
                .filter_map(|overlay| {
                    let rectangle = crate::media::place(
                        panel,
                        aspect,
                        iced::ContentFit::Contain,
                        crop,
                        overlay.region,
                    )?;
                    let (width, height) = crate::media::viewport_for(rectangle, scale);
                    Some((
                        overlay.id,
                        pulpit_media::Viewport::new(width, height, scale),
                    ))
                })
                .collect();

        let Some(media) = self.media_supervisor.as_mut() else {
            return;
        };
        // Before the runtime probes land, selection would silently choose
        // the static poster for every overlay — and that choice sticks for
        // the session. `Message::MediaProbed` calls back in here.
        if !media.probed() {
            return;
        }
        media.retire_generation(generation);
        self.media.open_ready(
            media,
            source.pdf_page,
            |id| viewports.get(&id).copied(),
            crate::media::worker_command,
            reduce_motion,
        );
        self.media.follow_page(media, source.pdf_page);
        // Navigating away from the projected overlay ends the projection for
        // good: coming back to the slide later must show it as authored, not
        // resume a fullscreen nobody asked for a second time.
        if let Some(id) = self.media_fullscreen {
            let still_here = self
                .media
                .index()
                .get(id)
                .is_some_and(|overlay| overlay.covers_page(source.pdf_page));
            if !still_here {
                self.media_fullscreen = None;
            }
        }
    }

    /// Drain the media supervisor, holding every complete frame.
    pub(super) fn poll_media(&mut self) -> bool {
        let started = Instant::now();
        let Some(media) = self.media_supervisor.as_mut() else {
            return false;
        };
        let batch = media.poll_bounded(crate::media::worker_command, MEDIA_DRAIN_BUDGET);
        let events = batch.events;
        let event_count = events.len();
        for event in events {
            match event {
                pulpit_media::SessionEvent::Ready {
                    overlay, runtime, ..
                } => {
                    self.diagnostics
                        .note(format!("{overlay} is playing through {runtime}"));
                }
                pulpit_media::SessionEvent::Frame {
                    overlay,
                    generation,
                    sequence,
                    width,
                    height,
                    rgba,
                    ..
                } => {
                    self.media
                        .frame_ready(overlay, generation, sequence, width, height, rgba);
                }
                pulpit_media::SessionEvent::Progress {
                    overlay, progress, ..
                } => {
                    self.media.progress_reported(overlay, progress);
                }
                pulpit_media::SessionEvent::Warning {
                    overlay, warning, ..
                } => {
                    // Presenter-only: warnings never reach the audience
                    // window, and never replace the frame on screen.
                    self.diagnostics.note(format!("{overlay}: {warning}"));
                    if matches!(warning, pulpit_media::MediaWarning::ContentRestarted) {
                        self.notify("The interactive content restarted.".to_string());
                    }
                }
                pulpit_media::SessionEvent::Failed {
                    overlay,
                    error,
                    exhausted,
                    ..
                } => {
                    self.diagnostics
                        .note(format!("{overlay}: {} ({:?})", error.message, error.kind));
                    self.media.session_failed(overlay, exhausted);
                    if exhausted {
                        self.input_router.forget(overlay);
                        // A runtime that could not even start is a setup
                        // problem, not a property of the deck, and the
                        // presenter cannot diagnose a poster that silently
                        // never becomes a video. Said once per run: every
                        // overlay on the slide would otherwise report it.
                        let missing = matches!(
                            error.kind,
                            pulpit_media::MediaErrorKind::LaunchFailed
                                | pulpit_media::MediaErrorKind::Unavailable
                        );
                        if missing && !self.media_runtime_warned {
                            self.media_runtime_warned = true;
                            self.notify(format!(
                                "Media on this slide is showing its still image: {}",
                                error.message
                            ));
                        }
                    }
                }
                pulpit_media::SessionEvent::Closed { .. } => {}
            }
        }
        if event_count > 0 {
            self.latency
                .record_stage(|stages| &mut stages.drain_media, started.elapsed());
        }
        batch.more
    }
}
