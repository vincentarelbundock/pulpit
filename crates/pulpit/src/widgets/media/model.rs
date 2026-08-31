//! What the media transport shows, decided without drawing anything.
//!
//! The view's job is layout; every question with a right answer — what the
//! button means, whether the scrub bar can be dragged, what the readout says
//! — is settled here, where it can be tested without a window.

use pulpit_core::overlay::ContentKind;

use crate::media::TransportTarget;

/// What the button under the presenter's thumb does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Play,
    Pause,
}

impl Action {
    /// A symbol, not a word: the pane is small, and these two are the least
    /// ambiguous glyphs in the interface.
    pub fn glyph(self) -> &'static str {
        match self {
            Action::Play => "\u{25B6}",
            Action::Pause => "\u{23F8}",
        }
    }
}

/// Everything the transport needs to draw itself, once.
#[derive(Debug, Clone, PartialEq)]
pub struct Transport {
    /// What pressing the button asks for.
    pub action: Action,
    /// Playhead and length, when the content has both.
    pub position: f32,
    pub duration: Option<f32>,
    /// Can the presenter actually drive this? False for a still poster, for a
    /// session that has not reported yet, and for an animation with no
    /// playhead to scrub.
    pub enabled: bool,
    pub scrubbable: bool,
    /// Audio exists and is currently silenced.
    pub mutable: bool,
    pub muted: bool,
    /// Whether the media is currently projected across the whole slide area,
    /// on the audience and presenter screens together.
    pub fullscreen: bool,
    /// The line shown where the times go.
    pub readout: String,
}

impl Transport {
    /// The transport for whatever the slide is carrying.
    ///
    /// `None` means the slide has no media at all, which is not the same as
    /// media that cannot be driven: an overlay whose runtime never started
    /// still gets a transport, drawn inert, because a presenter who put a
    /// video on this slide should be told it is not going to play rather than
    /// be shown an empty pane.
    pub fn for_target(target: Option<TransportTarget>, fullscreen: bool) -> Option<Transport> {
        let target = target?;
        let progress = target.progress;
        let paused = progress.map(|p| p.paused).unwrap_or(true);
        let video = target.kind == ContentKind::Video;
        let duration = progress.and_then(|p| p.duration).filter(|_| video);
        let position = progress.map(|p| p.position).unwrap_or(0.0);
        // Reported at all, and by something still running: a poster left
        // behind by a dead session must not offer a button that does nothing.
        let enabled = target.live && progress.is_some();
        Some(Transport {
            action: if paused { Action::Play } else { Action::Pause },
            position,
            duration,
            enabled,
            scrubbable: enabled && duration.is_some(),
            mutable: enabled && video,
            muted: progress.map(|p| p.muted).unwrap_or(false),
            fullscreen,
            readout: readout(target, position, duration),
        })
    }
}

/// What the times line says.
fn readout(target: TransportTarget, position: f32, duration: Option<f32>) -> String {
    if !target.live {
        return "Not playing".to_string();
    }
    match (target.kind, duration) {
        (ContentKind::Video, Some(duration)) => {
            format!("{} / {}", clock(position), clock(duration))
        }
        // A stream, or a clip whose metadata has not arrived: the elapsed
        // time is still true and still useful, and an em dash is honest about
        // the rest rather than guessing a length.
        (ContentKind::Video, None) => format!("{} / —", clock(position)),
        (ContentKind::AnimatedImage, _) => "Animation".to_string(),
        (ContentKind::Web, _) => "Interactive".to_string(),
    }
}

/// `m:ss`, or `h:mm:ss` once there is an hour to show.
fn clock(seconds: f32) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds as u64
    } else {
        0
    };
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::OverlayId;
    use pulpit_media::protocol::PlaybackProgress;

    fn progress(position: f32, duration: Option<f32>, paused: bool) -> PlaybackProgress {
        PlaybackProgress {
            position,
            duration,
            paused,
            muted: false,
            volume: 1.0,
        }
    }

    fn target(
        kind: ContentKind,
        progress: Option<PlaybackProgress>,
        live: bool,
    ) -> TransportTarget {
        TransportTarget {
            overlay: OverlayId(1),
            kind,
            progress,
            live,
        }
    }

    #[test]
    fn a_slide_with_no_media_has_no_transport() {
        assert!(Transport::for_target(None, false).is_none());
    }

    #[test]
    fn a_playing_clip_offers_pause_and_a_scrub_bar() {
        let transport = Transport::for_target(
            Some(target(
                ContentKind::Video,
                Some(progress(30.0, Some(120.0), false)),
                true,
            )),
            false,
        )
        .unwrap();
        assert_eq!(transport.action, Action::Pause);
        assert!(transport.scrubbable);
        assert_eq!(transport.readout, "0:30 / 2:00");
        // The scrub bar's range comes straight from these two, in seconds.
        assert_eq!(transport.duration, Some(120.0));
        assert_eq!(transport.position, 30.0);
    }

    #[test]
    fn a_paused_clip_offers_play() {
        let transport = Transport::for_target(
            Some(target(
                ContentKind::Video,
                Some(progress(0.0, Some(10.0), true)),
                true,
            )),
            false,
        )
        .unwrap();
        assert_eq!(transport.action, Action::Play);
    }

    /// The presenter put a video here and it is showing a still. Saying so is
    /// the whole reason this widget draws at all in that case.
    #[test]
    fn media_whose_runtime_never_started_is_shown_but_not_offered() {
        let transport =
            Transport::for_target(Some(target(ContentKind::Video, None, false)), false).unwrap();
        assert!(!transport.enabled, "a dead session must not offer controls");
        assert!(!transport.scrubbable);
        assert_eq!(transport.readout, "Not playing");
    }

    #[test]
    fn an_animation_can_be_stopped_but_not_scrubbed_or_muted() {
        let transport = Transport::for_target(
            Some(target(
                ContentKind::AnimatedImage,
                Some(progress(0.0, None, false)),
                true,
            )),
            false,
        )
        .unwrap();
        assert_eq!(transport.action, Action::Pause);
        assert!(transport.enabled, "a GIF can still be frozen");
        assert!(!transport.scrubbable, "a GIF has no playhead");
        assert!(!transport.mutable, "a GIF has no audio");
        assert_eq!(transport.readout, "Animation");
    }

    /// A stream has no length. The elapsed half is still true, so it is still
    /// shown rather than the whole readout being thrown away.
    #[test]
    fn a_clip_with_no_known_duration_still_reports_where_it_is() {
        let transport = Transport::for_target(
            Some(target(
                ContentKind::Video,
                Some(progress(75.0, None, false)),
                true,
            )),
            false,
        )
        .unwrap();
        assert_eq!(transport.readout, "1:15 / —");
        assert!(!transport.scrubbable, "there is no range to scrub within");
    }

    #[test]
    fn the_transport_reports_the_projection_it_was_told_about() {
        let projected = Transport::for_target(
            Some(target(
                ContentKind::Video,
                Some(progress(30.0, Some(120.0), false)),
                true,
            )),
            true,
        )
        .unwrap();
        assert!(projected.fullscreen);
        let ordinary = Transport::for_target(
            Some(target(
                ContentKind::Video,
                Some(progress(30.0, Some(120.0), false)),
                true,
            )),
            false,
        )
        .unwrap();
        assert!(!ordinary.fullscreen);
    }

    #[test]
    fn an_hour_long_recording_reads_as_hours() {
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(59.0), "0:59");
        // Nothing a media element can report should produce a negative or a
        // NaN clock.
        assert_eq!(clock(-5.0), "0:00");
        assert_eq!(clock(f32::NAN), "0:00");
    }
}
