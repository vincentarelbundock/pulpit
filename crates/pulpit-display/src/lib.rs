//! Display coordination: identity, snapshots, roles and the single
//! idempotent reconciliation function.
//!
//! This crate is the heart of pulpit's promise. All of its decision
//! logic is pure and testable without a graphical session; only the
//! `backend` implementations touch a display server.

pub mod backend;
pub mod identity;
pub mod reconcile;
pub mod roles;
pub mod scenario;
pub mod snapshot;

#[cfg(all(feature = "wayland", unix, not(target_os = "macos")))]
pub mod wayland;
#[cfg(all(feature = "x11", unix, not(target_os = "macos")))]
pub mod x11;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

pub use backend::{BackendError, DisplayBackend, NativeWindow, NullBackend, PlacementOutcome};
pub use identity::{IdentityRecord, IdentityTier, MonitorIdentity, IDENTITY_SCHEMA};
pub use reconcile::{
    apply_outcome, reconcile, Action, Capabilities, Outcome, Reconciler, Reconciliation,
    ResolvedRoles, Warning, WindowMode, WindowState, Windows,
};
pub use roles::{DisplayRoles, Role, RoleTarget};
pub use scenario::{capture, Scenario, Step};
pub use snapshot::{
    is_builtin_connector, DisplaySnapshot, MirrorGroup, Monitor, Overlap, Rect, Resolution,
};
