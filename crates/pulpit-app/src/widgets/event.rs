//! What a live widget can ask for.
//!
//! Widgets emit these, never application messages: the view boundary
//! translates them into core commands. That is what keeps a widget testable
//! without an application and reusable in the editor, where the same controls
//! are drawn inert.

// Not `Copy`: the alarm field carries what has been typed, and text is not a
// thing to copy behind the caller's back.
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetEvent {
    /// Nothing at all.
    ///
    /// A control drawn inert still has to be *built*, and Iced widgets need
    /// somewhere to send what the pointer does to them. Mapping that away
    /// with `unreachable!` was a panic waiting for the first person to drag
    /// an inert slider in the editor; this is the message that means "the
    /// pointer moved and nothing should happen".
    Ignored,
    Next,
    Previous,
    ScrubTo(usize),
    CommitScrub,
    /// The pointer moved over the current-slide panel. Coordinates are
    /// normalised to the drawn slide content: `(0, 0)` is its top-left,
    /// `(1, 1)` its bottom-right; values outside that range mean the pointer
    /// is over the letterbox.
    SlideCursor {
        x: f32,
        y: f32,
    },
    /// The current-slide panel was pressed. The application hit-tests the
    /// last cursor position against the page's link annotations.
    SlidePressed,
    ToggleTimer,
    EndPresentation,
    /// Something the presenter asked of the annotation palette.
    Annotate(AnnotationCommand),
    /// Something the presenter asked of the media on the current slide.
    Transport(TransportRequest),
    /// Something the presenter asked of the clock's alarms.
    Alarm(AlarmCommand),
    /// Something the presenter asked of the timer itself.
    Timer(TimerCommand),
}

/// What the timer's menu can ask for.
///
/// The clock has alarms; the timer has a direction and a length, and the same
/// reasoning applies to both: they are set at the lectern, so they are set by
/// pressing rather than by typing.
#[derive(Debug, Clone, PartialEq)]
pub enum TimerCommand {
    /// Open or close the menu that sets the two below.
    Open(bool),
    /// Count down towards the target, or up from zero.
    SetCountDown(bool),
    /// Move the target length by whole minutes. Below a second is no target.
    NudgeTarget(i32),
    /// Set the target length outright, in seconds.
    SetTarget(u32),
    /// What has been typed into one half of the length field, as typed. The
    /// model decides what of it is a length.
    Type(TimeField, String),
    /// Take what is in the length field as the target.
    CommitLength,
    /// Run open-ended: no target, and therefore counting up.
    ClearTarget,
    /// Give the talk another snooze's worth of target.
    Snooze,
    /// Acknowledge the overrun and stop being offered anything about it.
    Dismiss,
    /// Change how long a snooze lasts, in whole minutes.
    NudgeSnooze(i32),
}

/// Which half of a two-field time picker a keystroke landed in.
///
/// The two pickers mean different units — hours and minutes on the clock,
/// minutes and seconds on the timer — but the same halves, so one word names
/// the position rather than two naming each unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeField {
    Left,
    Right,
}

/// What the clock's alarm popup can ask for.
///
/// Separate from [`WidgetEvent`] for the same reason [`AnnotationCommand`] is:
/// one widget's vocabulary is one thing for the application to map, rather
/// than half a dozen variants loose among the rest.
#[derive(Debug, Clone, PartialEq)]
pub enum AlarmCommand {
    /// Open or close the popup that edits the list.
    Open(bool),
    /// What has been typed into one half of the time picker, as typed. The
    /// model decides what of it is a time.
    Type(TimeField, String),
    /// Whether a typed hour of twelve or less means the afternoon.
    SetAfternoon(bool),
    /// Fill the field with a time this many seconds from now, for the cue that
    /// is set at the lectern: "I hand off in twenty minutes".
    DraftFromNow(u32),
    /// Set the drafted time.
    Add,
    Remove(u32),
    /// Change how long a snooze lasts, in whole minutes.
    NudgeSnooze(i32),
    /// Put the ringing cue off for a few minutes; it will ask again.
    Snooze,
    /// Answer the cue that is currently going off, for good.
    Dismiss,
}

/// What the media transport can ask for.
///
/// The widget names an intent; the coordinator decides what that means for a
/// clip as opposed to an animation, because only it knows which is on the
/// slide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportRequest {
    Play,
    Pause,
    /// Move the playhead, in seconds from the start.
    SeekTo(f32),
    SetMuted(bool),
}

/// What the annotation palette can ask for.
///
/// Separate from [`WidgetEvent`] so the palette's vocabulary is one thing the
/// application maps, rather than half a dozen variants scattered through the
/// widget vocabulary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnnotationCommand {
    /// Arm a tool, or hand the pointer back to links and media overlays.
    Arm(Option<pulpit_core::annotation::AnnotationTool>),
    /// Open or close the option palette anchored to a tool.
    OpenOptions(Option<pulpit_core::annotation::AnnotationTool>),
    /// Open or close the overflow menu, which holds whatever the palette was
    /// too narrow to draw.
    OpenOverflow(bool),
    /// Change the live size of one tool.
    SetSize(pulpit_core::annotation::AnnotationTool, f32),
    /// Change the live colour of ink or highlighting.
    SetColor(
        pulpit_core::annotation::AnnotationTool,
        pulpit_core::annotation::InkColor,
    ),
    /// Open or close the colour wheel that mixes a colour the palette does
    /// not offer, for one tool.
    OpenColorWheel(Option<pulpit_core::annotation::AnnotationTool>),
    /// Choose what the pointer control does: a dot, or a lit circle.
    SetPointerSpotlight(bool),
    /// Take back the most recent edit — a stroke drawn, or a sweep erased.
    Undo,
    /// Put back the most recently taken-back edit.
    Redo,
    /// Take away every mark on this slide.
    Clear,
    /// Show the marks on the audience screen, or stop showing them.
    ToggleAudience,
}
