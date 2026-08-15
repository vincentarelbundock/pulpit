//! The display-extension contract.
//!
//! Application code never sees a native monitor handle. A backend produces
//! immutable snapshots and reports what placement the platform actually
//! supports; the caller turns [`crate::reconcile::Action`]s into requests and
//! receives an explicit [`PlacementOutcome`].

use crate::identity::MonitorIdentity;
use crate::reconcile::{Capabilities, WindowMode};
use crate::snapshot::DisplaySnapshot;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("no display backend is available for this session")]
    Unavailable,
    #[error("display server error: {0}")]
    Protocol(String),
}

/// Explicit result of a placement request. Never a silent success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementOutcome {
    /// The request was applied.
    Applied,
    /// The compositor refused it (policy, tiling WM, ...).
    Refused,
    /// The target monitor disappeared between snapshot and request. This is a
    /// normal topology race: reconcile again from a fresh snapshot.
    Disappeared,
    /// The platform does not implement the request at all.
    Unsupported,
    Failed(String),
}

/// A monitor-enumeration and placement adapter.
pub trait DisplayBackend: Send + Sync {
    fn name(&self) -> &'static str;

    /// Enumerate a fresh snapshot. Called on every reconciliation; must be
    /// cheap enough to poll.
    fn snapshot(&self) -> Result<DisplaySnapshot, BackendError>;

    fn capabilities(&self) -> Capabilities;

    /// Give a window the keyboard focus.
    ///
    /// The toolkit's own focus request is advisory and, on Wayland, usually
    /// ignored: a client cannot activate itself without a token it was never
    /// given. A compositor IPC adapter can do it properly, which is what keeps
    /// the presenter in front after the audience window is fullscreened.
    fn focus(&self, _window: NativeWindow) -> PlacementOutcome {
        PlacementOutcome::Unsupported
    }

    /// Place a window. `window` is an opaque native window id supplied by the
    /// UI layer (an X11 window id, a `wl_surface`-owning toplevel, ...). No
    /// handle is retained after the call returns.
    fn place(
        &self,
        window: NativeWindow,
        identity: &MonitorIdentity,
        mode: WindowMode,
    ) -> PlacementOutcome;
}

/// An opaque backend-specific window reference, valid only for the duration
/// of a call. Usually this is a native X11 id; compositor IPC backends may use
/// a short-lived role token and resolve the compositor's real id on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeWindow(pub u64);

/// A backend for sessions with no display server (CI, tests).
#[derive(Debug, Default)]
pub struct NullBackend {
    pub snapshot: DisplaySnapshot,
}

impl DisplayBackend for NullBackend {
    fn name(&self) -> &'static str {
        "null"
    }

    fn snapshot(&self) -> Result<DisplaySnapshot, BackendError> {
        Ok(self.snapshot.clone())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            arbitrary_position: false,
            unfullscreen_safe: true,
            place_before_map: false,
        }
    }

    fn place(&self, _: NativeWindow, _: &MonitorIdentity, _: WindowMode) -> PlacementOutcome {
        PlacementOutcome::Unsupported
    }
}

/// Records every placement request so integration tests can assert on them.
#[derive(Debug, Default)]
pub struct RecordingBackend {
    inner: NullBackend,
    pub requests: std::sync::Mutex<Vec<(NativeWindow, MonitorIdentity, WindowMode)>>,
    pub result: PlacementOutcomeStub,
}

#[derive(Debug, Default, Clone, Copy)]
pub enum PlacementOutcomeStub {
    #[default]
    Applied,
    Refused,
    Disappeared,
}

impl RecordingBackend {
    pub fn new(snapshot: DisplaySnapshot) -> Self {
        Self {
            inner: NullBackend { snapshot },
            ..Self::default()
        }
    }

    pub fn requests(&self) -> Vec<(NativeWindow, MonitorIdentity, WindowMode)> {
        self.requests.lock().unwrap().clone()
    }
}

impl DisplayBackend for RecordingBackend {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn snapshot(&self) -> Result<DisplaySnapshot, BackendError> {
        self.inner.snapshot()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::X11
    }

    fn place(
        &self,
        window: NativeWindow,
        identity: &MonitorIdentity,
        mode: WindowMode,
    ) -> PlacementOutcome {
        self.requests
            .lock()
            .unwrap()
            .push((window, identity.clone(), mode));
        match self.result {
            PlacementOutcomeStub::Applied => PlacementOutcome::Applied,
            PlacementOutcomeStub::Refused => PlacementOutcome::Refused,
            PlacementOutcomeStub::Disappeared => PlacementOutcome::Disappeared,
        }
    }
}
