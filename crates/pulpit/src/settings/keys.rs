//! Fixed keyboard shortcuts and presenter-remote fallbacks.
//!
//! Keyboard shortcuts are intentionally not user-configurable yet. Keeping a
//! single curated map lets the interface teach the same small vocabulary in
//! menus, on the landing page, and in the complete reference. Presenter
//! remotes are recognised separately so their hardware aliases never crowd
//! that reference.

use serde::{Deserialize, Serialize};

/// Everything an input source can trigger. This is deliberately a superset of
/// [`pulpit_core::Command`]: it also covers window and display actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Next,
    Previous,
    First,
    Last,
    /// Unattended page turning: start it, or stop it. Held loops resume
    /// from the same key, because "the loop is off" and "the loop is
    /// waiting for me to stop touching it" are the same thing to the hand
    /// reaching for the key.
    ToggleAutoadvance,
    PreviewNext,
    PreviewPrevious,
    CommitPreview,
    CancelPreview,
    /// Blank in whichever colour the venue setting names. This is the only
    /// blanking key: pressing it again brings the deck back, and which colour
    /// it produces is a property of the room rather than of the deck.
    ///
    /// The old `blank-black` binding loads as this one: a stored keymap must
    /// keep working, and black is the default colour, so the behaviour a
    /// presenter had is the behaviour they keep. `blank-white` and
    /// `blank-alternate` named the retired second key; a keymap holding
    /// either loses that binding rather than failing to parse — see
    /// [`Keymap`].
    #[serde(alias = "blank-black")]
    Blank,
    ToggleTimer,
    ResetTimer,
    /// Read the whole document aloud, or pause it (issue #20).
    ///
    /// One key for start and pause, because that is what a reader expects of
    /// one control, and because the alternative — hunting for a second key
    /// while a voice talks over you — is exactly the moment when hunting is
    /// hardest. Its partner is [`Action::SpeakPageToggle`]: same behaviour,
    /// smaller scope.
    SpeakToggle,
    /// Read this page aloud — or, when text is selected, just the selection —
    /// or pause it.
    ///
    /// Toggling is per scope: pressing this while the *document* is being
    /// read starts reading the page, rather than pausing something else. A
    /// key that does a different thing depending on hidden state is worse
    /// than no key. The selection is not hidden state — it is lit up on the
    /// page — so the one key narrows to it when there is one, rather than a
    /// third key nobody would find.
    SpeakPageToggle,
    /// Stop reading and forget the place.
    SpeakStop,
    SpeakNextSentence,
    SpeakPreviousSentence,
    SwapDisplays,
    ToggleAudienceFullscreen,
    OpenDocument,
    ReloadDocument,
    /// Open the print dialog. Ctrl+P everywhere, which is the one shortcut a
    /// reader will try before looking for a menu.
    Print,
    /// The whole deck as thumbnails, to jump by eye rather than by number.
    ShowOverview,
    /// Mount the next usable layout in library order, wrapping at the end.
    CycleLayout,
    /// The layout library, without going through the menu first.
    ///
    /// The presenter screen *is* the layout, so changing it is a first-class
    /// thing to do at the lectern rather than a setting to go looking for.
    ShowLayouts,
    /// Show the live keyboard reference.
    ShowShortcuts,
    // Annotations. Arming a tool is a toggle: the same key puts it down
    // again, so the presenter never has to find an "off" control while the
    // room is watching.
    AnnotateSelect,
    AnnotateInk,
    AnnotateHighlighter,
    AnnotateText,
    AnnotateNote,
    AnnotateEraser,
    /// The dot, or the lit circle: which of the two depends on the mode the
    /// pointer control is in, so one key covers both — which is also why a
    /// keymap that names the spotlight still resolves here.
    #[serde(alias = "annotate-spotlight")]
    AnnotatePointer,
    /// The select-text tool: sweep the page's own text and hold it, for the
    /// clipboard and for reading aloud, without marking anything.
    AnnotateSelectText,
    /// Put the held text selection on the clipboard.
    CopySelection,
    UndoAnnotation,
    RedoAnnotation,
    ClearAnnotations,
    ToggleAnnotationAudience,
    /// Step keyboard focus through the current slide's links, so a deck's
    /// internal navigation is reachable without a pointer.
    FocusNextLink,
    FocusPreviousLink,
    /// Move the preview a tenth of the deck at a time. Iced only gives the
    /// slider its arrow keys while the pointer rests on it, which is no use
    /// to someone scrubbing a long deck from a lectern.
    /// Move between reading the document and presenting it.
    ///
    /// Mode is which layout is mounted, not which document is loaded (§2.3 of
    /// `SPEC-document.md`): the document stays open, the revision is
    /// unchanged, and each mode comes back to the layout it was last in.
    ToggleReader,
    /// Collapse the outline rail to its header, or open it again. Nothing is
    /// mounted or moved: a layout without an outline pane has no rail to
    /// collapse, and the key does nothing rather than rearranging the screen.
    ToggleOutline,
    /// Put the caret in the search box, wherever the pane is placed.
    FocusSearch,
    /// The next and the previous match, without leaving the page.
    FindNext,
    FindPrevious,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    FitPage,
    FitWidth,
    RotateReader,
    ToggleDualPage,
    Quit,
}

/// A semantic section in the complete shortcut reference.
#[derive(Debug, Clone, Copy)]
pub struct ShortcutGroup {
    pub title: &'static str,
    pub actions: &'static [Action],
}

/// The information architecture of the complete shortcut reference.
///
/// The grouping is semantic, not balanced by item count. Actions deliberately
/// left without a keyboard shortcut are absent because there is nothing a
/// user can press for them while shortcut customisation is unavailable.
pub const SHORTCUT_GROUPS: [ShortcutGroup; 7] = [
    ShortcutGroup {
        title: "Files & application",
        actions: &[
            Action::OpenDocument,
            Action::ReloadDocument,
            Action::Print,
            Action::CycleLayout,
            Action::ShowLayouts,
            Action::ShowShortcuts,
            Action::Quit,
        ],
    },
    ShortcutGroup {
        title: "Move through pages",
        actions: &[
            Action::Next,
            Action::Previous,
            Action::First,
            Action::Last,
            Action::ShowOverview,
            Action::ToggleAutoadvance,
        ],
    },
    ShortcutGroup {
        title: "Present",
        actions: &[
            Action::Blank,
            Action::ToggleTimer,
            Action::ResetTimer,
            Action::SwapDisplays,
            Action::ToggleAudienceFullscreen,
        ],
    },
    ShortcutGroup {
        title: "Read aloud",
        // `SpeakStop` is deliberately absent: it has no key, because "r"
        // already pauses and both shifted forms are taken. It is reachable
        // from the menu, and this reference only publishes rows a reader can
        // actually press something for.
        actions: &[
            Action::SpeakToggle,
            Action::SpeakPageToggle,
            Action::SpeakNextSentence,
            Action::SpeakPreviousSentence,
        ],
    },
    ShortcutGroup {
        title: "Annotate",
        actions: &[
            Action::AnnotateSelect,
            Action::AnnotateInk,
            Action::AnnotateHighlighter,
            Action::AnnotateText,
            Action::AnnotateNote,
            Action::AnnotateEraser,
            Action::AnnotatePointer,
            Action::AnnotateSelectText,
            Action::CopySelection,
            Action::UndoAnnotation,
            Action::RedoAnnotation,
            Action::ClearAnnotations,
            Action::ToggleAnnotationAudience,
        ],
    },
    ShortcutGroup {
        title: "Read & search",
        actions: &[
            Action::ToggleOutline,
            Action::FocusSearch,
            Action::FindNext,
            Action::FindPrevious,
        ],
    },
    ShortcutGroup {
        title: "Page view",
        actions: &[
            Action::ZoomIn,
            Action::ZoomOut,
            Action::ZoomReset,
            Action::FitPage,
            Action::FitWidth,
            Action::RotateReader,
            Action::ToggleDualPage,
        ],
    },
];

