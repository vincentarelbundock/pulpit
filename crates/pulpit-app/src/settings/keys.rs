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
    ShowDiagnostics,
    /// The whole deck as thumbnails, to jump by eye rather than by number.
    ShowOverview,
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
    Quit,
}

impl Action {
    /// Every action, so a keymap can be checked against the whole set.
    pub const ALL: [Action; 29] = [
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
        Action::ShowDiagnostics,
        Action::ShowOverview,
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
            Action::ShowDiagnostics => "Diagnostics",
            Action::ShowOverview => "Slide overview",
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
            Action::Quit => "Quit",
        }
    }
}

/// How a key is recognised.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum KeyBinding {
    /// A named logical key: `"Right"`, `"space"`, `"F5"`, `"MediaPlayPause"`.
    Named { key: String },
    /// A raw physical scancode, for remotes whose keys the toolkit reports as
    /// unidentified. This is the documented fallback path, not an oddity.
    Scancode { code: u32 },
}

impl KeyBinding {
    pub fn named(key: &str) -> Self {
        KeyBinding::Named {
            key: key.to_string(),
        }
    }

    pub fn scancode(code: u32) -> Self {
        KeyBinding::Scancode { code }
    }

    pub fn describe(&self) -> String {
        match self {
            KeyBinding::Named { key } => key.clone(),
            KeyBinding::Scancode { code } => format!("scancode {code}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keymap {
    pub bindings: Vec<(KeyBinding, Action)>,
}

impl Default for Keymap {
    fn default() -> Self {
        let named = |key: &str, action: Action| (KeyBinding::named(key), action);
        Self {
            bindings: vec![
                // Ordinary keyboard.
                named("Right", Action::Next),
                named("Down", Action::Next),
                named("Space", Action::Next),
                named("PageDown", Action::Next),
                named("Enter", Action::Next),
                named("Left", Action::Previous),
                named("Up", Action::Previous),
                named("PageUp", Action::Previous),
                named("Backspace", Action::Previous),
                named("Home", Action::First),
                named("End", Action::Last),
                named("Tab", Action::PreviewNext),
                named("ShiftTab", Action::PreviewPrevious),
                named("Return", Action::CommitPreview),
                named("Escape", Action::CancelPreview),
                named("b", Action::Blank),
                named("w", Action::BlankAlternate),
                named("p", Action::ToggleTimer),
                named("r", Action::ResetTimer),
                named("s", Action::SwapDisplays),
                named("f", Action::ToggleAudienceFullscreen),
                named("o", Action::OpenDocument),
                named("F5", Action::ReloadDocument),
                named("d", Action::ShowDiagnostics),
                // "j" for jump: the deck at a glance, to land on a slide by
                // eye rather than by number.
                named("j", Action::ShowOverview),
                // The three tools sit under the digits, in the order the
                // palette draws them; the marks they make are cleared and
                // taken back from the keys beside them.
                named("1", Action::AnnotateInk),
                named("2", Action::AnnotateHighlighter),
                named("3", Action::AnnotateEraser),
                named("4", Action::AnnotatePointer),
                named("z", Action::UndoAnnotation),
                named("ShiftZ", Action::RedoAnnotation),
                named("c", Action::ClearAnnotations),
                named("v", Action::ToggleAnnotationAudience),
                // "l" for link, and its neighbour for stepping back. Tab is
                // already the preview key, so link focus gets its own pair
                // rather than overloading the one presenters use most.
                named("l", Action::FocusNextLink),
                named("k", Action::FocusPreviousLink),
                named("q", Action::Quit),
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
        self.resolve_with_shift(key, false, scancode)
    }

    /// Resolve a keypress, preferring a binding written for the shifted form.
    ///
    /// `Shift`-prefixed names — `"ShiftTab"`, `"ShiftRight"` — are tried
    /// first and the bare name second. Falling back matters: `Shift`+`b`
    /// must still blank the screen, because a presenter holding the key down
    /// with a finger resting on shift is not asking for something else.
    pub fn resolve_with_shift(
        &self,
        key: Option<&str>,
        shift: bool,
        scancode: Option<u32>,
    ) -> Option<Action> {
        if let Some(key) = key {
            let shifted = shift.then(|| format!("Shift{key}"));
            for candidate in shifted.as_deref().into_iter().chain(std::iter::once(key)) {
                if let Some(action) =
                    self.bindings
                        .iter()
                        .find_map(|(binding, action)| match binding {
                            KeyBinding::Named { key: named }
                                if named.eq_ignore_ascii_case(candidate) =>
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
        let scancode = scancode?;
        self.bindings
            .iter()
            .find_map(|(binding, action)| match binding {
                KeyBinding::Scancode { code } if *code == scancode => Some(*action),
                _ => None,
            })
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
    fn every_action_has_at_least_one_default_binding() {
        let keymap = Keymap::default();
        let actions = [
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
            Action::ShowDiagnostics,
            Action::Quit,
        ];
        for action in actions {
            assert!(
                !keymap.keys_for(action).is_empty(),
                "{action:?} is unreachable from the keyboard"
            );
        }
    }

    #[test]
    fn each_annotation_tool_has_its_own_key_and_none_is_shared() {
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
        let mut keys: Vec<String> = Vec::new();
        for action in annotation {
            let bound = keymap.keys_for(action);
            assert!(!bound.is_empty(), "{action:?} is unreachable");
            keys.extend(bound);
        }
        for key in &keys {
            assert_eq!(
                keymap.resolve(Some(key), None),
                Some(
                    *annotation
                        .iter()
                        .find(|action| keymap.keys_for(**action).contains(key))
                        .unwrap()
                ),
                "{key} resolves to something else, so an annotation key was taken twice"
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
        // The toolkit reports the modifier separately from the key, so a
        // `Shift`-prefixed binding only works if resolution looks for it.
        // Without this, every `Shift…` default was silently dead.
        let keymap = Keymap::default();
        assert_eq!(
            keymap.resolve_with_shift(Some("Tab"), true, None),
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
            keymap.resolve_with_shift(Some("b"), true, None),
            Some(Action::Blank)
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
        assert_eq!(stored.resolve(Some("j"), None), None);

        stored.restore_missing_defaults();
        assert_eq!(stored.resolve(Some("j"), None), Some(Action::ShowOverview));
    }

    #[test]
    fn a_deliberate_remapping_survives_the_repair() {
        let mut stored = Keymap::default();
        stored
            .bindings
            .retain(|(_, action)| *action != Action::ShowOverview);
        // The presenter has put the overview on their remote's blue button
        // and uses "j" for something else.
        stored.bind(KeyBinding::scancode(191), Action::ShowOverview);
        stored.bind(KeyBinding::named("j"), Action::BlankAlternate);

        stored.restore_missing_defaults();

        assert_eq!(
            stored.resolve(None, Some(191)),
            Some(Action::ShowOverview),
            "their binding stands"
        );
        assert_eq!(
            stored.resolve(Some("j"), None),
            Some(Action::BlankAlternate),
            "and the default does not steal a key they have used"
        );
    }

    #[test]
    fn every_action_has_a_default_key() {
        // The repair can only give back what the defaults offer, so an action
        // with no default would be unreachable on a fresh install too.
        let defaults = Keymap::default();
        for action in Action::ALL {
            assert!(
                defaults.bindings.iter().any(|(_, bound)| *bound == action),
                "{:?} has no default binding",
                action
            );
        }
    }
}
