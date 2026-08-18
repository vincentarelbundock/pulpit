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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
    /// Bumped whenever the compositor adds, changes or removes an output.
    revision: Arc<AtomicU64>,
}

impl OutputHandler for Outputs {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
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
    revision: Arc<AtomicU64>,
    sequence: AtomicU64,
    /// Last revision folded into a snapshot, so `has_changed` is cheap.
    seen_revision: AtomicU64,
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

        let revision = Arc::new(AtomicU64::new(0));
        let mut state = Outputs {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &handle),
            revision: Arc::clone(&revision),
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
            revision,
            sequence: AtomicU64::new(0),
            seen_revision: AtomicU64::new(0),
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

    /// True when the compositor reported an output change since the last
    /// call. A hint only: enumeration remains authoritative.
    pub fn has_changed(&self) -> bool {
        let _ = self.pump();
        let current = self.revision.load(Ordering::Relaxed);
        self.seen_revision.swap(current, Ordering::Relaxed) != current
    }

    /// Block until the compositor sends something, so a subscription thread
    /// does not have to spin. Returns on any event, not only output changes.
    pub fn wait_for_change(&self) -> Result<(), BackendError> {
        // `blocking_dispatch` needs both locks; take them in the same order
        // as `pump` to keep the ordering trivially deadlock-free.
        let mut queue = self.queue.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| BackendError::Protocol(format!("Wayland dispatch: {e}")))?;
        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}

impl DisplayBackend for WaylandBackend {
    fn name(&self) -> &'static str {
        "wayland-output"
    }

    fn wait_for_topology_change(&self) -> Result<bool, BackendError> {
        WaylandBackend::wait_for_change(self).map(|()| true)
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
            let (x, y) = info.logical_position.unwrap_or(info.location);
            let (width, height) = match info.logical_size {
                Some((width, height)) => (width.max(0) as u32, height.max(0) as u32),
                None => {
                    let mode = info
                        .modes
                        .iter()
                        .find(|mode| mode.current)
                        .map(|mode| mode.dimensions)
                        .unwrap_or((0, 0));
                    let scale = info.scale_factor.max(1);
                    (
                        (mode.0 / scale).max(0) as u32,
                        (mode.1 / scale).max(0) as u32,
                    )
                }
            };

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
                scale_factor: info.scale_factor.max(1) as f64,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleCheck {
    pub connector: String,
    pub logical: (u32, u32),
    pub scale: i32,
    /// Physical pixels from the current mode, when one is reported.
    pub mode: Option<(u32, u32)>,
    pub consistent: bool,
}

impl ScaleCheck {
    pub fn describe(&self) -> String {
        match self.mode {
            None => format!("{}: no current mode reported", self.connector),
            Some((mw, mh)) if self.consistent => format!(
                "{}: {}×{} × {} = {mw}×{mh} ✓",
                self.connector, self.logical.0, self.logical.1, self.scale
            ),
            Some((mw, mh)) => format!(
                "{}: {}×{} × {} ≠ mode {mw}×{mh} — scale reporting is inconsistent",
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
            let scale = info.scale_factor.max(1);
            let mode = info.modes.iter().find(|mode| mode.current).map(|mode| {
                (
                    mode.dimensions.0.max(0) as u32,
                    mode.dimensions.1.max(0) as u32,
                )
            });
            let logical = match info.logical_size {
                Some((width, height)) => (width.max(0) as u32, height.max(0) as u32),
                None => match mode {
                    Some((width, height)) => (width / scale as u32, height / scale as u32),
                    None => (0, 0),
                },
            };
            let consistent = match mode {
                Some((mw, mh)) => logical.0 * scale as u32 == mw && logical.1 * scale as u32 == mh,
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

    #[test]
    fn scale_checks_report_the_arithmetic_plainly() {
        let good = ScaleCheck {
            connector: "eDP-1".into(),
            logical: (1920, 1200),
            scale: 2,
            mode: Some((3840, 2400)),
            consistent: true,
        };
        assert!(good.describe().contains('✓'));

        let doubled = ScaleCheck {
            connector: "eDP-1".into(),
            logical: (3840, 2400),
            scale: 2,
            mode: Some((3840, 2400)),
            consistent: false,
        };
        assert!(doubled.describe().contains("inconsistent"));
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