/// The compact reference shown before a document is open.
pub const QUICK_START_ACTIONS: [Action; 7] = [
    Action::Next,
    Action::Previous,
    Action::First,
    Action::Last,
    Action::FocusSearch,
    Action::ShowOverview,
    Action::ShowShortcuts,
];

/// Keys worth teaching specifically for live presentation.
pub const PRESENTING_ACTIONS: [Action; 2] = [Action::Blank, Action::ToggleTimer];

impl Action {
    /// Every action, so a keymap can be checked against the whole set.
    pub const ALL: [Action; 49] = [
        Action::Next,
        Action::Previous,
        Action::First,
        Action::Last,
        Action::ToggleAutoadvance,
        Action::PreviewNext,
        Action::PreviewPrevious,
        Action::CommitPreview,
        Action::CancelPreview,
        Action::Blank,
        Action::ToggleTimer,
        Action::ResetTimer,
        Action::SwapDisplays,
        Action::ToggleAudienceFullscreen,
        Action::OpenDocument,
        Action::ReloadDocument,
        Action::Print,
        Action::ShowOverview,
        Action::CycleLayout,
        Action::ShowLayouts,
        Action::ShowShortcuts,
        Action::AnnotateSelect,
        Action::AnnotateInk,
        Action::AnnotateHighlighter,
        Action::AnnotateText,
        Action::AnnotateNote,
        Action::AnnotateEraser,
        Action::AnnotatePointer,
        Action::AnnotateSelectText,
        Action::CopySelection,
        Action::UndoAnnotation,
        Action::RedoAnnotation,
        Action::ClearAnnotations,
        Action::ToggleAnnotationAudience,
        Action::FocusNextLink,
        Action::FocusPreviousLink,
        Action::ToggleReader,
        Action::ToggleOutline,
        Action::FocusSearch,
        Action::FindNext,
        Action::FindPrevious,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomReset,
        Action::FitPage,
        Action::FitWidth,
        Action::RotateReader,
        Action::ToggleDualPage,
        Action::Quit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::Next => "Next page",
            Action::Previous => "Previous page",
            Action::First => "First page",
            Action::Last => "Last page",
            Action::ToggleAutoadvance => "Autoadvance",
            Action::PreviewNext => "Preview next",
            Action::PreviewPrevious => "Preview previous",
            Action::CommitPreview => "Show the previewed page",
            Action::CancelPreview => "Cancel preview",
            Action::Blank => "Blank",
            Action::ToggleTimer => "Start/pause timer",
            Action::ResetTimer => "Reset timer",
            Action::SwapDisplays => "Swap displays",
            Action::ToggleAudienceFullscreen => "Audience fullscreen",
            Action::OpenDocument => "Open…",
            Action::ReloadDocument => "Reload document",
            Action::Print => "Print…",
            Action::ShowOverview => "Page overview",
            Action::CycleLayout => "Next layout",
            Action::ShowLayouts => "Layouts",
            Action::ShowShortcuts => "Keyboard shortcuts",
            Action::SpeakToggle => "Read the document aloud / pause",
            Action::SpeakPageToggle => "Read page or selection aloud / pause",
            Action::SpeakStop => "Stop reading",
            Action::SpeakNextSentence => "Next sentence",
            Action::SpeakPreviousSentence => "Previous sentence",
            Action::AnnotateSelect => "Hold marks with a rubber band",
            Action::AnnotateInk => "Draw on the page",
            Action::AnnotateHighlighter => "Highlight on the page",
            Action::AnnotateText => "Write on the page",
            Action::AnnotateNote => "Leave a note on the page",
            Action::AnnotateEraser => "Erase annotations",
            Action::AnnotatePointer => "Point at the page",
            Action::AnnotateSelectText => "Select text on the page",
            Action::CopySelection => "Copy the selected text",
            Action::UndoAnnotation => "Undo the last stroke",
            Action::RedoAnnotation => "Redo the last stroke",
            Action::ClearAnnotations => "Clear annotations",
            Action::ToggleAnnotationAudience => "Show annotations to the audience",
            Action::FocusNextLink => "Focus the next link",
            Action::FocusPreviousLink => "Focus the previous link",
            Action::ToggleReader => "Read or present",
            Action::ToggleOutline => "Show or hide the outline",
            Action::FocusSearch => "Search",
            Action::FindNext => "Next match",
            Action::FindPrevious => "Previous match",
            Action::ZoomIn => "Zoom in",
            Action::ZoomOut => "Zoom out",
            Action::ZoomReset => "Actual size",
            Action::FitPage => "Fit page",
            Action::FitWidth => "Fit width",
            Action::RotateReader => "Rotate pages",
            Action::ToggleDualPage => "Toggle two-page view",
            Action::Quit => "Quit",
        }
    }

    /// Does this action's shortcut still fire while a widget owns the
    /// keyboard?
    ///
    /// A text box that has the caret owns nearly every key — that is what
    /// typing is. The two sidebar selectors are the standing exception:
    /// Ctrl+B must reach the outline and Ctrl+F must reach — and close —
    /// the search from inside the search box itself, or the key that opened
    /// the pane could never toggle it. This is a property of the action, not
    /// a special case at the dispatch site, so the next such shortcut is a
    /// one-line change here. It only ever applies to a press whose modifiers
    /// [`Mods::commands`]: a bare letter in a text box is a letter.
    pub fn reaches_captured(self) -> bool {
        matches!(self, Action::ToggleOutline | Action::FocusSearch)
    }
}

/// The modifiers held down with a key.
///
/// These are data rather than a prefix glued onto the key name. The old
/// `"CtrlShiftZ"` spelling could not express modifiers structurally, and — worse
/// — it gave resolution no way to tell a modifier that is load-bearing from
/// one that is incidental. See [`Keymap::resolve_with_mods`].
///
/// Modifiers are named by role, not by key cap, exactly as
/// `crate::platform::input::Modifier` names them: `primary` is the modifier
/// this desktop commands applications with — Command on macOS, Control
/// elsewhere — and `control` is the Control key specifically, on the one
/// platform where that is a different key. The event's raw flags are folded
/// into these two by `InputPolicy::split_modifiers` before they reach the
/// keymap, so a binding written once means the conventional thing on every
/// desktop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Mods {
    /// A stored keymap from before the rename spells this field `ctrl`. It
    /// always meant "the modifier applications command with" — resolution
    /// accepted Command as well as Control — so it loads as `primary`
    /// unchanged.
    #[serde(alias = "ctrl")]
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    /// The Control key itself, where it is not the primary modifier. Always
    /// false on Windows and Linux: one press must never count as both.
    pub control: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        primary: false,
        shift: false,
        alt: false,
        control: false,
    };

    pub fn primary() -> Self {
        Mods {
            primary: true,
            ..Mods::NONE
        }
    }

    pub fn shift() -> Self {
        Mods {
            shift: true,
            ..Mods::NONE
        }
    }

    pub fn primary_shift() -> Self {
        Mods {
            primary: true,
            shift: true,
            ..Mods::NONE
        }
    }

    /// A press that is a command, not typing: at least one non-shift
    /// modifier is down. Only these may reach a binding while a widget owns
    /// the keyboard — a bare or merely shifted key inside a text box is a
    /// character being written, never a shortcut.
    pub fn commands(self) -> bool {
        self.primary || self.alt || self.control
    }

    fn prefix(self) -> String {
        let mut out = String::new();
        if self.primary {
            out.push_str("Ctrl+");
        }
        if self.control {
            out.push_str("Control+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        out
    }
}

/// How a key is recognised.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum KeyBinding {
    /// A named logical key: `"Right"`, `"space"`, `"F5"`, `"MediaPlayPause"`.
    Named {
        key: String,
        #[serde(default)]
        mods: Mods,
    },
    /// A raw physical scancode, for remotes whose keys the toolkit reports as
    /// unidentified. This is the documented fallback path, not an oddity.
    Scancode {
        code: u32,
        #[serde(default)]
        mods: Mods,
    },
}

