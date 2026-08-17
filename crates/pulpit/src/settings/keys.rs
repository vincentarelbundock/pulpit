//! Keyboard and presenter-remote bindings.
//!
//! Remotes are the reason this is configurable at all: many report ordinary
//! keys, some report media keys, and a few report nothing a toolkit can name.
//! Every binding therefore has a raw-scancode fallback.

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
    PreviewNext,
    PreviewPrevious,
    CommitPreview,
    CancelPreview,
    /// Blank in whichever colour the venue setting names. This is the key a
    /// presenter reaches for mid-talk, and which colour it produces is a
    /// property of the room rather than of the deck.
    ///
    /// The old `blank-black` binding loads as this one: a stored keymap must
    /// keep working, and black is the default colour, so the behaviour a
    /// presenter had is the behaviour they keep.
    #[serde(alias = "blank-black")]
    Blank,
    /// Blank in the colour the setting did *not* name, for anyone who wants
    /// both within reach without visiting the settings page.
    #[serde(alias = "blank-white")]
    BlankAlternate,
    ToggleTimer,
    ResetTimer,
    SwapDisplays,
    ToggleAudienceFullscreen,
    OpenDocument,
    ReloadDocument,
    /// The whole deck as thumbnails, to jump by eye rather than by number.
    ShowOverview,
    /// The layout library, without going through the menu first.
    ///
    /// The presenter screen *is* the layout, so changing it is a first-class
    /// thing to do at the lectern rather than a setting to go looking for.
    ShowLayouts,
    // Annotations. Arming a tool is a toggle: the same key puts it down
    // again, so the presenter never has to find an "off" control while the
    // room is watching.
    AnnotateInk,
    AnnotateHighlighter,
    AnnotateEraser,
    /// The dot, or the lit circle: which of the two depends on the mode the
    /// pointer control is in, so one key covers both — which is also why a
    /// keymap that names the spotlight still resolves here.
    #[serde(alias = "annotate-spotlight")]
    AnnotatePointer,
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

impl Action {
    /// Every action, so a keymap can be checked against the whole set.
    pub const ALL: [Action; 41] = [
        Action::Next,
        Action::Previous,
        Action::First,
        Action::Last,
        Action::PreviewNext,
        Action::PreviewPrevious,
        Action::CommitPreview,
        Action::CancelPreview,
        Action::Blank,
        Action::BlankAlternate,
        Action::ToggleTimer,
        Action::ResetTimer,
        Action::SwapDisplays,
        Action::ToggleAudienceFullscreen,
        Action::OpenDocument,
        Action::ReloadDocument,
        Action::ShowOverview,
        Action::ShowLayouts,
        Action::AnnotateInk,
        Action::AnnotateHighlighter,
        Action::AnnotateEraser,
        Action::AnnotatePointer,
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
            Action::Next => "Next slide",
            Action::Previous => "Previous slide",
            Action::First => "First slide",
            Action::Last => "Last slide",
            Action::PreviewNext => "Preview next",
            Action::PreviewPrevious => "Preview previous",
            Action::CommitPreview => "Show the previewed slide",
            Action::CancelPreview => "Cancel preview",
            Action::Blank => "Blank",
            Action::BlankAlternate => "Blank (other colour)",
            Action::ToggleTimer => "Start/pause timer",
            Action::ResetTimer => "Reset timer",
            Action::SwapDisplays => "Swap displays",
            Action::ToggleAudienceFullscreen => "Audience fullscreen",
            Action::OpenDocument => "Open…",
            Action::ReloadDocument => "Reload document",
            Action::ShowOverview => "Slide overview",
            Action::ShowLayouts => "Layouts",
            Action::AnnotateInk => "Draw on the slide",
            Action::AnnotateHighlighter => "Highlight on the slide",
            Action::AnnotateEraser => "Erase annotations",
            Action::AnnotatePointer => "Point at the slide",
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
            Action::ToggleDualPage => "One or two pages across",
            Action::Quit => "Quit",
        }
    }
}

/// The modifiers held down with a key.
///
/// These are data rather than a prefix glued onto the key name. The old
/// `"ShiftTab"` spelling could not express two modifiers at once, and — worse
/// — it gave resolution no way to tell a modifier that is load-bearing from
/// one that is incidental. See [`Keymap::resolve_with_mods`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
    };

    pub fn new(ctrl: bool, shift: bool, alt: bool) -> Self {
        Mods { ctrl, shift, alt }
    }

    pub fn ctrl() -> Self {
        Mods {
            ctrl: true,
            ..Mods::NONE
        }
    }

    pub fn shift() -> Self {
        Mods {
            shift: true,
            ..Mods::NONE
        }
    }

    pub fn ctrl_shift() -> Self {
        Mods {
            ctrl: true,
            shift: true,
            alt: false,
        }
    }

    fn prefix(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
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
                        "Ctrl" | "Control" => mods.ctrl = true,
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
    bindings: Vec<(KeyBinding, Action)>,
}

