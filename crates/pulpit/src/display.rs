//! Glue between Iced windows and the display extension.
//!
//! Iced 0.14 exposes no monitor enumeration, so this module resolves a native
//! window handle and hands it to a platform adapter for the duration of one
//! call — no native handle is ever stored in application state.
//!
//! Adapter selection (the one per-operating-system `cfg` above the platform
//! boundary) and the native-window plumbing it needs live in
//! [`crate::platform::display`] and are re-exported here.

use std::sync::Arc;

use iced::window;
use pulpit_display::backend::NativeWindow;
use pulpit_display::{
    Capabilities, DisplayBackend, DisplayRoles, DisplaySnapshot, PlacementOutcome, Reconciler,
    Role, WindowMode, WindowState, Windows,
};

pub use crate::platform::display::{detect_backend, identify_window, native_window_id};

/// What [`DisplayCoordinator::refresh`] found.
///
/// A prior version returned `bool`, so "the backend could not enumerate
/// monitors" and "it enumerated the same topology as before" both read as
/// `false` to the caller — indistinguishable, though the first is a fault
/// worth surfacing and the second is the ordinary case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The topology changed since the last snapshot.
    Changed,
    /// The topology is the same as it was.
    Unchanged,
    /// The backend's `snapshot()` call failed; the previous snapshot is kept
    /// rather than replaced with an empty one.
    EnumerationFailed,
}

/// Everything display-related that the application owns.
pub struct DisplayCoordinator {
    pub backend: Arc<dyn DisplayBackend>,
    pub capabilities: Capabilities,
    pub reconciler: Reconciler,
    pub windows: Windows,
    pub snapshot: DisplaySnapshot,
    pub roles: DisplayRoles,
    /// Which monitor each role resolved to at the last reconciliation.
    pub resolved: pulpit_display::ResolvedRoles,
    /// Native window ids, refreshed from the toolkit, never persisted.
    presenter_native: Option<NativeWindow>,
    audience_native: Option<NativeWindow>,
}

impl std::fmt::Debug for DisplayCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayCoordinator")
            .field("backend", &self.backend.name())
            .field("monitors", &self.snapshot.len())
            .finish_non_exhaustive()
    }
}

impl DisplayCoordinator {
    pub fn new(roles: DisplayRoles) -> Self {
        let (backend, capabilities) = detect_backend();
        Self {
            capabilities,
            backend,
            reconciler: Reconciler::new(),
            windows: Windows::default(),
            snapshot: DisplaySnapshot::default(),
            roles,
            resolved: pulpit_display::ResolvedRoles::default(),
            presenter_native: None,
            audience_native: None,
        }
    }

    pub fn set_native(&mut self, role: Role, native: Option<NativeWindow>) {
        match role {
            Role::Presenter => self.presenter_native = native,
            Role::Audience => self.audience_native = native,
        }
    }

    pub fn native(&self, role: Role) -> Option<NativeWindow> {
        match role {
            Role::Presenter => self.presenter_native,
            Role::Audience => self.audience_native,
        }
    }

    /// Take a fresh snapshot. Called on every topology hint and on a poll
    /// interval; a snapshot is never cached across a reconciliation.
    pub fn refresh(&mut self) -> RefreshOutcome {
        match self.backend.snapshot() {
            Ok(snapshot) => {
                let changed = !snapshot.same_topology(&self.snapshot);
                self.snapshot = snapshot;
                if changed {
                    RefreshOutcome::Changed
                } else {
                    RefreshOutcome::Unchanged
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot enumerate monitors");
                RefreshOutcome::EnumerationFailed
            }
        }
    }

    pub fn window_state_mut(&mut self, role: Role) -> &mut WindowState {
        self.windows.get_mut(role)
    }
}

/// Map the domain's window mode onto Iced's.
pub fn iced_mode(mode: WindowMode) -> window::Mode {
    match mode {
        WindowMode::Windowed => window::Mode::Windowed,
        WindowMode::Fullscreen => window::Mode::Fullscreen,
    }
}

pub fn describe_placement(outcome: &PlacementOutcome) -> Option<String> {
    match outcome {
        PlacementOutcome::Applied => None,
        PlacementOutcome::Pending => None,
        PlacementOutcome::Refused => {
            Some("the compositor refused to place that window; move it yourself".into())
        }
        PlacementOutcome::Disappeared => {
            Some("that display disappeared while it was being assigned".into())
        }
        PlacementOutcome::Unsupported => Some(
            "this session cannot place windows on a chosen display; use the compositor's \
             own controls"
                .into(),
        ),
        PlacementOutcome::Failed(reason) => Some(format!("placement failed: {reason}")),
    }
}
