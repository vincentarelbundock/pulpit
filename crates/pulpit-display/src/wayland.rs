//! Wayland output adapter.
//!
//! Glue over `smithay-client-toolkit`, not raw protocol code: `wl_output`
//! plus `zxdg_output_v1` give names, descriptions, logical geometry and scale,
//! and the registry gives add/remove events.
//!
//! # What this can and cannot do
//!
//! It **enumerates and identifies** outputs, and reports topology changes. It
//! **cannot place windows**. Choosing the output for the audience window is
//! not something the application attempts on any platform: the window goes
//! fullscreen on whichever output it is already on, the user is told, and the
//! result stays recoverable — which is exactly the capability-aware contract.
//!
//! Because this adapter opens its own Wayland connection, its outputs cannot
//! be correlated with winit's windows by object identity. They are correlated
//! by **name** (`HDMI-A-1`, `eDP-1`, …), which is stable within a session and
//! is what the user sees in the display selector.

use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_output;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};

use crate::backend::{BackendError, DisplayBackend, NativeWindow, PlacementOutcome};
use crate::identity::MonitorIdentity;
use crate::reconcile::{Capabilities, WindowMode};
use crate::snapshot::{is_builtin_connector, DisplaySnapshot, Monitor, Rect};

/// Stable app ids assigned to the two top-level windows on Linux. They are not
/// used for placement — the compositor owns that — but they let the user, and
/// any compositor rule the user writes themselves, tell the two windows apart.
pub const PRESENTER_APP_ID: &str = "pulpit";
pub const AUDIENCE_APP_ID: &str = "pulpit-audience";

/// Dispatch state for the private Wayland connection.
struct Outputs {
    registry: RegistryState,
    outputs: OutputState,
}

impl OutputHandler for Outputs {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    // The three callbacks below are deliberately empty. `OutputState` has
    // already folded the event in by the time we are called, and enumeration
    // is authoritative: the listener's only question is whether the
    // connection said anything at all, which it answers from the descriptor.
    // A change counter used to live here, and nothing ever read it.

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for Outputs {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState];
}

delegate_dispatch2!(Outputs);
delegate_registry!(Outputs);

pub struct WaylandBackend {
    connection: Connection,
    queue: Mutex<EventQueue<Outputs>>,
    state: Mutex<Outputs>,
    sequence: AtomicU64,
}

impl std::fmt::Debug for WaylandBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaylandBackend").finish_non_exhaustive()
    }
}

impl WaylandBackend {
    pub fn connect() -> Result<Self, BackendError> {
        let connection = Connection::connect_to_env().map_err(|e| {
            BackendError::Protocol(format!("cannot connect to the Wayland display: {e}"))
        })?;
        let (globals, mut queue) = registry_queue_init::<Outputs>(&connection)
            .map_err(|e| BackendError::Protocol(format!("Wayland registry: {e}")))?;
        let handle = queue.handle();

        let mut state = Outputs {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &handle),
        };
        // One round trip so the initial output set, including xdg-output
        // logical geometry, has arrived before the first snapshot.
        queue
            .roundtrip(&mut state)
            .map_err(|e| BackendError::Protocol(format!("Wayland round trip: {e}")))?;
        queue
            .roundtrip(&mut state)
            .map_err(|e| BackendError::Protocol(format!("Wayland round trip: {e}")))?;

        Ok(Self {
            connection,
            queue: Mutex::new(queue),
            state: Mutex::new(state),
            sequence: AtomicU64::new(0),
        })
    }

    /// Process any pending compositor events. Cheap; called before every
    /// enumeration so a snapshot is always current.
    fn pump(&self) -> Result<(), BackendError> {
        let mut queue = self.queue.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        queue
            .roundtrip(&mut state)
            .map_err(|e| BackendError::Protocol(format!("Wayland dispatch: {e}")))?;
        Ok(())
    }

    /// Wait until the compositor has something to say, holding no lock while
    /// waiting.
    ///
    /// This is what makes the adapter's mutexes safe for the event-loop
    /// thread to contend for. Every read of the socket happens inside
    /// `roundtrip`, which is bounded request/response work; the open-ended
    /// part — sitting on a connection that may never speak again — happens
    /// here, on the raw descriptor, with no lock held. An earlier version
    /// called `blocking_dispatch` with both locks held, and a suspend/resume
    /// that changed no output was enough to park the UI thread forever behind
    /// a listener thread that was itself waiting on a silent compositor.
    ///
    /// `Ok(true)` means the connection is readable and the caller should
    /// re-enumerate; `Ok(false)` means nothing arrived within
    /// [`WAIT_HINT_TIMEOUT`] and the caller should fall back to its own poll
    /// cadence; an error means the connection is unusable, which sends the
    /// caller to that same fallback rather than into a spin.
    pub fn wait_for_change(&self) -> Result<bool, BackendError> {
        // Flush first, so anything another thread queued is on the wire
        // before we wait for a reply to it. `flush` writes what it can and
        // reports `WouldBlock`; it never waits on the compositor. The queue
        // lock is held only for that call, and released before the wait.
        {
            let _queue = self.queue.lock().unwrap();
            self.connection
                .flush()
                .map_err(|e| BackendError::Protocol(format!("Wayland flush: {e}")))?;
        }
        wait_readable(self.connection.as_fd(), WAIT_HINT_TIMEOUT)
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}

