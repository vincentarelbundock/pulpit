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
    /// A document can be sent to a printer.
    ///
    /// Not "this machine has a printer": a session with a spooler and no
    /// queues configured can still be printed from, and the queue is where
    /// that is found out. This is whether there is anything here to hand a
    /// file to at all.
    pub printing: bool,
    /// The spooler takes the job's particulars: which pages, how many copies,
    /// which queue.
    ///
    /// One flag for all three because they arrive together. A spooler either
    /// takes a job description — CUPS does — or it is a shell verb that
    /// prints a file to the default printer and takes nothing else, in which
    /// case none of the three can be honoured and the dialog must not offer
    /// them. False here never means "printing does not work"; that is
    /// [`Capabilities::printing`].
    pub print_options: bool,
    /// The platform puts *its own* print dialog up, and asks the reader
    /// which pages, which printer, how many copies, duplex, paper and the
    /// rest of it.
    ///
    /// When this is true pulpit asks none of those: the system dialog is the
    /// dialog, and pulpit's own asks only what no system dialog can know —
    /// whether the paper carries the reader's marks. When it is false pulpit
    /// asks the ones its spooler can honour, which is what
    /// [`Capabilities::print_options`] governs.
    ///
    /// A separate flag rather than a stronger `print_options` because the two
    /// are independent: a session can have a spooler that takes a page range
    /// and no dialog to ask for one — CUPS with no portal is exactly that —
    /// and which of them is true decides who does the asking.
    pub system_print_dialog: bool,
    /// An image can be put on the system clipboard.
    ///
    /// Separate from "there is a clipboard": every session pulpit runs in
    /// has one for text, through the toolkit. This is about the pixels, and
    /// a headless session or a compositor with no data-control protocol has
    /// nowhere to put them.
    pub image_clipboard: bool,
    /// Whether this session can read a document aloud, and if not, why.
    ///
    /// Three states, not a `bool`, for the same reason `accessibility_bridge`
    /// is careful about what it claims: "cannot" and "not yet" have different
    /// remedies, and a control that is merely greyed out tells the reader
    /// which of the two it is — never. A session with no audio output is
    /// [`Speech::Unavailable`]; a session one download away is
    /// [`Speech::Downloadable`], and the settings page offers the download
    /// with its size on it.
    pub speech: Speech,
}

/// What speech can do in this session.
///
/// Mirrors `pulpit_media::speech::Availability`, which is where it is computed.
/// Restated here because `Capabilities` is what views are allowed to ask, and
/// a view that reached into the speech crate for one field would be the
/// beginning of the platform boundary leaking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Speech {
    /// No engine can run here — no audio output, or no build for this
    /// platform. A download would not help.
    Unavailable { why: String },
    /// Everything is present except the artifacts.
    Downloadable { bytes: u64, needs_engine: bool },
    /// Ready, with this many voices installed.
    Ready { voices: usize },
}

impl Default for Speech {
    /// The honest default, before anything has been probed.
    fn default() -> Self {
        Speech::Unavailable {
            why: "speech has not been set up".into(),
        }
    }
}

impl Speech {
    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn can_speak(&self) -> bool {
        matches!(self, Speech::Ready { .. })
    }

    /// Whether the settings page should offer a download button.
    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn can_download(&self) -> bool {
        matches!(self, Speech::Downloadable { .. } | Speech::Ready { .. })
    }
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
            // The null adapter must never send anything to a real printer,
            // and a desktop that has not been taught to print says so.
            printing: false,
            print_options: false,
            system_print_dialog: false,
            // Nothing to put a clipboard on: the null adapter must never
            // touch the real one.
            image_clipboard: false,
            speech: Speech::default(),
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
                self.printing,
                "printing: available",
                "printing: NOT available — no spooler answered",
            ),
            flag(
                self.system_print_dialog,
                "print dialog: the system's own",
                "print dialog: pulpit's — no system print dialog answered",
            ),
            flag(
                self.accessibility_bridge,
                "accessibility bridge: present",
                "accessibility bridge: absent",
            ),
            match &self.speech {
                Speech::Unavailable { why } => format!("speech: unavailable — {why}"),
                Speech::Downloadable { bytes, .. } => {
                    format!("speech: not installed — {bytes} bytes to download")
                }
                Speech::Ready { voices } => format!("speech: ready, {voices} voice(s)"),
            },
        ]
    }

    /// Everything that is *not* available, for a one-line summary.
    #[allow(dead_code)] // reached by its tests, not by the application
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
        if !self.printing {
            out.push("printing");
        }
        if self.identity < IdentityQuality::Connector {
            out.push("remembering displays across a reconnect");
        }
        // Only the genuinely impossible case is a limitation. "A download
        // away" is not something missing from the session, it is something
        // the reader has not asked for yet, and listing it here would nag.
        if matches!(self.speech, Speech::Unavailable { .. }) {
            out.push("reading the document aloud");
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
            printing: true,
            print_options: true,
            system_print_dialog: true,
            arbitrary_placement: true,
            system_appearance: true,
            sleep_inhibition: true,
            native_dialogs: true,
            identity: IdentityQuality::Stable,
            speech: Speech::Ready { voices: 1 },
            ..Capabilities::default()
        };
        assert!(full.limitations().is_empty());
    }

    #[test]
    fn speech_distinguishes_impossible_from_not_yet_downloaded() {
        // The whole reason this is not a bool: the two "no" answers have
        // different remedies, and only one of them is a limitation of the
        // session.
        let impossible = Capabilities {
            speech: Speech::Unavailable {
                why: "this session has no audio output".into(),
            },
            ..Capabilities::default()
        };
        assert!(!impossible.speech.can_speak());
        assert!(!impossible.speech.can_download());
        assert!(impossible
            .limitations()
            .contains(&"reading the document aloud"));

        let one_download_away = Capabilities {
            speech: Speech::Downloadable {
                bytes: 63_000_000,
                needs_engine: true,
            },
            ..Capabilities::default()
        };
        assert!(!one_download_away.speech.can_speak());
        assert!(one_download_away.speech.can_download());
        assert!(
            !one_download_away
                .limitations()
                .contains(&"reading the document aloud"),
            "a download the reader has not asked for is not a limitation"
        );

        let ready = Capabilities {
            speech: Speech::Ready { voices: 2 },
            ..Capabilities::default()
        };
        assert!(ready.speech.can_speak());
        assert!(ready.report().join("\n").contains("speech: ready"));
    }

    #[test]
    fn the_report_contains_no_c1_control_characters() {
        // A prior version's em dashes and ellipses were UTF-8 bytes that
        // had been decoded once too many times: valid UTF-8, but decoding
        // to C1 control characters (U+0080-U+009F) rather than the
        // punctuation intended. This bundle goes into bug reports verbatim,
        // so every line must be made only of printable characters (plus
        // ordinary whitespace).
        let capabilities = Capabilities {
            speech: Speech::Ready { voices: 1 },
            ..Capabilities::default()
        };
        for line in capabilities.report() {
            for ch in line.chars() {
                assert!(
                    !ch.is_control() || ch == ' ',
                    "report line {line:?} contains a control character {ch:?} (U+{:04X})",
                    ch as u32
                );
            }
        }
    }

    #[test]
    fn identity_quality_is_ordered_weakest_first() {
        assert!(IdentityQuality::None < IdentityQuality::Session);
        assert!(IdentityQuality::Connector < IdentityQuality::Stable);
    }
}
