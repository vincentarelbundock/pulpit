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