impl KeyBinding {
    pub fn named(key: &str) -> Self {
        KeyBinding::Named {
            key: key.to_string(),
            mods: Mods::NONE,
        }
    }

    pub fn named_with(key: &str, mods: Mods) -> Self {
        KeyBinding::Named {
            key: key.to_string(),
            mods,
        }
    }

    pub fn scancode(code: u32) -> Self {
        KeyBinding::Scancode {
            code,
            mods: Mods::NONE,
        }
    }

    pub fn scancode_with(code: u32, mods: Mods) -> Self {
        KeyBinding::Scancode { code, mods }
    }

    pub fn mods(&self) -> Mods {
        match self {
            KeyBinding::Named { mods, .. } | KeyBinding::Scancode { mods, .. } => *mods,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            KeyBinding::Named { key, mods } => format!("{}{key}", mods.prefix()),
            KeyBinding::Scancode { code, mods } => {
                format!("{}scancode {code}", mods.prefix())
            }
        }
    }

    /// Fold a legacy `"Shift…"` / `"Ctrl…"` name prefix into the modifiers.
    ///
    /// A keymap written before modifiers were data spells redo as
    /// `{"kind":"named","key":"ShiftZ"}`. Left alone it would never match
    /// again, because the toolkit reports `Z` and the shift flag separately.
    fn normalized(self) -> Self {
        let KeyBinding::Named { key, mut mods } = self else {
            return self;
        };
        let mut rest = key.as_str();
        // Longest first, so "Shift" is not mistaken inside another name, and
        // repeated so "CtrlShiftZ" folds both.
        loop {
            let stripped = ["Ctrl", "Control", "Shift", "Alt"]
                .iter()
                .find_map(|prefix| rest.strip_prefix(prefix).map(|rest| (*prefix, rest)));
            // A bare modifier name is a key in its own right, not a prefix.
            match stripped {
                Some((prefix, tail)) if !tail.is_empty() => {
                    match prefix {
                        // The legacy spelling predates the macOS split, so
                        // it can only have meant the primary modifier.
                        "Ctrl" | "Control" => mods.primary = true,
                        "Shift" => mods.shift = true,
                        _ => mods.alt = true,
                    }
                    rest = tail;
                }
                _ => break,
            }
        }
        KeyBinding::Named {
            key: rest.to_string(),
            mods,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "KeymapWire")]
pub struct Keymap {
    pub bindings: Vec<(KeyBinding, Action)>,
}

/// The stored shape, so every binding is normalised on the way in and a
/// keymap written against the old `"ShiftZ"` spelling keeps working.
#[derive(Deserialize)]
struct KeymapWire {
    bindings: Vec<(KeyBinding, StoredAction)>,
}

/// An action name as it was stored, which may name an action that no longer
/// exists — `blank-white` and `blank-alternate` are the retired second
/// blanking key. A keymap is a presenter's own file: one stale name in it
/// costs that binding, never the whole file.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredAction {
    Known(Action),
    Retired(String),
}

impl From<KeymapWire> for Keymap {
    fn from(wire: KeymapWire) -> Self {
        let mut bindings: Vec<_> = wire
            .bindings
            .into_iter()
            .filter_map(|(binding, action)| match action {
                StoredAction::Known(action) => Some((binding.normalized(), action)),
                StoredAction::Retired(_) => None,
            })
            // Retired bindings go from stored keymaps too, or existing users
            // would keep a key that fresh installs stopped advertising.
            // Preview stepping is controlled by the scrubber rather than the
            // fixed keymap; reading and presenting are reached by cycling
            // layouts, which is the same act named once instead of twice.
            .filter(|(_, action)| {
                !matches!(
                    action,
                    Action::PreviewNext
                        | Action::PreviewPrevious
                        | Action::CommitPreview
                        | Action::CancelPreview
                        | Action::ToggleReader
                )
            })
            .collect();

        // These were shipped as defaults but could not do what their labels
        // promised: Iced reports the slash character as "/", not "slash",
        // while Ctrl+F is the conventional way to find. Move the old
        // fullscreen default to the bare `f` documented by the launcher and
        // carry existing keymaps forward with the corrected bindings.
        for (binding, action) in &mut bindings {
            if *action == Action::FocusSearch && *binding == KeyBinding::named("slash") {
                *binding = KeyBinding::named("/");
            }
            if *action == Action::ToggleAudienceFullscreen
                && *binding == KeyBinding::named_with("f", Mods::primary())
            {
                *binding = KeyBinding::named("f");
            }
        }
        let ctrl_f = KeyBinding::named_with("f", Mods::primary());
        if !bindings.iter().any(|(binding, _)| *binding == ctrl_f) {
            bindings.push((ctrl_f, Action::FocusSearch));
        }
        Keymap { bindings }
    }
}