/// How long a waiter sits on a silent connection before reporting "no hint".
///
/// The wait has to be bounded. This connection carries registry and output
/// events only, so after a resume that changed nothing the compositor may
/// legitimately say nothing for the rest of the session, and a thread whose
/// progress cannot be observed is a thread whose locks cannot be reasoned
/// about. A couple of seconds is long enough that a quiet compositor costs
/// only an occasional wake-up, and short enough that the listener visibly
/// ticks. When it expires the caller re-enumerates on its own fallback
/// cadence — exactly what it does on a platform with no native listener.
const WAIT_HINT_TIMEOUT: Duration = Duration::from_secs(2);

/// Wait for `fd` to become readable, for at most `timeout`, holding nothing.
///
/// `Ok(true)` is "readable", `Ok(false)` is "the timeout expired, no hint".
/// A hang-up or invalid descriptor is an error *even when data is also
/// pending*: a closed socket stays readable forever, so calling that a hint
/// would turn the caller's loop into a spin, whereas an error drops it onto
/// its slow fallback poll. An interrupted `poll` is likewise reported as "no
/// hint" rather than retried; the caller already handles that answer well,
/// and it keeps this function's own bound honest.
fn wait_readable(fd: BorrowedFd<'_>, timeout: Duration) -> Result<bool, BackendError> {
    let mut pollfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `pollfd` is a single initialised record owned by this frame,
    // and the borrowed descriptor outlives the call.
    let ready = unsafe { libc::poll(&mut pollfd, 1, millis) };
    if ready < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(BackendError::Protocol(format!("poll: {error}")));
    }
    if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return Err(BackendError::Protocol(
            "the Wayland connection was hung up or is no longer valid".into(),
        ));
    }
    Ok(ready > 0 && pollfd.revents & libc::POLLIN != 0)
}

/// A fractional-aware scale factor.
///
/// `wl_output.scale` is an integer, which is a lie on any 1.5x/1.25x
/// fractional-scaling display: the compositor rounds it up (or down) to
/// report *something*, and `scale_checks` (below) then flags every such
/// output as "inconsistent" even though nothing is actually wrong. When both
/// the logical size and the current mode's physical pixels are known, the
/// true scale is their ratio; the integer is only a fallback for when one of
/// those is unavailable. (§77.1)
fn derive_scale_factor(
    logical: Option<(i32, i32)>,
    mode: Option<(i32, i32)>,
    integer_scale: i32,
) -> f64 {
    if let (Some((logical_width, _)), Some((mode_width, _))) = (logical, mode) {
        if logical_width > 0 {
            return mode_width as f64 / logical_width as f64;
        }
    }
    integer_scale.max(1) as f64
}

