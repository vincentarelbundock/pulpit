//! Shortcuts: one semantic command, platform-appropriate mechanics.
//!
//! The application never spells a modifier. It asks for the *primary*
//! modifier and this module prints `⌘S` on macOS and `Ctrl+S` elsewhere, from
//! the same [`Shortcut`] value.

use serde::{Deserialize, Serialize};

/// A modifier, named by role rather than by key cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Modifier {
    /// `Command` on macOS, `Control` elsewhere.
    Primary,
    Shift,
    Alt,
    /// `Control` even on macOS, for the rare binding that needs it.
    Control,
}

/// A key binding as the application means it, not as a platform spells it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Shortcut {
    pub modifiers: Vec<Modifier>,
    /// A portable named key: `"S"`, `"Right"`, `"F5"`, `"Space"`.
    pub key: String,
}

impl Shortcut {
    pub fn key(key: &str) -> Shortcut {
        Shortcut {
            modifiers: Vec::new(),
            key: key.to_string(),
        }
    }

    pub fn primary(key: &str) -> Shortcut {
        Shortcut {
            modifiers: vec![Modifier::Primary],
            key: key.to_string(),
        }
    }

    pub fn primary_shift(key: &str) -> Shortcut {
        Shortcut {
            modifiers: vec![Modifier::Primary, Modifier::Shift],
            key: key.to_string(),
        }
    }

    pub fn shift(key: &str) -> Shortcut {
        Shortcut {
            modifiers: vec![Modifier::Shift],
            key: key.to_string(),
        }
    }

    pub fn has(&self, modifier: Modifier) -> bool {
        self.modifiers.contains(&modifier)
    }
}

/// Platform input conventions.
pub trait InputPolicy: Send + Sync {
    /// `⌘` or `Ctrl`.
    fn primary_label(&self) -> &'static str;

    /// Format a shortcut for display.
    fn format(&self, shortcut: &Shortcut) -> String;

    /// Does this collide with something the desktop reserves? Best effort:
    /// a false negative is acceptable, a false positive is not.
    fn is_reserved(&self, shortcut: &Shortcut) -> Option<&'static str>;

    /// Should a press with these modifiers count as the primary modifier?
    fn is_primary(&self, control: bool, command: bool) -> bool;

    /// Fold the toolkit's raw modifier flags into the two semantic ones:
    /// `(primary, control)`.
    ///
    /// `control` and `command` are the physical Control and Command/logo
    /// keys as the event reported them. On macOS the Command key is primary
    /// and Control comes back separately, for the rare binding that means
    /// that key specifically. Everywhere else the Control key *is* primary
    /// — one press must never count as both — and the logo key belongs to
    /// the desktop, so it is not pulpit's to interpret.
    fn split_modifiers(&self, control: bool, command: bool) -> (bool, bool);
}

/// The portable implementation, parameterised by target at compile time.
#[derive(Debug, Default, Clone, Copy)]
pub struct DesktopInput;

/// macOS spells modifiers with glyphs and no separators.
const MACOS: bool = cfg!(target_os = "macos");