impl Default for Keymap {
    fn default() -> Self {
        let named = |key: &str, action: Action| (KeyBinding::named(key), action);
        let with =
            |key: &str, mods: Mods, action: Action| (KeyBinding::named_with(key, mods), action);
        Self {
            bindings: vec![
                // Conventional reader keys are primary. The one familiar
                // Vim/Zathura alternative follows them and is shown in
                // parentheses in the reference. PageUp/PageDown remain
                // visible exceptions because many presenter remotes emit
                // them and many keyboards label them explicitly.
                named("Right", Action::Next),
                named("PageDown", Action::Next),
                named("j", Action::Next),
                named("Left", Action::Previous),
                named("PageUp", Action::Previous),
                named("k", Action::Previous),
                named("Home", Action::First),
                named("End", Action::Last),
                with("g", Mods::shift(), Action::Last),
                // "p" for play: the unattended loop, started and stopped from
                // one key. Bare rather than modified because the hand that
                // stops a running loop is usually reaching in a hurry, and
                // free because printing is Ctrl+P like everywhere else.
                named("p", Action::ToggleAutoadvance),
                // Blanking is the most reflexive key at a lectern, so it does
                // not move for anyone — `b` is a vim word motion, but a
                // presenter has no words to move over. It is the only
                // blanking key: pressing it again brings the deck back, and
                // the colour it blanks to is a setting rather than a second
                // key to hit by accident.
                named("b", Action::Blank),
                named("t", Action::ToggleTimer),
                with("t", Mods::shift(), Action::ResetTimer),
                named("s", Action::SwapDisplays),
                // "o" for overview: the deck at a glance, to land on a slide
                // by eye rather than by number. It used to be "j", which is
                // now a navigation key; "o" is free because opening a file is
                // `Ctrl+O` like everywhere else.
                named("o", Action::ShowOverview),
                // Cycling is the live layout action; the shifted form opens
                // the library when a specific layout is wanted.
                named("l", Action::CycleLayout),
                with("l", Mods::shift(), Action::ShowLayouts),
                with("/", Mods::shift(), Action::ShowShortcuts),
                // Speech: "r" for read. One key starts and pauses, which is
                // what a reader expects of one control, and hunting for a
                // second key while a voice talks over you is the worst moment
                // to be hunting.
                //
                // Deliberately *not* "p". Nothing in pulpit binds it — the
                // timer is "t" — but "p" means pause-the-talk in every other
                // presenter, pdfpc included, so a presenter reaching for it
                // expecting the clock to stop would instead have the slide
                // read out loud to the room. No conflict in the keymap;
                // a bad one in the hands. "v" and "s" are taken by the
                // annotation-audience toggle and display swap.
                //
                // Two scopes, one key each, both play/pause toggles: bare "r"
                // reads the whole document, shifted reads just this page.
                // Rotating pages moved to "Ctrl+Shift+R" to make room, which
                // keeps it an R-shaped key beside "Ctrl+R" (reload).
                //
                // Stop has no key of its own: both keys already pause, and
                // stopping outright is rare enough to live in the menu.
                named("r", Action::SpeakToggle),
                with("r", Mods::shift(), Action::SpeakPageToggle),
                with("Right", Mods::shift(), Action::SpeakNextSentence),
                with("Left", Mods::shift(), Action::SpeakPreviousSentence),
                // The tools sit under the digits, in the order the palette
                // draws them — which is the order document mode's own digits
                // arm them in, so a digit means the same tool in both modes.
                // The marks they make are cleared and taken back from the keys
                // beside them.
                named("1", Action::AnnotateSelect),
                named("2", Action::AnnotateInk),
                named("3", Action::AnnotateHighlighter),
                named("4", Action::AnnotateText),
                named("5", Action::AnnotateNote),
                named("6", Action::AnnotateEraser),
                // The pointer has no document-mode counterpart to line up
                // with, so it takes the digit after them.
                named("7", Action::AnnotatePointer),
                // The text selection takes the digit after the pointer's, in
                // both modes: 7 stays the pointer's even though document mode
                // has no pointer, so that 8 means "select text" everywhere.
                named("8", Action::AnnotateSelectText),
                // The one clipboard chord everybody will try. It reaches the
                // held text selection; the band's copies commit on release
                // and need no key.
                with("c", Mods::primary(), Action::CopySelection),
                // Editing convention first, then the familiar Vim undo.
                with("z", Mods::primary(), Action::UndoAnnotation),
                named("u", Action::UndoAnnotation),
                with("z", Mods::primary_shift(), Action::RedoAnnotation),
                named("c", Action::ClearAnnotations),
                named("v", Action::ToggleAnnotationAudience),
                // Anything that leaves the deck — opening, reloading, quitting,
                // resizing the audience window — takes the primary modifier,
                // as it does in every other application. Quit especially: a
                // bare "q" next to "w" is a talk ended by a typo.
                with("o", Mods::primary(), Action::OpenDocument),
                with("p", Mods::primary(), Action::Print),
                // Ctrl+F and "/" find; F3 and Shift+F3 step through matches.
                // Iced reports the slash character as "/", so the binding
                // must use the character rather than the key-cap name.
                // Ctrl+B for the side rail, as every editor and reader with a
                // sidebar has it. Bare "b" is not free — and would be a
                // blanked screen in the middle of a talk if it were taken.
                with("b", Mods::primary(), Action::ToggleOutline),
                with("f", Mods::primary(), Action::FocusSearch),
                named("/", Action::FocusSearch),
                named("f3", Action::FindNext),
                with("f3", Mods::shift(), Action::FindPrevious),
                // Zathura/Vim match traversal. Lowercase advances and the
                // shifted form walks back through the same live result set.
                named("n", Action::FindNext),
                with("n", Mods::shift(), Action::FindPrevious),
                // Reader transforms keep both Acrobat and Zathura muscle
                // memory. One semantic action may have several bindings; a
                // physical combination still resolves to only one action.
                named("+", Action::ZoomIn),
                with("=", Mods::primary(), Action::ZoomIn),
                named("-", Action::ZoomOut),
                with("-", Mods::primary(), Action::ZoomOut),
                named("=", Action::ZoomReset),
                with("1", Mods::primary(), Action::ZoomReset),
                named("a", Action::FitPage),
                with("0", Mods::primary(), Action::FitPage),
                with("2", Mods::primary(), Action::FitWidth),
                // Moved off "Shift+R", which now reads this page aloud. Still
                // an R-shaped key, and beside the primary chord that reloads;
                // primary-with-shift is the row redo already lives on.
                with("r", Mods::primary_shift(), Action::RotateReader),
                named("d", Action::ToggleDualPage),
                with("r", Mods::primary(), Action::ReloadDocument),
                named("f", Action::ToggleAudienceFullscreen),
                with("q", Mods::primary(), Action::Quit),
                // Link focus has no fixed shortcut. It remains an internal
                // action for pointer/focus routing, not a key the reference
                // can honestly advertise.
            ],
        }
    }
}

impl Keymap {
    /// Resolve a logical key name, then a raw scancode. Logical wins so a
    /// remap of a named key is never shadowed by a stale scancode entry.
    pub fn resolve(&self, key: Option<&str>, scancode: Option<u32>) -> Option<Action> {
        self.resolve_with_mods(key, Mods::NONE, scancode)
    }

    /// Resolve a keypress held with `mods`.
    ///
    /// Two passes. The first wants the modifiers to match exactly, so
    /// `Ctrl`+`Q` finds quit and `Shift`+`F3` finds the previous match.
    /// The second allows an *unmatched shift only*: a presenter resting a
    /// finger on shift must still be able to blank the screen with `b`.
    ///
    /// `Primary`, `Control` and `Alt` never fall back. They are pressed
    /// deliberately, so `Ctrl`+`Q` must never reach a binding for a bare `q`
    /// — which is the whole reason modifiers are data here and not a name
    /// prefix.
    pub fn resolve_with_mods(
        &self,
        key: Option<&str>,
        mods: Mods,
        scancode: Option<u32>,
    ) -> Option<Action> {
        // Exact, then shift-lenient; named keys before scancodes within each
        // pass, so a remapped name is never shadowed by a stale scancode.
        for lenient in [false, true] {
            if lenient && !mods.shift {
                continue;
            }
            let matches = |bound: Mods| {
                bound.primary == mods.primary
                    && bound.control == mods.control
                    && bound.alt == mods.alt
                    && if lenient {
                        !bound.shift
                    } else {
                        bound.shift == mods.shift
                    }
            };
            if let Some(key) = key {
                if let Some(action) =
                    self.bindings
                        .iter()
                        .find_map(|(binding, action)| match binding {
                            KeyBinding::Named {
                                key: named,
                                mods: bound,
                            } if named.eq_ignore_ascii_case(key) && matches(*bound) => {
                                Some(*action)
                            }
                            _ => None,
                        })
                {
                    return Some(action);
                }
            }
            if let Some(scancode) = scancode {
                if let Some(action) =
                    self.bindings
                        .iter()
                        .find_map(|(binding, action)| match binding {
                            KeyBinding::Scancode { code, mods: bound }
                                if *code == scancode && matches(*bound) =>
                            {
                                Some(*action)
                            }
                            _ => None,
                        })
                {
                    return Some(action);
                }
            }
        }
        None
    }

    pub fn bind(&mut self, binding: KeyBinding, action: Action) {
        self.bindings.retain(|(existing, _)| existing != &binding);
        self.bindings.push((binding, action));
    }

    pub fn unbind(&mut self, binding: &KeyBinding) {
        self.bindings.retain(|(existing, _)| existing != binding);
    }

    /// Give any action with no key at all its default one.
    ///
    /// The keymap is stored in full, so a settings file written before an
    /// action existed pins the old list and the new default never appears —
    /// which is how a shortcut can be in the source, in the documentation and
    /// in the menu, and still do nothing on the machine of anyone who has
    /// ever saved a setting.
    ///
    /// Only actions with *nothing* bound are touched, and the key is only
    /// taken if it is still free, so a deliberate remapping is never undone
    /// and a borrowed key is never stolen back.
    pub fn restore_missing_defaults(&mut self) {
        let defaults = Keymap::default();
        for action in Action::ALL {
            if self.bindings.iter().any(|(_, bound)| *bound == action) {
                continue;
            }
            for (binding, default_action) in &defaults.bindings {
                if *default_action != action {
                    continue;
                }
                let taken = self
                    .bindings
                    .iter()
                    .any(|(existing, _)| existing == binding);
                if !taken {
                    self.bindings.push((binding.clone(), action));
                }
            }
        }
    }

