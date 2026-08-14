//! Media and interactive overlays for pulpit (`docs-src/internals.typ`).
//!
//! This crate owns runtime discovery, capability-based selection, worker
//! supervision, the continuous frame transport and the runtime adapters. It
//! knows nothing about PDF dictionaries — `pulpit-render` interprets
//! those — and nothing about Iced: `pulpit-app` composites and routes
//! input. The two never depend on one another; they exchange pure
//! `pulpit-core` descriptors through the application.
//!
//! The invariants worth stating once, because everything here serves them:
//!
//! * one authoritative session per overlay, consumed by both windows;
//! * the PDF page is always the fallback, and a valid audience frame is never
//!   replaced by a partial one, an error, or a runtime-selection UI;
//! * heavy runtimes live in supervised worker processes, so the main
//!   executable links neither a browser engine nor a media framework.

pub mod capability;
pub mod diagnostics;
pub mod protocol;
pub mod runtime;
pub mod selection;
pub mod supervisor;
pub mod surface;
pub mod worker;

pub use capability::{Availability, Limitation, RuntimeProbe, UnmetRequirement};
pub use diagnostics::{OverlayReport, OverlayStatus, Preflight};
pub use protocol::{
    ImageCommand, InputEvent, MediaError, MediaErrorKind, MediaEvent, MediaRequest, MediaWarning,
    PixelFormat, PointerButton, RuntimeId, SessionId, SessionSource, SessionSpec, SurfaceFrame,
    VideoCommand, Viewport, WebCommand, WorkerCounters, MEDIA_PROTOCOL_VERSION,
};
pub use selection::{RuntimePolicy, Selection};
pub use supervisor::{MediaConfig, MediaSupervisor, SessionEvent, WorkerCommand};
pub use surface::{AttachedRing, SurfaceRing, DEFAULT_SLOTS};

/// Install the tracing subscriber a worker binary uses.
///
/// Workers log to stderr, which the supervisor inherits, because stdout is
/// the protocol stream and anything printed there would corrupt it.
pub fn init_worker_logging(runtime: RuntimeId) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("PULPIT_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
    tracing::debug!(runtime = %runtime, "worker starting");
}