impl InputPolicy for DesktopInput {
    fn primary_label(&self) -> &'static str {
        if MACOS {
            "⌘"
        } else {
            "Ctrl"
        }
    }

    fn format(&self, shortcut: &Shortcut) -> String {
        let mut parts: Vec<String> = Vec::new();
        // A stable order, so the same binding always reads the same way.
        for modifier in [
            Modifier::Control,
            Modifier::Alt,
            Modifier::Shift,
            Modifier::Primary,
        ] {
            if !shortcut.has(modifier) {
                continue;
            }
            parts.push(
                match (modifier, MACOS) {
                    (Modifier::Primary, true) => "⌘",
                    (Modifier::Primary, false) => "Ctrl",
                    (Modifier::Shift, true) => "⇧",
                    (Modifier::Shift, false) => "Shift",
                    (Modifier::Alt, true) => "⌥",
                    (Modifier::Alt, false) => "Alt",
                    (Modifier::Control, true) => "⌃",
                    (Modifier::Control, false) => "Ctrl",
                }
                .to_string(),
            );
        }
        parts.push(pretty_key(&shortcut.key));
        if MACOS {
            parts.concat()
        } else {
            parts.join("+")
        }
    }

    fn is_reserved(&self, shortcut: &Shortcut) -> Option<&'static str> {
        let key = shortcut.key.to_ascii_uppercase();
        let primary = shortcut.has(Modifier::Primary);
        if MACOS {
            return match (primary, key.as_str()) {
                (true, "Q") => Some("macOS reserves ⌘Q for Quit"),
                (true, "H") => Some("macOS reserves ⌘H for Hide"),
                (true, "TAB") => Some("macOS reserves ⌘Tab for the application switcher"),
                (true, "SPACE") => Some("macOS reserves ⌘Space for Spotlight"),
                _ => None,
            };
        }
        match (primary, shortcut.has(Modifier::Alt), key.as_str()) {
            (_, true, "TAB") => Some("the desktop reserves Alt+Tab for window switching"),
            (_, true, "F4") => Some("Windows reserves Alt+F4 for Close"),
            (true, _, "L") if cfg!(target_os = "windows") => {
                Some("Windows reserves Ctrl+Alt+Del and Win+L for the session")
            }
            _ => None,
        }
    }

    fn is_primary(&self, control: bool, command: bool) -> bool {
        if MACOS {
            command
        } else {
            control
        }
    }

    fn split_modifiers(&self, control: bool, command: bool) -> (bool, bool) {
        if MACOS {
            (command, control)
        } else {
            (control, false)
        }
    }
}

/// Key names that read better than their raw spelling.
fn pretty_key(key: &str) -> String {
    match key {
        "Right" => {
            if MACOS {
                "→"
            } else {
                "Right"
            }
        }
        "Left" => {
            if MACOS {
                "←"
            } else {
                "Left"
            }
        }
        "Up" => {
            if MACOS {
                "↑"
            } else {
                "Up"
            }
        }
        "Down" => {
            if MACOS {
                "↓"
            } else {
                "Down"
            }
        }
        "Enter" | "Return" => {
            if MACOS {
                "↩"
            } else {
                "Enter"
            }
        }
        "Escape" => {
            if MACOS {
                "⎋"
            } else {
                "Esc"
            }
        }
        "Space" => "Space",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_modifier_matches_the_platform() {
        let input = DesktopInput;
        if MACOS {
            assert_eq!(input.primary_label(), "⌘");
            assert!(input.is_primary(false, true));
            assert!(!input.is_primary(true, false));
        } else {
            assert_eq!(input.primary_label(), "Ctrl");
            assert!(input.is_primary(true, false));
            assert!(!input.is_primary(false, true));
        }
    }

    #[test]
    fn one_press_is_never_both_primary_and_control() {
        let input = DesktopInput;
        if MACOS {
            assert_eq!(input.split_modifiers(false, true), (true, false));
            assert_eq!(input.split_modifiers(true, false), (false, true));
        } else {
            assert_eq!(input.split_modifiers(true, false), (true, false));
            // The logo key is the desktop's, not a primary modifier.
            assert_eq!(input.split_modifiers(false, true), (false, false));
        }
    }

    #[test]
    fn shortcuts_format_in_a_stable_order() {
        let input = DesktopInput;
        let save = input.format(&Shortcut::primary("S"));
        let redo = input.format(&Shortcut::primary_shift("Z"));
        if MACOS {
            assert_eq!(save, "⌘S");
            assert_eq!(redo, "⇧⌘Z");
        } else {
            assert_eq!(save, "Ctrl+S");
            assert_eq!(redo, "Shift+Ctrl+Z");
        }
        assert_eq!(input.format(&Shortcut::key("F5")), "F5");
    }

    #[test]
    fn reserved_shortcuts_are_reported_with_a_reason() {
        let input = DesktopInput;
        let clash = Shortcut {
            modifiers: vec![Modifier::Alt],
            key: "Tab".into(),
        };
        if MACOS {
            assert!(input.is_reserved(&Shortcut::primary("Q")).is_some());
        } else {
            let reason = input.is_reserved(&clash).expect("Alt+Tab is reserved");
            assert!(reason.len() > 10);
        }
        assert!(input.is_reserved(&Shortcut::key("Right")).is_none());
    }

    #[test]
    fn arrow_keys_read_naturally_on_each_platform() {
        let input = DesktopInput;
        let right = input.format(&Shortcut::key("Right"));
        assert_eq!(right, if MACOS { "→" } else { "Right" });
    }
}
