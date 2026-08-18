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
        // Nothing to commit: the sample is a picture of a palette, not a
        // session, and there is no document behind it.
        let _ = annotations.end_stroke();
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

/// A reader with nothing open, for the editor and for a presenter layout.
///
/// Statics rather than constructors because the render context borrows them,
/// and the editor's context has to outlive the call that builds it.
#[allow(dead_code)] // see `closed_reader`
pub static EMPTY_COLUMN: std::sync::LazyLock<crate::widgets::document::model::Column> =
    std::sync::LazyLock::new(crate::widgets::document::model::Column::default);

#[allow(dead_code)] // see `closed_reader`
pub static READER_CONTROLS: std::sync::LazyLock<crate::widgets::document::model::ReaderControls> =
    std::sync::LazyLock::new(crate::widgets::document::model::ReaderControls::default);

/// The reader facet for a context with no document behind it.
///
/// Not a fake document: a reader widget in a presenter layout, or in the
/// editor, has nothing to show, and says so (§2) rather than drawing a sample
/// page that would be mistaken for the user's own.
#[allow(dead_code)] // used by the layout renderer.s own tests
pub fn closed_reader() -> crate::widgets::context::ReaderData<'static> {
    crate::widgets::context::ReaderData {
        open: false,
        page_count: 0,
        column: &EMPTY_COLUMN,
        viewport: 600.0,
        viewport_width: 800.0,
        visible: Vec::new(),
        date_picker: None,
        time_picker: None,
        focused_widget: None,
        focused_hint: None,
        choice_list: None,
        date_language: crate::datefield::Locale::default(),
        controls: &READER_CONTROLS,
        outline_reveal: 1.0,
        scale: 1.0,
        outline: &[],
        outline_focus: None,
        outline_scroll: 0.0,
        outline_viewport: std::rc::Rc::new(std::cell::Cell::new(600.0)),
        document_keyboard_focus: true,
        has_form: false,
        fields: &[],
        level: pulpit_render::document::CompatibilityLevel::AnnotateOnly,
        warnings: &[],
        dirty: false,
        page_entry: None,
        can_undo: false,
        can_go_back: false,
        can_go_forward: false,
        can_redo: false,
        selected: false,
        panning: false,
        composing: None,
    }
}

/// A search with something in it, so the editor can judge how much room the
/// pane wants rather than sizing it against an empty box.
///
/// A static for the same reason [`ANNOTATIONS`] is one: the render context
/// borrows it and has to outlive the call that builds it.
pub static SEARCH: std::sync::LazyLock<pulpit_core::search::SearchState> =
    std::sync::LazyLock::new(|| {
        use pulpit_core::page::PageIndex;
        use pulpit_core::search::{Hit, HitSource, Query, SearchState, TextMatch};

        let mut state = SearchState::new();
        state.open(SLIDE_COUNT);
        state.set_query(Query::new("reconnect", false, false));
        state.absorb((0..3).map(|index| {
            Hit::from_text(
                PageIndex(index * 4 + 1),
                if index == 1 {
                    HitSource::Notes
                } else {
                    HitSource::PageText
                },
                0,
                "…the reconnect comes back at a different index…",
                TextMatch { offset: 5, len: 9 },
                Vec::new(),
            )
        }));
        state
    });

/// A complete `Context` for tests that only need every widget to have
/// something to draw.
///
/// Several tests prove the same thing from different angles — that dispatch
/// reaches every kind, that a whole layout builds, that no arm panics on a
/// facet it was not given — and each of them needs every field of `Context`
/// filled in before it can ask. Filling them in is not what any of those
/// tests is about, so it happens once here. A test that cares about a
/// particular field should set that field on the value this returns, so the
/// condition it is proving stays visible at the test.
///
/// Frames are deliberately absent: a widget that needs a picture to build at
/// all is a widget that would fail in front of a room while one renders.
#[cfg(test)]
pub fn context(mode: crate::widgets::Mode) -> crate::widgets::context::Context<'static> {
    use crate::widgets::context::{
        AudienceData, Context, DocumentData, FrameSource, MediaData, SearchData, SlideData,
        TimingData,
    };

    struct NoFrames;
    impl FrameSource for NoFrames {
        fn frame(
            &self,
            _slide: usize,
            _kind: pulpit_render::cache::FrameKind,
            _max_width: u32,
        ) -> Option<iced::widget::image::Handle> {
            None
        }
    }

    static EMPTY_TEXT: std::sync::LazyLock<
        std::sync::Arc<std::collections::HashMap<u64, crate::typst_annotation::RenderedText>>,
    > = std::sync::LazyLock::new(|| std::sync::Arc::new(std::collections::HashMap::new()));

    Context {
        mode,
        search: SearchData {
            state: &SEARCH,
            input_focus: false,
            results_focus: false,
            scroll: 0.0,
            viewport: std::rc::Rc::new(std::cell::Cell::new(600.0)),
        },
        search_open: false,
        search_reveal: 0.0,
        slides: SlideData {
            current: SLIDE,
            preview: SLIDE,
            count: SLIDE_COUNT,
            frames: &NoFrames,
            preview_width: 640,
            aspect: 16.0 / 9.0,
            text_notes: None,
            has_links: false,
            link_highlights: Vec::new(),
            overlays: Vec::new(),
            crop: pulpit_core::notes::Region::FULL,
            annotations: &ANNOTATIONS,
            rendered_text: &EMPTY_TEXT,
            marks_cache: std::rc::Rc::new(iced::widget::canvas::Cache::new()),
            annotation_controls: crate::widgets::AnnotationControls::default(),
            annotation_style: pulpit_core::annotation::AnnotationStyle::default(),
        },
        alarms: &ALARMS,
        timer_controls: &TIMER,
        timing: TimingData {
            elapsed: std::time::Duration::from_secs(12 * 60),
            target: Some(std::time::Duration::from_secs(40 * 60)),
            running: true,
            seconds_of_day: 13 * 3600,
        },
        document: DocumentData {
            title: TITLE.to_string(),
            section: Some("Reconnection".to_string()),
            sample_notes: NOTES,
        },
        reader: closed_reader(),
        audience: AudienceData {
            blank: pulpit_core::Blank::Off,
            connected: true,
            fullscreen: true,
            started: true,
            menu_open: false,
        },
        media: MediaData {
            transport: transport(),
        },
    }
}
