//! The capability snapshot.
//!
//! Views ask this, never `cfg!(target_os = …)`. It is immutable: when the
//! session changes, a *new* snapshot is taken, exactly as with display
//! topology, so nothing can hold a stale belief about what is possible.

use serde::{Deserialize, Serialize};

/// How well displays can be identified across a reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IdentityQuality {
    /// No enumeration at all.
    None,
    /// Session-local handles only: a reconnect loses the user's choice.
    Session,
    /// Connector plus make and model.
    Connector,
    /// EDID or a platform-stable identifier.
    Stable,
}

impl IdentityQuality {
    pub fn label(self) -> &'static str {
        match self {
            IdentityQuality::None => "no display enumeration",
            IdentityQuality::Session => "session-local display identity",
            IdentityQuality::Connector => "connector and model identity",
            IdentityQuality::Stable => "stable (EDID) display identity",
        }
    }
}

/// Whether the toolkit publishes an accessibility tree at all.
///
/// Iced 0.14 has no AccessKit integration and exposes no accessibility tree,
/// so no label, role or description pulpit writes reaches assistive technology
/// — on any platform, under any desktop, however well equipped the session is.
/// Every adapter's `accessibility_bridge` is therefore gated on this, and the
/// gate is what keeps a platform that *can* detect a session bus from
/// reporting one pulpit cannot put anything on.
///
/// When Iced gains an accessibility backend this becomes `true` and each
/// adapter's own detection starts deciding the answer. See the tracking issue
/// for the rest of the work that unblocks with it.
pub const TOOLKIT_PUBLISHES_AN_ACCESSIBILITY_TREE: bool = false;

/// What the running desktop can actually do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Session/window backend, for diagnostics: `x11`, `wayland`, `win32`, …
    pub backend: String,
    pub identity: IdentityQuality,
    /// Position a window at chosen coordinates.
    pub arbitrary_placement: bool,
    /// Leaving fullscreen cannot strand the window off-screen.
    pub safe_unfullscreen: bool,
    /// Placement requests are honoured before a window is mapped.
    pub place_before_map: bool,
    /// The system light/dark preference can be read.
    pub system_appearance: bool,
    /// A high-contrast preference can be read.
    pub high_contrast_detection: bool,
    /// Sleep and idle can be inhibited.
    pub sleep_inhibition: bool,
    /// Native file dialogs are available.
    pub native_dialogs: bool,
    /// A desktop-integrated (global) menu is available.
    pub native_menus: bool,
    /// Assistive technology can read this application's interface.
    ///
    /// Not "the session runs an accessibility bus". A bus with nothing
    /// published on it is what an X11 desktop with Orca installed looks like
    /// from here, and reporting that as a bridge would tell a screen-reader
    /// user the one thing they most need to be told the truth about. Both
    /// halves have to hold: the session must offer a bridge *and* the toolkit
    /// must publish a tree to put on it — see
    /// [`TOOLKIT_PUBLISHES_AN_ACCESSIBILITY_TREE`].
    pub accessibility_bridge: bool,
    /// Media and presenter-remote keys reach the application.
    pub media_keys: bool,
    /// Desktop notifications are available.
    pub notifications: bool,
    /// An image can be put on the system clipboard.
    ///
    /// Separate from "there is a clipboard": every session pulpit runs in
    /// has one for text, through the toolkit. This is about the pixels, and
    /// a headless session or a compositor with no data-control protocol has
    /// nowhere to put them.
    pub image_clipboard: bool,
}

impl Default for Capabilities {
    /// The honest default: assume nothing works until an adapter says it does.
    fn default() -> Self {
        Capabilities {
            backend: "unknown".into(),
            identity: IdentityQuality::None,
            arbitrary_placement: false,
            safe_unfullscreen: true,
            place_before_map: false,
            system_appearance: false,
            high_contrast_detection: false,
            sleep_inhibition: false,
            native_dialogs: false,
            native_menus: false,
            accessibility_bridge: false,
            media_keys: true,
            notifications: false,
            // Nothing to put a clipboard on: the null adapter must never
            // touch the real one.
            image_clipboard: false,
        }
    }
}

impl Capabilities {
    /// Lines for the diagnostics bundle: every fallback in effect, named.
    pub fn report(&self) -> Vec<String> {
        let flag = |on: bool, yes: &str, no: &str| {
            if on {
                yes.to_string()
            } else {
                no.to_string()
            }
        };
        vec![
            format!("backend: {}", self.backend),
            format!("display identity: {}", self.identity.label()),
            flag(
                self.arbitrary_placement,
                "window placement: yes",
                "window placement: NO — manual placement may be required",
            ),
            flag(
                self.safe_unfullscreen,
                "leaving fullscreen: safe",
                "leaving fullscreen: unsafe — windows are left as they are",
            ),
            flag(
                self.system_appearance,
                "system appearance: detected",
                "system appearance: not detectable — using the dark palette",
            ),
            flag(
                self.sleep_inhibition,
                "sleep inhibition: available",
                "sleep inhibition: NOT available — the screen may blank",
            ),
            flag(
                self.native_dialogs,
                "file dialogs: native",
                "file dialogs: none — open files from the command line",
            ),
            flag(
                self.accessibility_bridge,
                "accessibility bridge: present",
                "accessibility bridge: absent",
            ),
        ]
    }

    /// Everything that is *not* available, for a one-line summary.
    pub fn limitations(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.arbitrary_placement {
            out.push("choosing which display the audience window uses");
        }
        if !self.sleep_inhibition {
            out.push("keeping the screensaver away");
        }
        if !self.native_dialogs {
            out.push("opening files through a dialog");
        }
        if self.identity < IdentityQuality::Connector {
            out.push("remembering displays across a reconnect");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_claims_nothing() {
        let capabilities = Capabilities::default();
        assert!(!capabilities.arbitrary_placement);
        assert!(!capabilities.native_dialogs);
        assert_eq!(capabilities.identity, IdentityQuality::None);
    }

    #[test]
    fn the_report_names_every_fallback_in_effect() {
        let capabilities = Capabilities::default();
        let report = capabilities.report().join("\n");
        assert!(report.contains("window placement: NO"));
        assert!(report.contains("sleep inhibition: NOT available"));
        assert!(!capabilities.limitations().is_empty());

        let full = Capabilities {
            arbitrary_placement: true,
            system_appearance: true,
            sleep_inhibition: true,
            native_dialogs: true,
            identity: IdentityQuality::Stable,
            ..Capabilities::default()
        };
        assert!(full.limitations().is_empty());
    }

    #[test]
    fn identity_quality_is_ordered_weakest_first() {
        assert!(IdentityQuality::None < IdentityQuality::Session);
        assert!(IdentityQuality::Connector < IdentityQuality::Stable);
    }
}