    /// Actions deliberately left unbound by default.
    ///
    /// Reading and presenting are which layout is mounted, so the layout keys
    /// are how a file moves between them: `l` cycles and `Shift+L` opens the
    /// library, and a second key doing the same thing to a bare letter is one
    /// vocabulary too many. Preview control belongs to the scrubber rather
    /// than Tab/Enter, and stepping through a slide's links is rare enough
    /// that it does not earn one of the bare letters. The variants remain
    /// internal actions, but no fixed shortcut or reference entry advertises
    /// them.
    pub const UNBOUND_BY_DEFAULT: [Action; 7] = [
        Action::ToggleReader,
        Action::PreviewNext,
        Action::PreviewPrevious,
        Action::CommitPreview,
        Action::CancelPreview,
        Action::FocusNextLink,
        Action::FocusPreviousLink,
    ];

    /// The binding to *show* for an action, if any.
    ///
    /// Menu labels quote one key, not the whole list. Prefer the conventional
    /// binding even where the resolver's table happens to list an alternate
    /// first, then fall back to any readable named binding.
    pub fn display_binding(&self, action: Action) -> Option<&KeyBinding> {
        self.bindings
            .iter()
            .find(|(binding, bound)| {
                *bound == action
                    && matches!(binding, KeyBinding::Named { .. })
                    && !Self::is_alternate(action, binding)
            })
            .or_else(|| {
                self.bindings.iter().find(|(binding, bound)| {
                    *bound == action && matches!(binding, KeyBinding::Named { .. })
                })
            })
            .map(|(binding, _)| binding)
    }

    pub fn keys_for(&self, action: Action) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|(_, bound)| *bound == action)
            .map(|(binding, _)| binding.describe())
            .collect()
    }

    /// Whether a visible binding is the Vim/Zathura alternative shown in
    /// parentheses rather than part of the primary keycap.
    pub fn is_alternate(action: Action, binding: &KeyBinding) -> bool {
        let KeyBinding::Named { key, mods } = binding else {
            return false;
        };
        match action {
            Action::Next => key.eq_ignore_ascii_case("j") && *mods == Mods::NONE,
            Action::Previous => key.eq_ignore_ascii_case("k") && *mods == Mods::NONE,
            Action::Last => key.eq_ignore_ascii_case("g") && *mods == Mods::shift(),
            Action::FocusSearch => key == "/" && *mods == Mods::NONE,
            Action::FindNext => key.eq_ignore_ascii_case("n") && *mods == Mods::NONE,
            Action::FindPrevious => key.eq_ignore_ascii_case("n") && *mods == Mods::shift(),
            Action::ZoomIn => key == "+" && *mods == Mods::NONE,
            Action::ZoomOut => key == "-" && *mods == Mods::NONE,
            Action::ZoomReset => key == "=" && *mods == Mods::NONE,
            Action::FitPage => key.eq_ignore_ascii_case("a") && *mods == Mods::NONE,
            Action::UndoAnnotation => key.eq_ignore_ascii_case("u") && *mods == Mods::NONE,
            _ => false,
        }
    }

    /// Fixed aliases emitted by common presenter remotes.
    ///
    /// They intentionally live outside `bindings`: hardware names should
    /// work without becoming nine noisy keycaps in the keyboard reference.
    pub fn resolve_remote(key: Option<&str>, mods: Mods) -> Option<Action> {
        if mods != Mods::NONE {
            return None;
        }
        match key? {
            key if key.eq_ignore_ascii_case("F1") => Some(Action::Blank),
            key if key.eq_ignore_ascii_case("MediaPlayPause") => Some(Action::Blank),
            key if key.eq_ignore_ascii_case("MediaTrackNext")
                || key.eq_ignore_ascii_case("BrowserForward")
                || key.eq_ignore_ascii_case("AudioVolumeUp") =>
            {
                Some(Action::Next)
            }
            key if key.eq_ignore_ascii_case("MediaTrackPrevious")
                || key.eq_ignore_ascii_case("BrowserBack")
                || key.eq_ignore_ascii_case("AudioVolumeDown") =>
            {
                Some(Action::Previous)
            }
            _ => None,
        }
    }
}

/// The modifiers of a binding, in the order a shortcut is written in.
///
/// The order is not cosmetic: [`crate::platform::Shortcut`] is compared and
/// formatted as a sequence, so two spellings of the same chord would be two
/// different shortcuts.
pub fn modifiers_of(mods: &Mods) -> Vec<crate::platform::input::Modifier> {
    use crate::platform::input::Modifier;

    let mut modifiers = Vec::new();
    if mods.primary {
        modifiers.push(Modifier::Primary);
    }
    if mods.control {
        modifiers.push(Modifier::Control);
    }
    if mods.alt {
        modifiers.push(Modifier::Alt);
    }
    if mods.shift {
        modifiers.push(Modifier::Shift);
    }
    modifiers
}

/// A keymap key name as an interface should print it. Letters are stored as
/// the toolkit reports them, in lower case, and the function keys
/// inconsistently so; a key cap is upper case either way.
///
/// The menus, the in-application reference and the website's table all spell
/// a key through this one function, so none of them can drift from another.
pub fn display_key(key: &str) -> String {
    match key {
        "slash" => "/".into(),
        "Right" => "→".into(),
        "Left" => "←".into(),
        "PageDown" => "PgDn".into(),
        "PageUp" => "PgUp".into(),
        other if other.len() == 1 => other.to_ascii_uppercase(),
        other if other.len() <= 3 && other.starts_with(['f', 'F']) => other.to_ascii_uppercase(),
        other => other.to_string(),
    }
}

/// The website's keyboard reference, generated from this file.
///
/// The table on the site was typed by hand for a while, which is a promise
/// nothing checks: a binding could change here and stay wrong there
/// indefinitely. It is rendered from [`SHORTCUT_GROUPS`] and
/// [`Keymap::default`] instead, and a test compares the rendering against
/// `docs-src/parts/keys.typ`, so a keymap change that is not carried into the
/// documentation fails the build rather than surviving it.
///
/// The spelling is deliberately platform-neutral — `Ctrl`, not whatever this
/// desktop's primary modifier happens to be — because one page is read from
/// every platform. That is the single difference from what the application
/// shows; `crate::platform`'s `InputPolicy` handles the rest there.
#[cfg(test)]
mod website {
    use super::*;

    /// Where the generated table lives, relative to this crate.
    const GENERATED: &str = "../../docs-src/parts/keys.typ";

