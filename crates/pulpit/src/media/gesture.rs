//! What a press on the slide's own media means.
//!
//! The transport widget is not the only transport: the clip itself is one. A
//! click on a video or an animation toggles playback, and a horizontal drag
//! on a clip with a known length scrubs it, exactly as the widget's slider
//! would. The decisions live here, pure, so every case — the jittery click,
//! the drag on a GIF that has no playhead, the stream with no duration — is
//! an ordinary unit test.
//!
//! The gesture speaks [`TransportCommand`], never raw pointer events. The
//! runtimes have click-parity behaviour of their own (the mpv worker and the
//! browser wrapper pages both toggle on a click), so forwarding the raw press
//! *and* interpreting it here would toggle twice; media overlays therefore
//! get transport commands from this interpreter, and raw input remains the
//! web overlays' alone.

use pulpit_core::overlay::ContentKind;
use pulpit_core::OverlayId;

use crate::media::TransportCommand;

/// Movement beyond this, as a fraction of the overlay's width, stops a press
/// being a click. One percent of the clip's width is above the tremor of a
/// hand holding a mouse still and below the smallest deliberate drag.
const CLICK_TOLERANCE: f32 = 0.01;

/// Seeks closer together than this are not worth a pipe round trip each;
/// pointer moves arrive at device rate and the position readout does not
/// show tenths anyway. The release always sends the final position exactly.
const SEEK_GRAIN_SECONDS: f32 = 0.1;

/// One press on a video or animated-image overlay, from button-down to
/// button-up.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaGesture {
    overlay: OverlayId,
    kind: ContentKind,
    /// The clip's length, when the content has reported one. `None` for an
    /// animation, and for a clip whose metadata has not arrived: neither has
    /// a playhead a drag could move.
    duration: Option<f32>,
    /// Where the press landed, horizontally, as a fraction of the overlay.
    pressed_x: f32,
    /// Where the pointer is now, in the same fraction.
    latest_x: f32,
    /// The last seek actually emitted, for coalescing.
    last_seek: Option<f32>,
    scrubbing: bool,
}

impl MediaGesture {
    /// A press landed on a media overlay at horizontal fraction `x`.
    pub fn press(overlay: OverlayId, kind: ContentKind, duration: Option<f32>, x: f32) -> Self {
        Self {
            overlay,
            kind,
            duration: duration.filter(|seconds| seconds.is_finite() && *seconds > 0.0),
            pressed_x: x,
            latest_x: x,
            last_seek: None,
            scrubbing: false,
        }
    }

    pub fn overlay(&self) -> OverlayId {
        self.overlay
    }

    /// The length a drag would scrub across, if a drag means anything here.
    /// Only a clip has a playhead; an animation loops without one whatever
    /// its runtime happens to report.
    fn scrub_length(&self) -> Option<f32> {
        (self.kind == ContentKind::Video)
            .then_some(self.duration)
            .flatten()
    }

    /// The playhead position a horizontal fraction means, clamped to the
    /// clip: dragging past either edge holds at that edge.
    fn seconds_at(&self, x: f32, length: f32) -> f32 {
        x.clamp(0.0, 1.0) * length
    }

    /// The pointer moved to horizontal fraction `x` with the button down.
    pub fn moved(&mut self, x: f32) -> Option<TransportCommand> {
        self.latest_x = x;
        let length = self.scrub_length()?;
        if !self.scrubbing && (x - self.pressed_x).abs() <= CLICK_TOLERANCE {
            return None;
        }
        self.scrubbing = true;
        let seconds = self.seconds_at(x, length);
        if let Some(last) = self.last_seek {
            if (seconds - last).abs() < SEEK_GRAIN_SECONDS {
                return None;
            }
        }
        self.last_seek = Some(seconds);
        Some(TransportCommand::SeekTo(seconds))
    }