impl From<KeymapWire> for Keymap {
    fn from(wire: KeymapWire) -> Self {
        let mut bindings: Vec<_> = wire
            .bindings
            .into_iter()
            .map(|(binding, action)| (binding.normalized(), action))
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
                && *binding == KeyBinding::named_with("f", Mods::ctrl())
            {
                *binding = KeyBinding::named("f");
            }
        }
        let ctrl_f = KeyBinding::named_with("f", Mods::ctrl());
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
                // Navigation is vim-first: `j`/`l` forward and `k`/`h` back,
                // so the keys a zathura or vim user reaches for are the ones
                // that work. Nothing conventional is taken away — space, the
                // arrows and the paging keys stay bound, because the person
                // driving the laptop is not always the person who chose the
                // bindings, and no remote emits `j`.
                named("j", Action::Next),
                named("l", Action::Next),
                named("Right", Action::Next),
                named("Down", Action::Next),
                named("Space", Action::Next),
                named("PageDown", Action::Next),
                named("k", Action::Previous),
                named("h", Action::Previous),
                named("Left", Action::Previous),
                named("Up", Action::Previous),
                named("PageUp", Action::Previous),
                named("Backspace", Action::Previous),
                named("Home", Action::First),
                // `G` for the last slide, as in vim. Its partner `gg` needs a
                // key-sequence buffer that resolution does not have, so the
                // first slide is `Home` until sequences exist.
                with("g", Mods::shift(), Action::Last),
                named("End", Action::Last),
                named("Tab", Action::PreviewNext),
                with("Tab", Mods::shift(), Action::PreviewPrevious),
                // `Enter` commits the preview and does *not* also advance.
                // It used to do both, which meant one of the two bindings was
                // dead — and since the toolkit never reports "Return", the
                // dead one was the commit. Next has six other keys; commit
                // has none, so this is where `Enter` belongs.
                named("Enter", Action::CommitPreview),
                named("Escape", Action::CancelPreview),
                // Blanking is the most reflexive key at a lectern, so it does
                // not move for anyone — `b` and `w` are vim word motions, but
                // a presenter has no words to move over.
                named("b", Action::Blank),
                named("w", Action::BlankAlternate),
                named("t", Action::ToggleTimer),
                with("t", Mods::shift(), Action::ResetTimer),
                named("s", Action::SwapDisplays),
                // "o" for overview: the deck at a glance, to land on a slide
                // by eye rather than by number. It used to be "j", which is
                // now a navigation key; "o" is free because opening a file is
                // `Ctrl+O` like everywhere else.
                named("o", Action::ShowOverview),
                // "r" for read: the same file, as a document to mark up
                // rather than a deck to present. Pressed again it goes back.
                // This binding existed before and never fired, because the
                // timer reset had taken "r" earlier in the list.
                named("r", Action::ToggleReader),
                // Layouts keep their mnemonic one row up, "l" itself being a
                // navigation key now.
                with("l", Mods::shift(), Action::ShowLayouts),
                // The four tools sit under the digits, in the order the
                // palette draws them; the marks they make are cleared and
                // taken back from the keys beside them.
                named("1", Action::AnnotateInk),
                named("2", Action::AnnotateHighlighter),
                named("3", Action::AnnotateEraser),
                named("4", Action::AnnotatePointer),
                // Undo and redo of a stroke are editing, not navigation, so
                // they follow the convention every other editor uses. `u` is
                // kept as vim's undo; vim's `Ctrl+R` redo is the one vim key
                // given up, because reload has the stronger claim on it.
                with("z", Mods::ctrl(), Action::UndoAnnotation),
                named("u", Action::UndoAnnotation),
                with("z", Mods::ctrl_shift(), Action::RedoAnnotation),
                with("y", Mods::ctrl(), Action::RedoAnnotation),
                named("c", Action::ClearAnnotations),
                named("v", Action::ToggleAnnotationAudience),
                // Anything that leaves the deck — opening, reloading, quitting,
                // resizing the audience window — takes the primary modifier,
                // as it does in every other application. Quit especially: a
                // bare "q" next to "w" is a talk ended by a typo.
                with("o", Mods::ctrl(), Action::OpenDocument),
                // Ctrl+F and "/" find; F3 and Shift+F3 step through matches.
                // Iced reports the slash character as "/", so the binding
                // must use the character rather than the key-cap name.
                // Ctrl+B for the side rail, as every editor and reader with a
                // sidebar has it. Bare "b" is not free — and would be a
                // blanked screen in the middle of a talk if it were taken.
                with("b", Mods::ctrl(), Action::ToggleOutline),
                with("f", Mods::ctrl(), Action::FocusSearch),
                named("/", Action::FocusSearch),
                named("f3", Action::FindNext),
                with("f3", Mods::shift(), Action::FindPrevious),
                named("?", Action::FindPrevious),
                // Reader transforms keep both Acrobat and Zathura muscle
                // memory. One semantic action may have several bindings; a
                // physical combination still resolves to only one action.
                named("+", Action::ZoomIn),
                with("=", Mods::ctrl(), Action::ZoomIn),
                named("-", Action::ZoomOut),
                with("-", Mods::ctrl(), Action::ZoomOut),
                named("=", Action::ZoomReset),
                with("1", Mods::ctrl(), Action::ZoomReset),
                named("a", Action::FitPage),
                with("0", Mods::ctrl(), Action::FitPage),
                with("2", Mods::ctrl(), Action::FitWidth),
                with("r", Mods::shift(), Action::RotateReader),
                named("d", Action::ToggleDualPage),
                with("r", Mods::ctrl(), Action::ReloadDocument),
                named("F5", Action::ReloadDocument),
                named("f", Action::ToggleAudienceFullscreen),
                with("q", Mods::ctrl(), Action::Quit),
                // Link focus has no default key. Stepping through a slide's
                // links is rare enough that it does not earn one of the bare
                // letters, and both actions stay bindable for anyone whose
                // decks lean on internal navigation.
                //
                // Common presenter remotes: most emit these logical keys.
                named("F1", Action::Blank),
                named("Escape", Action::CancelPreview),
                named("MediaPlayPause", Action::Blank),
                named("MediaTrackNext", Action::Next),
                named("MediaTrackPrevious", Action::Previous),
                named("BrowserForward", Action::Next),
                named("BrowserBack", Action::Previous),
                named("AudioVolumeUp", Action::Next),
                named("AudioVolumeDown", Action::Previous),
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
    /// `Ctrl`+`Q` finds quit and `Shift`+`Tab` finds the previous preview.
    /// The second allows an *unmatched shift only*: a presenter resting a
    /// finger on shift must still be able to blank the screen with `b`.
    ///
    /// `Ctrl` and `Alt` never fall back. They are pressed deliberately, so
    /// `Ctrl`+`Q` must never reach a binding for a bare `q` — which is the
    /// whole reason modifiers are data here and not a name prefix.
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
                bound.ctrl == mods.ctrl
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
    /// Stepping through a slide's links is rare enough that it does not earn
    /// one of the bare letters, but it stays bindable. Listing them here is
    /// what lets the "everything is reachable" test stay strict about the
    /// rest.
    pub const UNBOUND_BY_DEFAULT: [Action; 2] = [Action::FocusNextLink, Action::FocusPreviousLink];

