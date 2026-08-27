//! Desktop services: dialogs, opening things, notifications, appearance,
//! inhibition and where files live.

use std::path::{Path, PathBuf};

use crate::platform::appearance::{MotionPreference, SystemAppearance};
use crate::platform::capabilities::Capabilities;
use crate::platform::inhibit::InhibitState;
use crate::platform::paths::Directories;
use crate::platform::Outcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Urgency {
    #[default]
    Normal,
    /// Something the presenter must know even if the window is not focused.
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
}

/// One document, on its way to a printer.
///
/// The pages are a *request*. An adapter whose spooler cannot take a range
/// must say [`Outcome::Unsupported`] rather than print the whole document:
/// forty pages when four were asked for is not a partial success, and the
/// reader finds out at the printer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintJob {
    /// The file to spool. Always a PDF, and always one this process may read
    /// until the call returns.
    pub file: PathBuf,
    /// What the queue should call the job — the document's name, never the
    /// scratch file's.
    pub title: String,
    /// The pages wanted, one-based. Empty means the whole document.
    pub pages: Vec<std::ops::RangeInclusive<u32>>,
    pub copies: u16,
    /// The queue to send to, or `None` for the platform's default.
    pub destination: Option<String>,
}

impl PrintJob {
    /// The range in the spelling CUPS reads: `1-3,7`. `None` for everything.
    pub fn cups_range(&self) -> Option<String> {
        cups_range(&self.pages)
    }
}

/// Page ranges in the spelling CUPS reads: `1-3,7,9-11`.
///
/// `None` for an empty list, so a caller passes no range argument at all
/// rather than one naming every page. Written once, here, because the print
/// dialog needs the same string to show as the adapter needs to send.
pub fn cups_range(pages: &[std::ops::RangeInclusive<u32>]) -> Option<String> {
    if pages.is_empty() {
        return None;
    }
    Some(
        pages
            .iter()
            .map(|range| {
                if range.start() == range.end() {
                    range.start().to_string()
                } else {
                    format!("{}-{}", range.start(), range.end())
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// What the desktop can do for the application.
///
/// Every method returns an explicit [`Outcome`] or an `Option`, so a caller
/// cannot mistake "nothing happened" for success.
pub trait PlatformServices: Send + Sync {
    /// Adapter name, for diagnostics.
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    fn directories(&self) -> Directories;

    /// The system light/dark/high-contrast preference, if readable.
    fn system_appearance(&self) -> SystemAppearance;

    /// Whether the desktop asks applications to keep motion to a minimum.
    ///
    /// Defaults to "no preference expressed" rather than "motion is fine":
    /// an adapter that cannot read the setting must not answer on the user's
    /// behalf.
    fn reduced_motion(&self) -> MotionPreference {
        MotionPreference::Unknown
    }

    /// Reveal a file in the desktop's file manager.
    fn reveal(&self, path: &Path) -> Outcome;

    /// Open a URL or file with the desktop's default handler.
    fn open(&self, target: &str) -> Outcome;

    /// Post a desktop notification. Never used for the audience window, and
    /// never the only record of a failure.
    fn notify(&self, notification: &Notification) -> Outcome;

    /// Put an image on the system clipboard.
    ///
    /// Here rather than on the toolkit's clipboard because Iced's carries
    /// text and nothing else. The default is [`Outcome::Unsupported`], so an
    /// adapter that has not been taught this — and the null one, which must
    /// never touch a real clipboard — reports a command to disable rather
    /// than a copy that silently did nothing.
    fn copy_image(&self, _image: &crate::platform::clipboard::ClipboardImage) -> Outcome {
        Outcome::Unsupported {
            what: "copying an image to the clipboard",
        }
    }

    /// The printers this session can send a job to, most-preferred first.
    ///
    /// Empty means "ask the platform for its own default", not "there are no
    /// printers": a session with a spooler but no way to enumerate queues
    /// still prints, and a dialog offering no choice is the right thing to
    /// show for it.
    fn printers(&self) -> Vec<String> {
        Vec::new()
    }

    /// Send a PDF to a printer.
    ///
    /// pulpit hands the file over rather than rasterising pages and pushing
    /// bitmaps: duplex, paper sizes, trays, margins and colour management are
    /// the platform's and are not improved by being written again here. See
    /// [`crate::printing`] for the whole of that reasoning.
    ///
    /// The default is [`Outcome::Unsupported`], so a desktop that has not
    /// been taught to print — and the null adapter, which must never send
    /// anything to a real printer — reports a command to disable rather than
    /// a job that went nowhere.
    fn print(&self, _job: &PrintJob) -> Outcome {
        Outcome::Unsupported { what: "printing" }
    }

    /// Put the *platform's* print dialog up, and spool whatever it says.
    ///
    /// Called instead of [`PlatformServices::print`] when the session has a
    /// system print dialog to put up, which is what
    /// [`crate::platform::Capabilities::system_print_dialog`] answers. What
    /// pulpit contributes is [`PrintJob::file`] and [`PrintJob::title`]; the
    /// pages, the copies and the queue are the dialog's to ask for, so those
    /// fields are not read here. They are for the spooler-only path, where
    /// pulpit does the asking because nothing else will.
    ///
    /// **This blocks while a reader looks at a modal dialog.** It must never
    /// be called from the event loop — both windows would freeze behind the
    /// dialog, and the audience window is one of them. [`crate::app`] runs it
    /// on a thread of its own and takes the answer back as a message.
    ///
    /// [`Outcome::Refused`] is a reader who pressed Cancel, and is not an
    /// error to report as one.
    fn print_with_dialog(&self, _job: &PrintJob) -> Outcome {
        Outcome::Unsupported {
            what: "the system print dialog",
        }
    }

    /// Whether [`PlatformServices::print_with_dialog`] has to be called on the
    /// thread that owns the event loop.
    ///
    /// AppKit says yes: `runOperation` runs its panel modally and refuses to
    /// be driven from anywhere but the main thread. The portal says no, and is
    /// the better for it — a portal print is a D-Bus round trip, and the
    /// application keeps drawing while the reader thinks about the dialog.
    ///
    /// Where this is true the application calls in place and its own drawing
    /// stops until the panel closes. That is a real cost, taken because the
    /// alternative on that platform is not opening the panel at all.
    fn print_dialog_wants_main_thread(&self) -> bool {
        false
    }

    /// Begin inhibiting sleep and idle.
    fn inhibit(&self) -> InhibitState;

    /// Release whatever [`PlatformServices::inhibit`] took.
    fn release_inhibit(&self, state: &InhibitState) -> Outcome;

    /// A best-effort list of recently used documents the platform knows
    /// about. `None` when the platform has no such notion.
    fn recent_documents(&self) -> Option<Vec<PathBuf>> {
        None
    }
}

/// Hand a URI or path to a desktop helper program, detached.
///
/// The backends that have one of these (`xdg-open`, `open`, …) all want the
/// same thing: no inherited stdio, and a missing program reported as
/// [`Outcome::Unsupported`] rather than a failure, because "this desktop does
/// not ship that helper" is not an error the reader can act on.
pub(crate) fn spawn_detached(program: &str, arguments: &[&str]) -> Outcome {
    match std::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => Outcome::Done,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Outcome::Unsupported {
            what: "This desktop integration",
        },
        Err(e) => Outcome::failed(e.to_string()),
    }
}