    /// The button came up. `paused` is where playback stands now, so a click
    /// knows which way to toggle.
    ///
    /// A scrub ends with the exact final position, uncoalesced. A press that
    /// stayed put is a click and toggles. A press that wandered on content
    /// with no playhead is neither: released buttons the pointer dragged off
    /// have always meant "never mind", and a GIF must not start because a
    /// drag on it could not scrub.
    pub fn release(self, paused: bool) -> Option<TransportCommand> {
        if self.scrubbing {
            let length = self.scrub_length()?;
            return Some(TransportCommand::SeekTo(
                self.seconds_at(self.latest_x, length),
            ));
        }
        if (self.latest_x - self.pressed_x).abs() > CLICK_TOLERANCE {
            return None;
        }
        Some(if paused {
            TransportCommand::Play
        } else {
            TransportCommand::Pause
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: OverlayId = OverlayId(1);

    fn video(duration: Option<f32>, x: f32) -> MediaGesture {
        MediaGesture::press(CLIP, ContentKind::Video, duration, x)
    }

    #[test]
    fn a_click_on_a_paused_clip_plays_it_and_on_a_playing_one_pauses_it() {
        assert_eq!(
            video(Some(120.0), 0.5).release(true),
            Some(TransportCommand::Play)
        );
        assert_eq!(
            video(Some(120.0), 0.5).release(false),
            Some(TransportCommand::Pause)
        );
    }

    #[test]
    fn a_click_on_an_animation_toggles_it_too() {
        let gesture = MediaGesture::press(CLIP, ContentKind::AnimatedImage, None, 0.5);
        assert_eq!(gesture.release(false), Some(TransportCommand::Pause));
    }

    #[test]
    fn hand_tremor_does_not_turn_a_click_into_a_drag() {
        let mut gesture = video(Some(120.0), 0.5);
        assert_eq!(gesture.moved(0.505), None);
        assert_eq!(gesture.release(true), Some(TransportCommand::Play));
    }

    #[test]
    fn a_drag_on_a_clip_scrubs_it_and_the_release_lands_exactly() {
        let mut gesture = video(Some(100.0), 0.2);
        assert_eq!(
            gesture.moved(0.5),
            Some(TransportCommand::SeekTo(50.0)),
            "half way across a 100 s clip is 50 s"
        );
        assert_eq!(gesture.moved(0.8), Some(TransportCommand::SeekTo(80.0)));
        // The release re-sends the final position and never toggles: the
        // presenter moved the playhead, they did not press the button.
        assert_eq!(gesture.release(false), Some(TransportCommand::SeekTo(80.0)));
    }

    #[test]
    fn scrubbing_past_either_edge_holds_at_that_edge() {
        let mut gesture = video(Some(100.0), 0.5);
        assert_eq!(gesture.moved(-0.3), Some(TransportCommand::SeekTo(0.0)));
        assert_eq!(gesture.moved(1.4), Some(TransportCommand::SeekTo(100.0)));
    }

    #[test]
    fn device_rate_moves_are_coalesced_but_a_real_step_gets_through() {
        let mut gesture = video(Some(100.0), 0.0);
        assert_eq!(gesture.moved(0.5), Some(TransportCommand::SeekTo(50.0)));
        // 0.05 s further on: below the grain, not worth a round trip.
        assert_eq!(gesture.moved(0.5005), None);
        assert_eq!(gesture.moved(0.51), Some(TransportCommand::SeekTo(51.0)));
    }

    #[test]
    fn once_scrubbing_a_return_to_the_press_point_keeps_scrubbing() {
        // Dragging out and back must not turn the release into a toggle.
        let mut gesture = video(Some(100.0), 0.5);
        assert!(gesture.moved(0.9).is_some());
        gesture.moved(0.5);
        assert_eq!(gesture.release(true), Some(TransportCommand::SeekTo(50.0)));
    }

    #[test]
    fn an_animation_cannot_be_scrubbed_whatever_its_runtime_reports() {
        // mpv knows a GIF's loop length, but ImageCommand has no seek: a drag
        // must not silently arm a scrub that nothing can perform.
        let mut gesture = MediaGesture::press(CLIP, ContentKind::AnimatedImage, Some(2.4), 0.2);
        assert_eq!(gesture.moved(0.9), None);
        assert_eq!(
            gesture.release(true),
            None,
            "a drag off content with no playhead means never mind, not play"
        );
    }

    #[test]
    fn a_clip_with_no_known_duration_clicks_but_does_not_scrub() {
        let mut gesture = video(None, 0.2);
        assert_eq!(gesture.moved(0.9), None, "there is no range to seek within");
        assert_eq!(gesture.release(true), None);

        let unmoved = video(None, 0.2);
        assert_eq!(unmoved.release(true), Some(TransportCommand::Play));
    }

    #[test]
    fn an_implausible_duration_is_treated_as_none() {
        for bad in [f32::NAN, 0.0, -3.0] {
            let mut gesture = video(Some(bad), 0.2);
            assert_eq!(gesture.moved(0.9), None, "{bad} is not a length");
        }
    }
}