    /// The binding to *show* for an action, if any.
    ///
    /// Menu labels quote one key, not the whole list, and a scancode is not
    /// something a person can read off a key cap — so this is the first named
    /// binding, in keymap order, which is the one the defaults put first on
    /// purpose.
    pub fn display_binding(&self, action: Action) -> Option<&KeyBinding> {
        self.bindings
            .iter()
            .find(|(binding, bound)| {
                *bound == action && matches!(binding, KeyBinding::Named { .. })
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
            KeyBinding::named_with("m", Mods::ctrl()),
            Action::ShowOverview,
        );
        assert_eq!(
            keymap.display_binding(Action::ShowOverview),
            Some(&KeyBinding::named_with("m", Mods::ctrl()))
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
            Action::AnnotateInk,
            Action::AnnotateHighlighter,
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
    fn documented_remote_keys_resolve() {
        let keymap = Keymap::default();
        for key in [
            "Right",
            "Left",
            "PageDown",
            "PageUp",
            "MediaTrackNext",
            "MediaTrackPrevious",
            "BrowserForward",
            "BrowserBack",
            "F5",
            "F1",
        ] {
            assert!(
                keymap.resolve(Some(key), None).is_some(),
                "{key} is not bound"
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
            keymap.resolve_with_mods(Some("Tab"), Mods::shift(), None),
            Some(Action::PreviewPrevious)
        );
    }

    #[test]
    fn an_unshifted_press_never_reaches_a_shifted_binding() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("Tab"), None), Some(Action::PreviewNext));
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
            keymap.resolve_with_mods(Some("q"), Mods::ctrl(), None),
            Some(Action::Quit)
        );
        assert_eq!(keymap.resolve(Some("q"), None), None, "a bare q is inert");
        // Ctrl+B is the outline rail's own key. What matters here is that it
        // is *that* and not a blanked screen: the bare "b" beneath it must not
        // show through.
        assert_eq!(
            keymap.resolve_with_mods(Some("b"), Mods::ctrl(), None),
            Some(Action::ToggleOutline),
            "Ctrl+B must not blank the screen"
        );
        assert_eq!(
            keymap.resolve_with_mods(Some("j"), Mods::new(false, false, true), None),
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
            keymap.resolve_with_mods(Some("z"), Mods::ctrl(), None),
            Some(Action::UndoAnnotation)
        );
        assert_eq!(
            keymap.resolve_with_mods(Some("Z"), Mods::ctrl_shift(), None),
            Some(Action::RedoAnnotation)
        );
    }