impl DisplayBackend for WaylandBackend {
    fn name(&self) -> &'static str {
        "wayland-output"
    }

    fn wait_for_topology_change(&self) -> Result<bool, BackendError> {
        WaylandBackend::wait_for_change(self)
    }

    fn snapshot(&self) -> Result<DisplaySnapshot, BackendError> {
        self.pump()?;
        let state = self.state.lock().unwrap();
        let mut monitors = Vec::new();

        for output in state.outputs.outputs() {
            let Some(info) = state.outputs.info(&output) else {
                continue;
            };
            let connector = info
                .name
                .clone()
                .unwrap_or_else(|| format!("wl-output-{}", info.id));
            let make = (!info.make.is_empty()).then(|| info.make.clone());
            let model = (!info.model.is_empty()).then(|| info.model.clone());

            // Logical geometry when xdg-output provides it, otherwise the
            // current mode divided by the buffer scale.
            let current_mode = info
                .modes
                .iter()
                .find(|mode| mode.current)
                .map(|mode| mode.dimensions);
            let (x, y) = info.logical_position.unwrap_or(info.location);
            let (width, height) = match info.logical_size {
                Some((width, height)) => (width.max(0) as u32, height.max(0) as u32),
                None => {
                    let mode = current_mode.unwrap_or((0, 0));
                    let scale = info.scale_factor.max(1);
                    (
                        (mode.0 / scale).max(0) as u32,
                        (mode.1 / scale).max(0) as u32,
                    )
                }
            };
            let scale_factor =
                derive_scale_factor(info.logical_size, current_mode, info.scale_factor);

            let identity = MonitorIdentity::Connector {
                connector: connector.clone(),
                make: make.clone().unwrap_or_default(),
                model: model.clone().unwrap_or_default(),
            };
            let physical = (
                info.physical_size.0.max(0) as u32,
                info.physical_size.1.max(0) as u32,
            );
            let fallback = Some(MonitorIdentity::Geometric {
                make: make.clone().unwrap_or_default(),
                model: model.clone().unwrap_or_default(),
                width_mm: physical.0,
                height_mm: physical.1,
                x,
                y,
            });

            monitors.push(Monitor {
                identity,
                fallback_identity: fallback,
                connector: Some(connector.clone()),
                make,
                model,
                geometry: Rect::new(x, y, width, height),
                scale_factor,
                physical_size_mm: (physical != (0, 0)).then_some(physical),
                builtin: is_builtin_connector(&connector),
                // Wayland has no notion of a primary output, and pretending
                // otherwise is how competitors pick the wrong screen.
                primary: false,
                handle: info.id as u64,
            });
        }

        Ok(DisplaySnapshot::new(
            monitors,
            self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
        ))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // See the module docs: the compositor owns placement.
            arbitrary_position: false,
            unfullscreen_safe: false,
            place_before_map: false,
        }
    }

    fn place(&self, _: NativeWindow, _: &MonitorIdentity, _: WindowMode) -> PlacementOutcome {
        PlacementOutcome::Unsupported
    }
}

/// Check that reported logical size × scale factor equals the current mode's
/// physical pixels, per output.
///
/// Phase 0 asks for this explicitly: toolkit-level scale reporting on Wayland
/// has a history of doubling (pdfpc ships an opt-in workaround for it). Rather
/// than a workaround flag, pulpit measures and reports.
#[derive(Debug, Clone, PartialEq)]
pub struct ScaleCheck {
    pub connector: String,
    pub logical: (u32, u32),
    /// The fractional-aware scale ([`derive_scale_factor`]), not the raw
    /// integer `wl_output.scale`.
    pub scale: f64,
    /// Physical pixels from the current mode, when one is reported.
    pub mode: Option<(u32, u32)>,
    pub consistent: bool,
}

impl ScaleCheck {
    pub fn describe(&self) -> String {
        match self.mode {
            None => format!("{}: no current mode reported", self.connector),
            Some((mw, mh)) if self.consistent => format!(
                "{}: {}×{} × {:.2} = {mw}×{mh} ✓",
                self.connector, self.logical.0, self.logical.1, self.scale
            ),
            Some((mw, mh)) => format!(
                "{}: {}×{} × {:.2} ≠ mode {mw}×{mh} — scale reporting is inconsistent",
                self.connector, self.logical.0, self.logical.1, self.scale
            ),
        }
    }
}

