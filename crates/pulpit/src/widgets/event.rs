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
    /// Something the reader asked of the open document.
    Read(ReadCommand),
    /// Something asked of the search pane.
    ///
    /// Its own vocabulary rather than a reader command: search is placed in a
    /// presenter layout and a document layout alike, and a presenter looking
    /// for a slide by what its notes say has no reader to send it to.
    Find(FindCommand),
    /// Something asked of the application's own chrome.
    Chrome(ChromeCommand),
}

/// What the search pane can ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindCommand {
    /// What is in the box, as typed. The model decides what of it is a query.
    Type(String),
    /// Move to the next or the previous hit, wrapping at either end.
    Next,
    Previous,
    /// Go to one particular hit: a press in the results list.
    Focus(usize),
    ToggleCaseSensitive,
    ToggleWholeWord,
    /// Forget the query and everything found for it.
    Clear,
}

/// What the menu button and the audience lifecycle controls can ask for.
///
/// These used to be a fixed strip above the layout and are now widgets, so
/// they speak the same vocabulary every other widget does: an intent, which
/// the application turns into a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeCommand {
    /// Open the main menu, or close it if the press was on an open one.
    ToggleMenu,
    /// Open the audience window where it last went.
    StartAudience,
    /// Close the audience window.
    StopAudience,
    /// Open the list of displays the audience window could go to instead.
    ToggleStartMenu,
}

/// What the reader's widgets can ask for.
///
/// One vocabulary for the whole family, for the same reason the palette has
/// one: five widgets over one document is one thing for the application to
/// map, not five sets of variants loose among the rest.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // see `crate::reader::ReaderSession`
pub enum ReadCommand {
    /// Scroll the page column to an offset in layout points.
    ScrollTo {
        /// Where the surface now is, in layout points from the top.
        offset: f32,
        /// …and from the left, which is only ever non-zero at a zoom that
        /// makes the page wider than the window.
        offset_x: f32,
        /// How tall the surface's window actually is.
        ///
        /// Reported rather than computed: the session fits pages to this, and
        /// a fit computed from the cell the layout *asked* for is out by
        /// whatever chrome sits between the cell and the scrollable — which
        /// is the difference between a page that fills the window exactly and
        /// one whose foot is cut off.
        viewport: f32,
    },
    /// The reader dragged the scroll handle to an offset.
    ///
    /// Told apart from [`ReadCommand::ScrollTo`], which is the surface saying
    /// where it already is: this one has to be pushed back to the surface,
    /// and that one must not be.
    DragScrollHandle(f32),
    /// Scroll by whole windows: what Page Down and Page Up do.
    ///
    /// In windows rather than in points because the reader means "the next
    /// screenful", and how much of the document that is depends on the zoom
    /// the window is at — which is the session's to know, not the key's.
    ScrollByWindows(i32),
    /// Put a page's top at the top of the window.
    GoToPage(pulpit_core::page::PageIndex),
    /// Fit the width, fit the page or set a scale.
    SetZoom(crate::widgets::document::model::Zoom),
    ZoomIn,
    ZoomOut,
    /// What has been typed into the page box, as typed. The model decides
    /// what of it is a page number.
    TypePage(String),
    /// Take what is in the page box as the page to go to.
    CommitPage,
    /// Read one page across the column, or two facing pages.
    SetSpread(crate::widgets::document::model::PageSpread),
    /// Show bookmarks or thumbnails in the outline rail.
    SetOutlineView(crate::widgets::document::model::OutlineView),
    /// Collapse the outline rail to its header, or open it again.
    SetOutlineCollapsed(bool),
    /// Arm a document tool, or hand the pointer back to the document's own
    /// links and form fields.
    Arm(Option<pulpit_core::annotation::AnnotationTool>),
    /// Open one tool's options popover in the toolbar, or close whichever is
    /// open.
    ToolOptions(Option<pulpit_core::annotation::AnnotationTool>),
    /// Set the colour a tool lays down.
    SetToolColor(
        pulpit_core::annotation::AnnotationTool,
        pulpit_core::annotation::InkColor,
    ),
    /// Set the pen's stroke width, in page points.
    SetInkWidth(f32),
    /// The pointer moved over the page surface, in canonical page points on
    /// the page it is over (A4).
    PageCursor {
        page: pulpit_core::page::PageIndex,
        x: f32,
        y: f32,
    },
    /// The page surface was double-clicked: open whatever text mark is under
    /// the pointer, if any (§8.5).
    PageDoubleClicked,
    /// The page surface was pressed, released, or the gesture was abandoned.
    PagePressed,
    PageReleased,
    PageCancelled,
    // Form filling has no command of its own on purpose (§8.6). Field values
    // are edited *in place on the page*, by PDFium's own form-fill
    // environment, which the page surface forwards raw input events to; the
    // application never draws a field editor and never sets a value itself.
    // That is what makes one editing surface rather than two, and removes the
    // whole class of bugs where an inspector and a widget disagree about what
    // a field holds. The events above are what carry the typing.
    /// What has been typed into the mark being written on the page (§8.5),
    /// as typed. The editor is drawn on the sheet at the spot the mark will
    /// land, so these come from the page surface and not from a dialog.
    ComposeMark(String),
    /// Typeset the text being written with Typst, or write it plain.
    ComposeAsTypst(bool),
    /// Place what was written, or abandon it. Empty text places nothing, and
    /// abandoning is not a mutation.
    CommitMark,
    CancelMark,
    /// Take the mark the reader has picked up out of the document (§8.4).
    ///
    /// The eraser's companion, not its replacement: a sweep takes whatever it
    /// passes over, and this takes exactly the one thing that is selected.
    DeleteSelected,
    /// Open what the selected mark says, for rewriting (§8.5). The same editor
    /// a double click opens, reached from the keyboard and from the toolbar.
    EditSelected,
    /// Put down whatever is held, committing nothing.
    ClearSelection,
    /// Set the size placed text is written at, in page points.
    SetTextSize(f32),
    /// Take back the last edit, or put it back.
    Undo,
    Redo,
    /// Write the annotated document somewhere else. Never over the source
    /// (A6), which is why there is no plain "Save".
    SaveAs,
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
    /// Write a copy of the deck with every slide's marks drawn into it.
    Save,
    /// Show the marks on the audience screen, or stop showing them.
    ToggleAudience,
}