    #[test]
    fn vim_navigation_keys_are_the_primary_bindings() {
        let keymap = Keymap::default();
        for key in ["j", "l"] {
            assert_eq!(keymap.resolve(Some(key), None), Some(Action::Next), "{key}");
        }
        for key in ["k", "h"] {
            assert_eq!(
                keymap.resolve(Some(key), None),
                Some(Action::Previous),
                "{key}"
            );
        }
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
    fn conventional_navigation_keys_are_not_taken_away() {
        // Vim-first is not vim-only: a remote emits none of these letters,
        // and the person driving the laptop did not necessarily choose the
        // bindings.
        let keymap = Keymap::default();
        for key in ["Space", "Right", "Down", "PageDown"] {
            assert_eq!(keymap.resolve(Some(key), None), Some(Action::Next), "{key}");
        }
        for key in ["Left", "Up", "PageUp", "Backspace"] {
            assert_eq!(
                keymap.resolve(Some(key), None),
                Some(Action::Previous),
                "{key}"
            );
        }
    }

    #[test]
    fn enter_commits_the_preview_rather_than_advancing() {
        // It used to be bound to both, so the toolkit's "Enter" hit Next and
        // the commit — written as "Return", a name the toolkit never emits —
        // was dead twice over.
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve(Some("Enter"), None),
            Some(Action::CommitPreview)
        );
    }

    #[test]
    fn reader_mode_is_actually_reachable() {
        // The regression this whole change started from.
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("r"), None), Some(Action::ToggleReader));
        assert_eq!(
            keymap.resolve_with_mods(Some("t"), Mods::shift(), None),
            Some(Action::ResetTimer)
        );
        assert_eq!(keymap.resolve(Some("t"), None), Some(Action::ToggleTimer));
    }

    #[test]
    fn search_uses_the_characters_iced_reports() {
        let keymap = Keymap::default();
        assert_eq!(keymap.resolve(Some("/"), None), Some(Action::FocusSearch));
        assert_eq!(
            keymap.resolve_with_mods(Some("f"), Mods::ctrl(), None),
            Some(Action::FocusSearch)
        );
        assert_eq!(
            keymap.resolve(Some("f"), None),
            Some(Action::ToggleAudienceFullscreen)
        );
    }

    #[test]
    fn link_focus_has_no_default_key_but_stays_bindable() {
        let mut keymap = Keymap::default();
        assert!(keymap.keys_for(Action::FocusNextLink).is_empty());
        keymap.bind(KeyBinding::named("n"), Action::FocusNextLink);
        assert_eq!(
            keymap.resolve(Some("n"), None),
            Some(Action::FocusNextLink),
            "a presenter whose decks lean on links can still have the key"
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn a_legacy_shift_prefixed_name_loads_as_a_modifier() {
        // Written by any version before modifiers were data. Left as a name,
        // it would never match again: the toolkit reports "Tab" and the shift
        // flag separately.
        let stored = r#"{"bindings":[[{"kind":"named","key":"ShiftTab"},"preview-previous"]]}"#;
        let keymap: Keymap = serde_json::from_str(stored).expect("should load");
        assert_eq!(
            keymap.bindings[0].0,
            KeyBinding::named_with("Tab", Mods::shift())
        );
        assert_eq!(
            keymap.resolve_with_mods(Some("Tab"), Mods::shift(), None),
            Some(Action::PreviewPrevious)
        );
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
            keymap.resolve_with_mods(Some("f"), Mods::ctrl(), None),
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
        stored.bind(KeyBinding::named("o"), Action::BlankAlternate);

        stored.restore_missing_defaults();

        assert_eq!(
            stored.resolve(None, Some(191)),
            Some(Action::ShowOverview),
            "their binding stands"
        );
        assert_eq!(
            stored.resolve(Some("o"), None),
            Some(Action::BlankAlternate),
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