    fn path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GENERATED)
    }

    /// A binding as the website prints it, or `None` for a raw scancode,
    /// which names nothing a reader could press deliberately.
    fn spell(binding: &KeyBinding) -> Option<String> {
        let KeyBinding::Named { key, mods } = binding else {
            return None;
        };
        Some(format!("{}{}", mods.prefix(), display_key(key)))
    }

    /// The keycap cell for one action: the primary bindings, then the
    /// Vim/Zathura alternative in parentheses, exactly as the application's
    /// own reference splits them.
    fn keys_cell(keymap: &Keymap, action: Action) -> String {
        let mut primary = Vec::new();
        let mut alternate = Vec::new();
        for (binding, bound) in &keymap.bindings {
            if *bound != action {
                continue;
            }
            let Some(spelled) = spell(binding) else {
                continue;
            };
            if Keymap::is_alternate(action, binding) {
                alternate.push(format!("`{spelled}`"));
            } else {
                primary.push(format!("`{spelled}`"));
            }
        }
        let mut cell = primary.join(" ");
        if !alternate.is_empty() {
            cell.push_str(&format!(" ({})", alternate.join(" ")));
        }
        cell
    }

    /// Two key/action pairs to a row, so the whole reference fits a screen
    /// rather than scrolling past one.
    fn render() -> String {
        let keymap = Keymap::default();
        let mut out = String::from(
            "// Generated from the application keymap. Do not edit.\n\
             //\n\
             // Regenerate with `make docs-keys` after changing\n\
             // `crates/pulpit/src/settings/keys.rs`.\n\
             \n\
             #table(\n  \
             columns: (auto, 1fr, auto, 1fr),\n  \
             stroke: none,\n  \
             inset: (x: 0.5em, y: 0.4em),\n",
        );
        for group in SHORTCUT_GROUPS {
            out.push_str(&format!(
                "  table.cell(colspan: 4)[#smallcaps[{}]],\n",
                group.title
            ));
            for pair in group.actions.chunks(2) {
                let mut cells = Vec::new();
                for action in pair {
                    cells.push(format!("[{}]", keys_cell(&keymap, *action)));
                    cells.push(format!("[{}]", action.label()));
                }
                // An odd group leaves the second pair empty rather than
                // pulling the next group's first entry up beside it.
                while cells.len() < 4 {
                    cells.push("[]".to_string());
                }
                out.push_str(&format!("  {},\n", cells.join(", ")));
            }
        }
        out.push_str(")\n");
        out
    }

    #[test]
    fn the_published_reference_is_the_keymap_itself() {
        let generated = render();
        if std::env::var_os("PULPIT_UPDATE_DOCS").is_some() {
            std::fs::write(path(), &generated).expect("write the generated table");
            return;
        }
        let published = std::fs::read_to_string(path()).unwrap_or_default();
        assert!(
            published == generated,
            "docs-src/parts/keys.typ no longer matches the keymap. \
             Run `make docs-keys` and commit the result.\n\n\
             --- expected ---\n{generated}\n--- published ---\n{published}"
        );
    }

    /// The reference is worth publishing only if every row names a key.
    #[test]
    fn every_published_row_has_a_key_to_press() {
        let keymap = Keymap::default();
        for group in SHORTCUT_GROUPS {
            for action in group.actions {
                assert!(!keys_cell(&keymap, *action).is_empty(), "{action:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_menu_entry_quotes_the_bound_key_not_a_remembered_one() {
        let mut keymap = Keymap::default();
        assert_eq!(
            keymap.display_binding(Action::ShowOverview),
            Some(&KeyBinding::named("o"))
        );
        keymap.unbind(&KeyBinding::named("o"));
        keymap.bind(
            KeyBinding::named_with("m", Mods::primary()),
            Action::ShowOverview,
        );
        assert_eq!(
            keymap.display_binding(Action::ShowOverview),
            Some(&KeyBinding::named_with("m", Mods::primary()))
        );
    }

    #[test]
    fn menus_prefer_the_conventional_binding_over_the_vim_alternative() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.display_binding(Action::ZoomIn),
            Some(&KeyBinding::named_with("=", Mods::primary()))
        );
        assert_eq!(
            keymap.display_binding(Action::FitPage),
            Some(&KeyBinding::named_with("0", Mods::primary()))
        );
    }

    #[test]
    fn every_action_has_at_least_one_default_binding() {
        let keymap = Keymap::default();
        for action in Action::ALL {
            if Keymap::UNBOUND_BY_DEFAULT.contains(&action) {
                continue;
            }
            assert!(
                !keymap.keys_for(action).is_empty(),
                "{action:?} is unreachable from the keyboard"
            );
        }
    }

    #[test]
    fn no_key_is_bound_to_two_different_actions() {
        // The guard that was missing. `r` was the reset-timer key and the
        // reader key at once, and because resolution takes the first match,
        // reader mode was unreachable — in the source, in the menu, and
        // nowhere on the keyboard.
        let keymap = Keymap::default();
        let mut seen: Vec<(&KeyBinding, Action)> = Vec::new();
        for (binding, action) in &keymap.bindings {
            if let Some((_, first)) = seen.iter().find(|(bound, _)| *bound == binding) {
                assert_eq!(
                    *first,
                    *action,
                    "{} is bound to both {first:?} and {action:?}; the second never fires",
                    binding.describe()
                );
            }
            seen.push((binding, *action));
        }
    }

    #[test]
    fn every_default_binding_resolves_to_the_action_it_was_written_for() {
        // Stronger than the collision check: it also catches a binding made
        // unreachable by the modifier rules rather than by a duplicate key.
        let keymap = Keymap::default();
        for (binding, action) in &keymap.bindings {
            let resolved = match binding {
                KeyBinding::Named { key, mods } => keymap.resolve_with_mods(Some(key), *mods, None),
                KeyBinding::Scancode { code, mods } => {
                    keymap.resolve_with_mods(None, *mods, Some(*code))
                }
            };
            assert_eq!(
                resolved,
                Some(*action),
                "{} was written for {action:?} but resolves to {resolved:?}",
                binding.describe()
            );
        }
    }

    #[test]
    fn every_annotation_action_is_reachable() {
        // The "and none is shared" half of this used to be checked here by
        // comparing key *names*, which cannot see a modifier. It is now
        // `no_key_is_bound_to_two_different_actions`, over the whole table
        // rather than over the annotation corner of it.
        let keymap = Keymap::default();
        let annotation = [
            Action::AnnotateSelect,
            Action::AnnotateInk,
            Action::AnnotateHighlighter,
            Action::AnnotateText,
            Action::AnnotateNote,
            Action::AnnotateEraser,
            Action::AnnotatePointer,
            Action::UndoAnnotation,
            Action::RedoAnnotation,
            Action::ClearAnnotations,
            Action::ToggleAnnotationAudience,
        ];
        for action in annotation {
            assert!(
                !keymap.keys_for(action).is_empty(),
                "{action:?} is unreachable"
            );
        }
    }

    #[test]
    fn keyboard_and_remote_aliases_are_separate() {
        let keymap = Keymap::default();
        for key in ["Right", "Left", "PageDown", "PageUp"] {
            assert!(keymap.resolve(Some(key), None).is_some(), "{key}");
        }
        for key in [
            "MediaTrackNext",
            "MediaTrackPrevious",
            "BrowserForward",
            "BrowserBack",
            "AudioVolumeUp",
            "AudioVolumeDown",
            "MediaPlayPause",
            "F1",
        ] {
            assert_eq!(
                keymap.resolve(Some(key), None),
                None,
                "{key} leaked into help"
            );
            assert!(
                Keymap::resolve_remote(Some(key), Mods::NONE).is_some(),
                "{key}"
            );
        }
    }

    #[test]
    fn unidentified_keys_fall_back_to_scancodes() {
        let mut keymap = Keymap::default();
        assert_eq!(keymap.resolve(None, Some(191)), None);
        keymap.bind(KeyBinding::scancode(191), Action::Next);
        assert_eq!(keymap.resolve(None, Some(191)), Some(Action::Next));
        assert_eq!(
            keymap.resolve(Some("Right"), Some(191)),
            Some(Action::Next),
            "a named key still resolves when a scancode is also present"
        );
    }

    #[test]
    fn rebinding_replaces_rather_than_duplicates() {
        let mut keymap = Keymap::default();
        keymap.bind(KeyBinding::named("Right"), Action::Previous);
        assert_eq!(keymap.resolve(Some("Right"), None), Some(Action::Previous));
        assert_eq!(
            keymap
                .bindings
                .iter()
                .filter(|(b, _)| *b == KeyBinding::named("Right"))
                .count(),
            1
        );
        keymap.unbind(&KeyBinding::named("Right"));
        assert_eq!(keymap.resolve(Some("Right"), None), None);
    }

    #[test]
    fn key_matching_is_case_insensitive() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("RIGHT"), None), Some(Action::Next));
        assert_eq!(keymap.resolve(Some("B"), None), Some(Action::Blank));
    }

    #[test]
    fn a_shifted_binding_is_reachable_at_all() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve_with_mods(Some("f3"), Mods::shift(), None),
            Some(Action::FindPrevious)
        );
    }

    #[test]
    fn an_unshifted_press_never_reaches_a_shifted_binding() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("f3"), None), Some(Action::FindNext));
        assert_eq!(keymap.resolve(Some("Right"), None), Some(Action::Next));
    }

    #[test]
    fn shift_falls_back_to_the_plain_binding_when_there_is_no_shifted_one() {
        // A presenter resting a finger on shift must still be able to blank
        // the screen.
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve_with_mods(Some("b"), Mods::shift(), None),
            Some(Action::Blank)
        );
    }

    #[test]
    fn ctrl_never_falls_back_to_the_bare_key() {
        // The reason modifiers are data rather than a name prefix. `Ctrl`+`Q`
        // quits; `Ctrl` held over a key that means something on its own must
        // do nothing at all, or every accelerator becomes a second way to
        // trigger a presenting action by accident.
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve_with_mods(Some("q"), Mods::primary(), None),
            Some(Action::Quit)
        );
        assert_eq!(keymap.resolve(Some("q"), None), None, "a bare q is inert");
        // Ctrl+B is the outline rail's own key. What matters here is that it
        // is *that* and not a blanked screen: the bare "b" beneath it must not
        // show through.
        assert_eq!(
            keymap.resolve_with_mods(Some("b"), Mods::primary(), None),
            Some(Action::ToggleOutline),
            "Ctrl+B must not blank the screen"
        );
        assert_eq!(
            keymap.resolve_with_mods(
                Some("j"),
                Mods {
                    alt: true,
                    ..Mods::NONE
                },
                None
            ),
            None,
            "Alt+J must not advance the slide"
        );
    }

    #[test]
    fn two_modifiers_at_once_are_expressible_and_distinct() {
        // Impossible under the old name-prefix scheme, and the reason redo
        // could not follow the convention every other editor uses.
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve_with_mods(Some("z"), Mods::primary(), None),
            Some(Action::UndoAnnotation)
        );
        assert_eq!(
            keymap.resolve_with_mods(Some("Z"), Mods::primary_shift(), None),
            Some(Action::RedoAnnotation)
        );
    }

    #[test]
    fn the_macos_control_key_is_not_the_primary_modifier() {
        // On macOS the Control key arrives as `control`, not `primary`, and
        // every default binding is written against `primary`. Ctrl+Q on a
        // Mac must therefore not quit — ⌘Q does. On Windows and Linux the
        // platform layer never sets `control`, so this shape cannot occur.
        let keymap = Keymap::default();
        let mac_ctrl = Mods {
            control: true,
            ..Mods::NONE
        };
        assert_eq!(keymap.resolve_with_mods(Some("q"), mac_ctrl, None), None);
        assert_eq!(keymap.resolve_with_mods(Some("f"), mac_ctrl, None), None);
    }

    #[test]
    fn a_stored_keymap_spelling_primary_as_ctrl_still_loads() {
        // The field was called `ctrl` before macOS Command and Control were
        // told apart. Every settings file written in that era says
        // `"ctrl":true` and must go on meaning the primary modifier.
        let mods: Mods = serde_json::from_str(r#"{"ctrl":true,"shift":true}"#)
            .expect("the legacy spelling parses");
        assert_eq!(
            mods,
            Mods {
                primary: true,
                shift: true,
                ..Mods::NONE
            }
        );
    }

    #[test]
    fn no_default_binding_is_reserved_by_a_desktop() {
        // `InputPolicy::is_reserved` knows what the desktop will not give
        // us — ⌘Q is macOS's, Alt+Tab is the window switcher's. A default
        // the desktop swallows is a documented key that does nothing, which
        // is worse than no key: this keeps the curated table honest on the
        // platform the build is for. (Quit *is* Ctrl+Q here and ⌘Q on
        // macOS, where the reservation and the binding agree in meaning; the
        // desktop delivering the press to us anyway is the app quitting,
        // which is what the key says.)
        use crate::platform::input::InputPolicy;
        let input = crate::platform::input::DesktopInput;
        let keymap = Keymap::default();
        for (binding, action) in &keymap.bindings {
            if *action == Action::Quit {
                continue;
            }
            let KeyBinding::Named { key, mods } = binding else {
                continue;
            };
            let modifiers = modifiers_of(mods);
            let shortcut = crate::platform::Shortcut {
                modifiers,
                key: key.clone(),
            };
            assert_eq!(
                input.is_reserved(&shortcut),
                None,
                "{} ({action:?}) is a key the desktop will swallow",
                binding.describe()
            );
        }
    }

    #[test]
    fn autoadvance_is_one_bare_key_in_both_directions() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve(Some("p"), None),
            Some(Action::ToggleAutoadvance),
            "starting and stopping are the same key"
        );
        // Printing keeps the modified form, which is the reason the bare
        // letter was free to take.
        assert_eq!(
            keymap.resolve_with_mods(Some("p"), Mods::primary(), None),
            Some(Action::Print),
        );
    }

    #[test]
    fn the_curated_vim_zathura_alternatives_resolve() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("j"), None), Some(Action::Next));
        assert_eq!(keymap.resolve(Some("k"), None), Some(Action::Previous));
        assert_eq!(
            keymap.resolve_with_mods(Some("G"), Mods::shift(), None),
            Some(Action::Last)
        );
        assert_eq!(
            keymap.resolve(Some("u"), None),
            Some(Action::UndoAnnotation)
        );
    }

    #[test]
    fn navigation_keeps_arrows_and_page_keys_without_excess_aliases() {
        let keymap = Keymap::default();
        for key in ["Right", "PageDown"] {
            assert_eq!(keymap.resolve(Some(key), None), Some(Action::Next), "{key}");
        }
        for key in ["Left", "PageUp"] {
            assert_eq!(
                keymap.resolve(Some(key), None),
                Some(Action::Previous),
                "{key}"
            );
        }
        for key in ["h", "Down", "Up", "Space", "Backspace"] {
            assert_eq!(keymap.resolve(Some(key), None), None, "{key} is excessive");
        }
    }

    #[test]
    fn shared_navigation_actions_use_page_vocabulary() {
        assert_eq!(Action::Next.label(), "Next page");
        assert_eq!(Action::Previous.label(), "Previous page");
        assert_eq!(Action::First.label(), "First page");
        assert_eq!(Action::Last.label(), "Last page");
        assert_eq!(Action::ShowOverview.label(), "Page overview");
    }

    #[test]
    fn preview_actions_have_no_fixed_shortcuts() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("Tab"), None), None);
        assert_eq!(
            keymap.resolve_with_mods(Some("Tab"), Mods::shift(), None),
            None
        );
        assert_eq!(keymap.resolve(Some("Enter"), None), None);
        assert_eq!(keymap.resolve(Some("Escape"), None), None);
        for action in [
            Action::PreviewNext,
            Action::PreviewPrevious,
            Action::CommitPreview,
            Action::CancelPreview,
        ] {
            assert!(keymap.keys_for(action).is_empty(), "{action:?}");
        }
    }

    /// Reading and presenting are layouts, and the layout keys are how a file
    /// moves between them.
    ///
    /// `r` used to be a second name for `l` and was left unbound when that
    /// went; it now reads the document aloud. What this test is actually
    /// defending is that mounting the reader is *not* a key of its own, and
    /// that `l` still cycles layouts — so it asserts those, and asserts what
    /// `r` does now rather than that it does nothing.
    #[test]
    fn reading_and_presenting_are_reached_by_the_layout_keys() {
        let keymap = Keymap::default();
        assert!(keymap.keys_for(Action::ToggleReader).is_empty());
        assert_eq!(keymap.resolve(Some("l"), None), Some(Action::CycleLayout));
        assert_eq!(
            keymap.resolve(Some("r"), None),
            Some(Action::SpeakToggle),
            "r reads aloud; it is no longer a spare name for the layout key"
        );
    }

    #[test]
    fn the_timer_keys_are_unaffected_by_the_letters_around_them() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve_with_mods(Some("t"), Mods::shift(), None),
            Some(Action::ResetTimer)
        );
        assert_eq!(keymap.resolve(Some("t"), None), Some(Action::ToggleTimer));
    }

    #[test]
    fn layout_keys_cycle_or_open_the_library() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("l"), None), Some(Action::CycleLayout));
        assert_eq!(
            keymap.resolve_with_mods(Some("L"), Mods::shift(), None),
            Some(Action::ShowLayouts)
        );
    }

    #[test]
    fn search_uses_the_characters_iced_reports() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("/"), None), Some(Action::FocusSearch));
        assert_eq!(
            keymap.resolve_with_mods(Some("f"), Mods::primary(), None),
            Some(Action::FocusSearch)
        );
        assert_eq!(
            keymap.resolve(Some("f"), None),
            Some(Action::ToggleAudienceFullscreen)
        );
    }

    #[test]
    fn link_focus_has_no_fixed_keyboard_shortcut() {
        let keymap = Keymap::default();
        assert!(keymap.keys_for(Action::FocusNextLink).is_empty());
        assert!(keymap.keys_for(Action::FocusPreviousLink).is_empty());
    }

    #[test]
    fn every_fixed_shortcut_appears_in_exactly_one_semantic_group() {
        let keymap = Keymap::default();
        for action in Action::ALL {
            let occurrences = SHORTCUT_GROUPS
                .iter()
                .flat_map(|group| group.actions.iter())
                .filter(|listed| **listed == action)
                .count();
            let has_binding = !keymap.keys_for(action).is_empty();
            assert_eq!(occurrences, usize::from(has_binding), "{action:?}");
        }
    }

    #[test]
    fn landing_reference_is_a_live_subset_of_the_fixed_keymap() {
        let keymap = Keymap::default();
        for action in QUICK_START_ACTIONS.into_iter().chain(PRESENTING_ACTIONS) {
            assert!(!keymap.keys_for(action).is_empty(), "{action:?}");
        }
    }

    #[test]
    fn remote_aliases_reject_modifiers_and_unknown_scancodes() {
        assert_eq!(
            Keymap::resolve_remote(Some("MediaTrackNext"), Mods::shift()),
            None
        );
        assert_eq!(Keymap::default().resolve(None, Some(191)), None);
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn a_legacy_shift_prefixed_name_loads_as_a_modifier() {
        // Written by any version before modifiers were data. Left as a name,
        // it would never match again: the toolkit reports "Z" and the shift
        // flag separately.
        let stored = r#"{"bindings":[[{"kind":"named","key":"CtrlShiftZ"},"redo-annotation"]]}"#;
        let keymap: Keymap = serde_json::from_str(stored).expect("should load");
        assert_eq!(
            keymap.bindings[0].0,
            KeyBinding::named_with("Z", Mods::primary_shift())
        );
        assert_eq!(
            keymap.resolve_with_mods(Some("Z"), Mods::primary_shift(), None),
            Some(Action::RedoAnnotation)
        );
    }

    #[test]
    fn retired_preview_bindings_are_removed_from_stored_keymaps() {
        let stored = r#"{"bindings":[[{"kind":"named","key":"Tab"},"preview-next"],[{"kind":"named","key":"ShiftTab"},"preview-previous"],[{"kind":"named","key":"Enter"},"commit-preview"],[{"kind":"named","key":"Escape"},"cancel-preview"],[{"kind":"named","key":"b"},"blank"]]}"#;
        let keymap: Keymap = serde_json::from_str(stored).expect("should load");
        assert_eq!(keymap.resolve(Some("b"), None), Some(Action::Blank));
        for action in [
            Action::PreviewNext,
            Action::PreviewPrevious,
            Action::CommitPreview,
            Action::CancelPreview,
        ] {
            assert!(keymap.keys_for(action).is_empty(), "{action:?}");
        }
    }

    #[test]
    fn a_plain_legacy_binding_is_unchanged() {
        let stored = r#"{"bindings":[[{"kind":"named","key":"b"},"blank"]]}"#;
        let keymap: Keymap = serde_json::from_str(stored).expect("should load");
        assert_eq!(keymap.bindings[0].0, KeyBinding::named("b"));
        assert_eq!(keymap.resolve(Some("b"), None), Some(Action::Blank));
    }

    #[test]
    fn a_bare_modifier_name_is_a_key_not_a_prefix() {
        let stored = r#"{"bindings":[[{"kind":"named","key":"Shift"},"blank"]]}"#;
        let keymap: Keymap = serde_json::from_str(stored).expect("should load");
        assert_eq!(keymap.bindings[0].0, KeyBinding::named("Shift"));
    }

    #[test]
    fn a_round_trip_through_json_keeps_the_modifiers() {
        let keymap = Keymap::default();
        let text = serde_json::to_string(&keymap).expect("should write");
        let back: Keymap = serde_json::from_str(&text).expect("should load");
        assert_eq!(back, keymap);
    }

    #[test]
    fn shipped_search_and_fullscreen_defaults_are_repaired() {
        let stored = r#"{"bindings":[[{"kind":"named","key":"slash"},"focus-search"],[{"kind":"named","key":"f","mods":{"ctrl":true}},"toggle-audience-fullscreen"]]}"#;
        let keymap: Keymap = serde_json::from_str(stored).expect("should load");
        assert_eq!(keymap.resolve(Some("/"), None), Some(Action::FocusSearch));
        assert_eq!(
            keymap.resolve_with_mods(Some("f"), Mods::primary(), None),
            Some(Action::FocusSearch)
        );
        assert_eq!(
            keymap.resolve(Some("f"), None),
            Some(Action::ToggleAudienceFullscreen)
        );
    }
}

