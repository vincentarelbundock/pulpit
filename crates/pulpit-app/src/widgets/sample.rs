//! Realistic stand-in content for the editor.
//!
//! The editor draws widgets with this so a layout can be judged before a
//! deck is open. It lives with the widgets because it is what they show.

/// Realistic sample notes for preview mode.
pub const NOTES: &str = "\
Open with the projector story — everyone in the room has lived it.\n\n\
• Three failure modes: the cable, the mirroring, the reconnect.\n\
• Show the reconnect trace: index changed, resolution changed, scale changed.\n\
• Land the point: the bug is stale state, not bad luck.\n\n\
Then move to the demo. Unplug the projector mid-slide.";

/// The long-notes simulation the preview offers.
#[allow(dead_code)]
pub const NOTES_LONG: &str = "\
Open with the projector story — everyone in the room has lived it, and it buys \
you thirty seconds of goodwill before the first diagram.\n\n\
• Three failure modes worth naming: the cable that is not seated, the display \
  that silently mirrors instead of extending, and the reconnect that comes back \
  at a different index.\n\
• Show the reconnect trace on screen. Walk the audience through what changed: \
  the enumeration index, the resolution, and the scale factor. Point out that \
  each one alone is survivable and all three together are what break every \
  presenter people have used.\n\
• Land the point: the bug is stale state, not bad luck. Anything that caches a \
  monitor handle across a topology change is holding a lie.\n\n\
Then move to the demo. Unplug the projector mid-slide, keep talking, and let \
the audience notice that the slides never stopped. Plug it back in without \
looking at the laptop.\n\n\
If time is short, cut the mirroring section entirely — it is the least \
surprising of the three and the demo makes the point better than the diagram.";

/// Annotations for the editor: a spotlight and a stroke, so a palette and a
/// slide pane both show what they will look like in front of an audience.
///
/// A static rather than a constructor because the render context borrows it,
/// and the editor's context has to outlive the call that builds it.
pub static ANNOTATIONS: std::sync::LazyLock<std::sync::Arc<pulpit_core::annotation::Annotations>> =
    std::sync::LazyLock::new(|| {
        use pulpit_core::annotation::{Annotations, InkColor};
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.22, 0.62), 0.006, InkColor::Red);
        for point in [(0.34, 0.58), (0.46, 0.63), (0.58, 0.55)] {
            annotations.extend_stroke(point);
        }
        annotations.end_stroke();
        annotations.set_spotlight(Some((0.5, 0.35)));
        std::sync::Arc::new(annotations)
    });

/// Cues for the editor, so the clock is sized against a line that has
/// something in it rather than against "set an alarm".
///
/// A static for the same reason [`ANNOTATIONS`] is one: the render context
/// borrows it and has to outlive the call that builds it.
pub static ALARMS: std::sync::LazyLock<crate::widgets::AlarmControls> =
    std::sync::LazyLock::new(|| {
        crate::widgets::AlarmControls::new(vec![crate::widgets::Alarm::new(
            14 * 3600 + 20 * 60,
            Some("handoff".to_string()),
        )])
    });

/// A timer for the editor: a talk of a stated length, counting down, so the
/// pane is sized against the longest thing the mode line ever says.
pub static TIMER: std::sync::LazyLock<crate::widgets::TimerControls> =
    std::sync::LazyLock::new(|| crate::widgets::TimerControls::new(Some(20), true));

/// A slide count that reads like a real deck.
pub const SLIDE_COUNT: usize = 42;
pub const SLIDE: usize = 17;
pub const TITLE: &str = "Dependable Projectors — A Field Guide";

/// A transport for the editor, showing a clip mid-play.
///
/// Realistic rather than empty: a designer sizing this pane needs to see the
/// widest thing it will ever hold — a running time, a full scrub bar and a
/// sound button — not the "no media" line it shows on most slides.
pub fn transport() -> Option<crate::widgets::media::model::Transport> {
    Some(crate::widgets::media::model::Transport {
        action: crate::widgets::media::model::Action::Pause,
        position: 95.0,
        duration: Some(214.0),
        enabled: true,
        scrubbable: true,
        mutable: true,
        muted: false,
        readout: "1:35 / 3:34".to_string(),
    })
}