impl WaylandBackend {
    /// Run the logical×scale check for every output.
    pub fn scale_checks(&self) -> Result<Vec<ScaleCheck>, BackendError> {
        self.pump()?;
        let state = self.state.lock().unwrap();
        let mut checks = Vec::new();
        for output in state.outputs.outputs() {
            let Some(info) = state.outputs.info(&output) else {
                continue;
            };
            let connector = info
                .name
                .clone()
                .unwrap_or_else(|| format!("wl-output-{}", info.id));
            let current_mode = info
                .modes
                .iter()
                .find(|mode| mode.current)
                .map(|mode| mode.dimensions);
            let mode =
                current_mode.map(|(width, height)| (width.max(0) as u32, height.max(0) as u32));
            let integer_scale = info.scale_factor.max(1);
            let logical = match info.logical_size {
                Some((width, height)) => (width.max(0) as u32, height.max(0) as u32),
                None => match mode {
                    Some((width, height)) => {
                        (width / integer_scale as u32, height / integer_scale as u32)
                    }
                    None => (0, 0),
                },
            };
            let scale = derive_scale_factor(info.logical_size, current_mode, info.scale_factor);
            // A rounding tolerance of one pixel per dimension: logical units
            // are themselves rounded, so a fractional scale reconstructed
            // from them will not always land exactly back on the mode.
            let consistent = match mode {
                Some((mw, mh)) => {
                    let expected_w = (logical.0 as f64 * scale).round() as i64;
                    let expected_h = (logical.1 as f64 * scale).round() as i64;
                    (expected_w - mw as i64).abs() <= 1 && (expected_h - mh as i64).abs() <= 1
                }
                None => true,
            };
            checks.push(ScaleCheck {
                connector,
                logical,
                scale,
                mode,
                consistent,
            });
        }
        Ok(checks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The waiter's contract, exercised on a socket pair rather than a
    /// compositor: nothing to read means "no hint" after the timeout and not
    /// a moment longer, a byte on the wire means "readable", and a peer that
    /// has gone away is an error even though the descriptor is readable —
    /// which is what keeps the listener loop off a spin when the connection
    /// dies. Needs no display, so it runs in CI.
    #[test]
    fn waiting_for_readability_is_bounded_and_honest() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        use std::time::Instant;

        let (ours, mut theirs) = UnixStream::pair().expect("socket pair");

        let started = Instant::now();
        let timeout = Duration::from_millis(120);
        assert!(
            !wait_readable(ours.as_fd(), timeout).expect("a silent peer is not an error"),
            "a silent peer times out with no hint"
        );
        assert!(
            started.elapsed() >= timeout,
            "the wait is not allowed to return early"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait is bounded by its timeout"
        );

        theirs.write_all(b"x").expect("write a byte");
        assert!(
            wait_readable(ours.as_fd(), timeout).expect("a readable peer is not an error"),
            "a byte on the wire is a hint"
        );

        drop(theirs);
        assert!(
            wait_readable(ours.as_fd(), timeout).is_err(),
            "a hung-up peer is an error, not an endless hint"
        );
    }

    #[test]
    fn scale_checks_report_the_arithmetic_plainly() {
        let good = ScaleCheck {
            connector: "eDP-1".into(),
            logical: (1920, 1200),
            scale: 2.0,
            mode: Some((3840, 2400)),
            consistent: true,
        };
        assert!(good.describe().contains('✓'));

        let doubled = ScaleCheck {
            connector: "eDP-1".into(),
            logical: (3840, 2400),
            scale: 2.0,
            mode: Some((3840, 2400)),
            consistent: false,
        };
        assert!(doubled.describe().contains("inconsistent"));
    }

    #[test]
    fn a_fractional_scale_is_derived_from_mode_over_logical_size() {
        // A 1.5x display: the integer `wl_output.scale` would report 2 (or
        // 1, depending on rounding direction), either of which is wrong.
        assert_eq!(
            derive_scale_factor(Some((1280, 800)), Some((1920, 1200)), 2),
            1.5
        );
        // Neither logical size nor mode known: fall back to the integer.
        assert_eq!(derive_scale_factor(None, None, 2), 2.0);
        // A logical width of zero must not divide by zero.
        assert_eq!(
            derive_scale_factor(Some((0, 800)), Some((1920, 1200)), 2),
            2.0
        );
    }

    #[test]
    fn scale_checks_tolerate_one_pixel_of_rounding() {
        // logical (1280, 800) at the true 1.5x scale reconstructs to exactly
        // the mode; the integer scale of 2 would not.
        let derived = derive_scale_factor(Some((1280, 800)), Some((1920, 1200)), 2);
        assert_eq!(derived, 1.5);
    }

    /// Runs only inside a real Wayland session; skips with a message
    /// elsewhere so CI and X11 machines stay green.
    #[test]
    fn enumerates_outputs_in_a_wayland_session() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            eprintln!("skipping: not a Wayland session");
            return;
        }
        let backend = match WaylandBackend::connect() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping: cannot connect to the compositor: {e}");
                return;
            }
        };
        let snapshot = backend.snapshot().expect("enumerate outputs");
        assert!(
            !snapshot.is_empty(),
            "a Wayland session has at least one output"
        );
        for monitor in &snapshot.monitors {
            assert!(monitor.geometry.width > 0 && monitor.geometry.height > 0);
            assert!(monitor.connector.is_some());
        }
        for check in backend.scale_checks().unwrap() {
            eprintln!("{}", check.describe());
        }
        assert!(
            !backend.capabilities().can_place(),
            "honest capability reporting"
        );
    }
}