#[cfg(test)]
mod default_repair_tests {
    use super::*;

    #[test]
    fn an_action_the_stored_file_never_heard_of_gets_its_default_key() {
        // Exactly the situation on any machine that saved a setting before
        // the overview existed: the whole keymap is on disk, without it.
        let mut stored = Keymap::default();
        stored
            .bindings
            .retain(|(_, action)| *action != Action::ShowOverview);
        assert_eq!(stored.resolve(Some("o"), None), None);

        stored.restore_missing_defaults();
        assert_eq!(stored.resolve(Some("o"), None), Some(Action::ShowOverview));
    }

    #[test]
    fn a_deliberate_remapping_survives_the_repair() {
        let mut stored = Keymap::default();
        stored
            .bindings
            .retain(|(_, action)| *action != Action::ShowOverview);
        // The presenter has put the overview on their remote's blue button
        // and uses "o" for something else.
        stored.bind(KeyBinding::scancode(191), Action::ShowOverview);
        stored.bind(KeyBinding::named("o"), Action::ToggleAnnotationAudience);

        stored.restore_missing_defaults();

        assert_eq!(
            stored.resolve(None, Some(191)),
            Some(Action::ShowOverview),
            "their binding stands"
        );
        assert_eq!(
            stored.resolve(Some("o"), None),
            Some(Action::ToggleAnnotationAudience),
            "and the default does not steal a key they have used"
        );
    }

    #[test]
    fn every_action_has_a_default_key_unless_it_is_deliberately_unbound() {
        // The repair can only give back what the defaults offer, so an action
        // with no default would be unreachable on a fresh install too. Link
        // focus is the deliberate exception, and naming it here is what keeps
        // the rule strict for everything else.
        let defaults = Keymap::default();
        for action in Action::ALL {
            let bound = defaults.bindings.iter().any(|(_, bound)| *bound == action);
            assert_eq!(
                bound,
                !Keymap::UNBOUND_BY_DEFAULT.contains(&action),
                "{action:?} disagrees with the unbound-by-default list"
            );
        }
    }
}
