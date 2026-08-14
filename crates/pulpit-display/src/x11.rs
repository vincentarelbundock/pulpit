//! X11/XRandR display adapter.
//!
//! Glue over `x11rb`, not raw protocol code: enumeration through XRandR,
//! identity from EDID where the driver exposes it, topology change events
//! from `RRScreenChangeNotify`, and EWMH fullscreen for placement.
//!
//! Everything here is a *notification and enumeration* source. A fresh
//! enumeration is always authoritative; no handle survives a call.

use std::sync::Mutex;

use x11rb::connection::Connection;
use x11rb::protocol::randr::{
    self, ConnectionExt as _, GetOutputInfoReply, NotifyMask, Output, ScreenSize,
};
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, StackMode, Window,
};
use x11rb::rust_connection::RustConnection;

use crate::backend::{BackendError, DisplayBackend, NativeWindow, PlacementOutcome};
use crate::identity::MonitorIdentity;
use crate::reconcile::{Capabilities, WindowMode};
use crate::snapshot::{is_builtin_connector, DisplaySnapshot, Monitor, Rect};

pub struct X11Backend {
    connection: RustConnection,
    root: Window,
    sequence: Mutex<u64>,
    /// Global scale hint (`Xft.dpi` / 96). X11 has no per-monitor scale; the
    /// value is reported so mixed-DPI behaviour is at least visible in
    /// diagnostics rather than silently wrong.
    scale_factor: f64,
    /// The `EDID` atom, interned once. Enumeration runs on a poll and read
    /// the atom again for every output on every pass — one avoidable X
    /// round trip per monitor per second.
    edid_atom: Option<Atom>,
    /// The running window manager's name, from EWMH. **Diagnostics only**:
    /// nothing branches on it. Which window manager this is does not determine
    /// what it does, so the name is reported in bug reports and never consulted
    /// by a decision.
    window_manager: Option<String>,
    /// What this session's window manager has actually been observed to do
    /// with a placement request.
    trust: Mutex<PlacementTrust>,
}

/// Whether placement requests survive contact with the window manager.
///
/// A stacking manager applies a move; a tiling one applies it and immediately
/// overrides it. No list of names can tell the two apart — an unknown manager
/// released tomorrow belongs to one camp or the other and says so only by what
/// it does. So the answer is measured once, from the first real placement, and
/// latched for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementTrust {
    /// Nothing has been placed yet. Assume the ordinary case.
    Untested,
    /// A placement was observed to stick.
    Honoured,
    /// A placement was observed to be discarded. The window manager owns
    /// layout; the application asks the user instead.
    Ignored,
}

impl std::fmt::Debug for X11Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Backend")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl X11Backend {
    pub fn connect() -> Result<Self, BackendError> {
        let (connection, screen_num) =
            x11rb::connect(None).map_err(|e| BackendError::Protocol(e.to_string()))?;
        let root = connection.setup().roots[screen_num].root;
        connection
            .randr_query_version(1, 5)
            .map_err(|e| BackendError::Protocol(e.to_string()))?
            .reply()
            .map_err(|_| BackendError::Unavailable)?;
        let scale_factor = read_xft_dpi(&connection, root).unwrap_or(96.0) / 96.0;
        let edid_atom = connection
            .intern_atom(true, b"EDID")
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.atom);
        let window_manager = read_window_manager_name(&connection, root);
        Ok(Self {
            connection,
            root,
            sequence: Mutex::new(0),
            scale_factor,
            edid_atom,
            window_manager,
            trust: Mutex::new(PlacementTrust::Untested),
        })
    }

    /// The window manager's name, when it announces one.
    pub fn window_manager(&self) -> Option<&str> {
        self.window_manager.as_deref()
    }

    /// Ask the server to deliver topology-change events on this connection.
    /// The caller runs [`Self::wait_for_change`] on its own thread and turns
    /// each event into an application message.
    pub fn subscribe_to_topology_changes(&self) -> Result<(), BackendError> {
        self.connection
            .randr_select_input(
                self.root,
                NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE,
            )
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        self.connection
            .flush()
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        Ok(())
    }

    /// Block until the server reports a topology change. Notifications are
    /// hints only: the caller re-enumerates.
    pub fn wait_for_change(&self) -> Result<(), BackendError> {
        self.connection
            .wait_for_event()
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        // Drain the burst; a settle delay is applied by the caller.
        while self
            .connection
            .poll_for_event()
            .map_err(|e| BackendError::Protocol(e.to_string()))?
            .is_some()
        {}
        Ok(())
    }

    fn next_sequence(&self) -> u64 {
        let mut sequence = self.sequence.lock().unwrap();
        *sequence += 1;
        *sequence
    }

    fn enumerate(&self) -> Result<Vec<Monitor>, BackendError> {
        let resources = self
            .connection
            .randr_get_screen_resources_current(self.root)
            .map_err(|e| BackendError::Protocol(e.to_string()))?
            .reply()
            .map_err(|e| BackendError::Protocol(e.to_string()))?;

        let primary = self
            .connection
            .randr_get_output_primary(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.output);

        let mut monitors = Vec::new();
        for output in resources.outputs.iter().copied() {
            let Ok(info) = self
                .connection
                .randr_get_output_info(output, resources.config_timestamp)
                .map(|c| c.reply())
            else {
                continue;
            };
            let Ok(info) = info else { continue };
            if info.connection != randr::Connection::CONNECTED || info.crtc == 0 {
                continue;
            }
            let Ok(crtc) = self
                .connection
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)
                .map(|c| c.reply())
            else {
                continue;
            };
            let Ok(crtc) = crtc else { continue };

            let connector = String::from_utf8_lossy(&info.name).to_string();
            let edid = self.read_edid(output);
            let (make, model, serial) = edid
                .as_deref()
                .map(parse_edid)
                .unwrap_or((None, None, None));

            let identity = match (&serial, &make, &model) {
                (Some(serial), _, _) => MonitorIdentity::Stable { id: serial.clone() },
                (None, Some(make), Some(model)) => MonitorIdentity::Connector {
                    connector: connector.clone(),
                    make: make.clone(),
                    model: model.clone(),
                },
                _ => MonitorIdentity::Connector {
                    connector: connector.clone(),
                    make: String::new(),
                    model: String::new(),
                },
            };
            let fallback = Some(MonitorIdentity::Connector {
                connector: connector.clone(),
                make: make.clone().unwrap_or_default(),
                model: model.clone().unwrap_or_default(),
            })
            .filter(|f| f != &identity);

            monitors.push(Monitor {
                identity,
                fallback_identity: fallback,
                connector: Some(connector.clone()),
                make,
                model,
                geometry: Rect::new(
                    crtc.x as i32,
                    crtc.y as i32,
                    crtc.width as u32,
                    crtc.height as u32,
                ),
                scale_factor: self.scale_factor,
                physical_size_mm: Some((info.mm_width, info.mm_height)),
                builtin: is_builtin_connector(&connector),
                primary: primary == Some(output),
                handle: output as u64,
            });
        }
        Ok(monitors)
    }

    fn read_edid(&self, output: Output) -> Option<Vec<u8>> {
        let atom = self.edid_atom?;
        let reply = self
            .connection
            .randr_get_output_property(output, atom, AtomEnum::ANY, 0, 256, false, false)
            .ok()?
            .reply()
            .ok()?;
        (reply.data.len() >= 128).then_some(reply.data)
    }

    fn set_fullscreen(&self, window: Window, fullscreen: bool) -> Result<(), BackendError> {
        let wm_state = self.atom(b"_NET_WM_STATE")?;
        let fs = self.atom(b"_NET_WM_STATE_FULLSCREEN")?;
        let action = u32::from(fullscreen); // 0 = remove, 1 = add
        let event = ClientMessageEvent::new(32, window, wm_state, [action, fs, 0, 1, 0]);
        self.connection
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                event,
            )
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        self.connection
            .flush()
            .map_err(|e| BackendError::Protocol(e.to_string()))?;
        Ok(())
    }

    /// Where the window actually is, in root coordinates.
    ///
    /// `translate_coordinates` rather than `get_geometry` alone: a window
    /// manager reparents its clients, so a window's own geometry is relative
    /// to a frame the application never sees.
    ///
    /// `None` when the window is not viewable, which is not evidence about the
    /// window manager — an unmapped window has no position to honour — and so
    /// must never latch a verdict.
    fn observed_geometry(&self, window: Window) -> Option<Rect> {
        let attributes = self
            .connection
            .get_window_attributes(window)
            .ok()?
            .reply()
            .ok()?;
        if attributes.map_state != xproto::MapState::VIEWABLE {
            return None;
        }
        let geometry = self.connection.get_geometry(window).ok()?.reply().ok()?;
        let origin = self
            .connection
            .translate_coordinates(window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        Some(Rect::new(
            origin.dst_x as i32,
            origin.dst_y as i32,
            geometry.width as u32,
            geometry.height as u32,
        ))
    }

    /// Wait for the window manager to have its say, then report where the
    /// window ended up.
    ///
    /// A manager that honours the request usually settles on the first look,
    /// so the common case pays the shortest delay. One that overrides it costs
    /// the whole budget exactly once per session: the verdict is latched and
    /// no later placement pays it again.
    fn settle(&self, window: Window, target: &Rect) -> Option<bool> {
        const STEPS_MS: [u64; 3] = [20, 50, 100];
        let mut seen = None;
        for step in STEPS_MS {
            std::thread::sleep(std::time::Duration::from_millis(step));
            let Some(observed) = self.observed_geometry(window) else {
                continue;
            };
            if landed_on(&observed, target) {
                return Some(true);
            }
            seen = Some(observed);
        }
        // Never viewable throughout: no evidence either way.
        seen.map(|_| false)
    }

    fn record_trust(&self, honoured: bool) {
        let mut trust = self.trust.lock().unwrap();
        let verdict = if honoured {
            PlacementTrust::Honoured
        } else {
            PlacementTrust::Ignored
        };
        if *trust != verdict {
            tracing::info!(
                window_manager = self.window_manager.as_deref().unwrap_or("unknown"),
                honoured,
                "observed how this window manager treats placement requests"
            );
        }
        *trust = verdict;
    }

    fn atom(&self, name: &[u8]) -> Result<u32, BackendError> {
        Ok(self
            .connection
            .intern_atom(false, name)
            .map_err(|e| BackendError::Protocol(e.to_string()))?
            .reply()
            .map_err(|e| BackendError::Protocol(e.to_string()))?
            .atom)
    }
}

impl DisplayBackend for X11Backend {
    fn name(&self) -> &'static str {
        "x11-randr"
    }

    fn snapshot(&self) -> Result<DisplaySnapshot, BackendError> {
        let monitors = self.enumerate()?;
        Ok(DisplaySnapshot::new(monitors, self.next_sequence()))
    }

    fn capabilities(&self) -> Capabilities {
        // Claiming a placement that never happens is the one failure the
        // display contract exists to prevent, so a manager observed to discard
        // requests is reported as what it is: one that owns layout. Until
        // something has actually been placed, the ordinary case is assumed —
        // a first attempt is what produces the evidence.
        match *self.trust.lock().unwrap() {
            PlacementTrust::Ignored => Capabilities::TILING,
            PlacementTrust::Untested | PlacementTrust::Honoured => Capabilities::X11,
        }
    }

    fn focus(&self, window: NativeWindow) -> PlacementOutcome {
        let window = window.0 as Window;
        let result = (|| -> Result<(), BackendError> {
            let active = self.atom(b"_NET_ACTIVE_WINDOW")?;
            // Source 2 = pager: window managers honour it without the
            // focus-stealing-prevention timeout applied to ordinary clients.
            let event = ClientMessageEvent::new(32, window, active, [2, 0, 0, 0, 0]);
            self.connection
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                    event,
                )
                .map_err(|e| BackendError::Protocol(e.to_string()))?;
            self.connection
                .flush()
                .map_err(|e| BackendError::Protocol(e.to_string()))
        })();
        match result {
            Ok(()) => PlacementOutcome::Applied,
            Err(e) => PlacementOutcome::Failed(e.to_string()),
        }
    }

    fn place(
        &self,
        window: NativeWindow,
        identity: &MonitorIdentity,
        mode: WindowMode,
    ) -> PlacementOutcome {
        // Once this session's window manager has been seen to discard
        // placements, stop asking: repeating a request that is known not to
        // work would report success the user cannot see.
        if *self.trust.lock().unwrap() == PlacementTrust::Ignored {
            return PlacementOutcome::Refused;
        }

        // Resolve the identity to live geometry *now*, immediately before the
        // native call, and never keep it.
        let snapshot = match self.snapshot() {
            Ok(snapshot) => snapshot,
            Err(e) => return PlacementOutcome::Failed(e.to_string()),
        };
        let Some(monitor) = snapshot
            .monitors
            .iter()
            .find(|m| &m.identity == identity || m.fallback_identity.as_ref() == Some(identity))
        else {
            return PlacementOutcome::Disappeared;
        };

        let window = window.0 as Window;
        let geometry = monitor.geometry;

        let result = (|| -> Result<(), BackendError> {
            if mode == WindowMode::Fullscreen {
                // Leave fullscreen first: some window managers ignore a move
                // request issued while the window is already fullscreen.
                self.set_fullscreen(window, false)?;
            }
            let config = xproto::ConfigureWindowAux::new()
                .x(geometry.x)
                .y(geometry.y)
                .stack_mode(StackMode::ABOVE);
            self.connection
                .configure_window(window, &config)
                .map_err(|e| BackendError::Protocol(e.to_string()))?;
            match mode {
                WindowMode::Fullscreen => self.set_fullscreen(window, true)?,
                WindowMode::Windowed => self.set_fullscreen(window, false)?,
                WindowMode::Hidden => {}
            }
            self.connection
                .flush()
                .map_err(|e| BackendError::Protocol(e.to_string()))
        })();

        if let Err(e) = result {
            return PlacementOutcome::Failed(e.to_string());
        }

        // A hidden window has no position to observe, and asking for one would
        // latch a verdict from no evidence.
        if mode == WindowMode::Hidden {
            return PlacementOutcome::Applied;
        }

        // The request went out. Whether it *held* is a question only the
        // window manager can answer, and it answers by what it does.
        match self.settle(window, &geometry) {
            Some(true) => {
                self.record_trust(true);
                PlacementOutcome::Applied
            }
            Some(false) => {
                self.record_trust(false);
                PlacementOutcome::Refused
            }
            // The window never became viewable. That says nothing about the
            // window manager, so no verdict is latched; the caller's bounded
            // post-map retry asks again once the window is up.
            None => PlacementOutcome::Refused,
        }
    }
}

/// Whether a window ended up on the monitor it was sent to.
///
/// Majority overlap rather than an exact match: a window manager may add a
/// frame, honour a panel reservation or round a size, and none of that means
/// the placement was refused. Being mostly on the requested monitor is the
/// question the user actually cares about.
fn landed_on(window: &Rect, target: &Rect) -> bool {
    if window.area() == 0 {
        return false;
    }
    window.intersection_area(target) * 2 > window.area()
}

/// The EWMH window-manager name: `_NET_SUPPORTING_WM_CHECK` on the root points
/// at a window the manager owns, and that window carries `_NET_WM_NAME`.
///
/// A manager that answers neither is not necessarily absent — it may simply
/// predate EWMH — so `None` means "unknown", and unknown is treated as the
/// ordinary stacking case rather than assumed to tile.
fn read_window_manager_name(connection: &RustConnection, root: Window) -> Option<String> {
    let intern = |name: &[u8]| -> Option<Atom> {
        connection
            .intern_atom(true, name)
            .ok()?
            .reply()
            .ok()
            .map(|reply| reply.atom)
            .filter(|atom| *atom != 0)
    };

    let check = intern(b"_NET_SUPPORTING_WM_CHECK")?;
    let reply = connection
        .get_property(false, root, check, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let owner = reply.value32()?.next()?;
    if owner == 0 {
        return None;
    }

    let name_atom = intern(b"_NET_WM_NAME")?;
    let utf8 = intern(b"UTF8_STRING")?;
    let reply = connection
        .get_property(false, owner, name_atom, utf8, 0, 128)
        .ok()?
        .reply()
        .ok()?;
    let name = String::from_utf8_lossy(&reply.value).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn read_xft_dpi(connection: &RustConnection, root: Window) -> Option<f64> {
    let reply = connection
        .get_property(
            false,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            0,
            1 << 16,
        )
        .ok()?
        .reply()
        .ok()?;
    let resources = String::from_utf8_lossy(&reply.value);
    for line in resources.lines() {
        if let Some(value) = line.strip_prefix("Xft.dpi:") {
            return value.trim().parse().ok();
        }
    }
    None
}

/// Minimal EDID 1.x parse: manufacturer, model name and serial.
///
/// Deliberately small — the goal is a *stable identity string*, not a full
/// EDID decoder. `libdisplay-info` is the intended replacement when the
/// native identity adapters land.
fn parse_edid(edid: &[u8]) -> (Option<String>, Option<String>, Option<String>) {
    if edid.len() < 128 || edid[0..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return (None, None, None);
    }
    let manufacturer_bits = u16::from_be_bytes([edid[8], edid[9]]);
    let letter = |shift: u16| -> char {
        let value = ((manufacturer_bits >> shift) & 0x1F) as u8;
        (b'A' + value.saturating_sub(1)) as char
    };
    let make: String = [letter(10), letter(5), letter(0)].iter().collect();
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let serial_number = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);

    // Descriptor blocks 54..126, 18 bytes each; 0xFC is the monitor name.
    let mut model = None;
    let mut serial_text = None;
    for block in 0..4 {
        let start = 54 + block * 18;
        let Some(descriptor) = edid.get(start..start + 18) else {
            break;
        };
        if descriptor[0..3] != [0, 0, 0] {
            continue;
        }
        let text = String::from_utf8_lossy(&descriptor[5..18])
            .trim_end_matches(['\n', ' ', '\0'])
            .to_string();
        match descriptor[3] {
            0xFC => model = Some(text),
            0xFF => serial_text = Some(text),
            _ => {}
        }
    }
    let model = model.or_else(|| Some(format!("{product:04X}")));
    let serial = match (&serial_text, serial_number) {
        (Some(text), _) if !text.is_empty() => Some(format!("{make}-{text}")),
        (_, 0) => None,
        (_, number) => Some(format!("{make}-{product:04X}-{number:08X}")),
    };
    (Some(make), model, serial)
}

/// Unused import guard: `ScreenSize` and `GetOutputInfoReply` document the
/// shape of the XRandR replies this module relies on.
#[allow(dead_code)]
fn _randr_types(_: Option<ScreenSize>, _: Option<GetOutputInfoReply>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_edid() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // "ACM" = 0b00001_00011_01101
        let bits: u16 = (1 << 10) | (3 << 5) | 13;
        edid[8..10].copy_from_slice(&bits.to_be_bytes());
        edid[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        edid[12..16].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
        // Monitor-name descriptor.
        let start = 54;
        edid[start + 3] = 0xFC;
        edid[start + 5..start + 5 + 9].copy_from_slice(b"Projector");
        edid
    }

    #[test]
    fn edid_yields_a_stable_identity() {
        let (make, model, serial) = parse_edid(&synthetic_edid());
        assert_eq!(make.as_deref(), Some("ACM"));
        assert_eq!(model.as_deref(), Some("Projector"));
        assert_eq!(serial.as_deref(), Some("ACM-1234-DEADBEEF"));
    }

    #[test]
    fn garbage_edid_is_rejected() {
        assert_eq!(parse_edid(&[0u8; 128]), (None, None, None));
        assert_eq!(parse_edid(&[]), (None, None, None));
    }

    const PROJECTOR: Rect = Rect {
        x: 1920,
        y: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn a_window_on_the_requested_monitor_counts_as_placed() {
        assert!(landed_on(&Rect::new(1920, 0, 1920, 1080), &PROJECTOR));
    }

    #[test]
    fn a_frame_or_a_panel_reservation_does_not_count_as_refusal() {
        // A window manager may add a title bar, honour a reserved strut or
        // round a size. The placement still went where it was asked.
        assert!(landed_on(&Rect::new(1920, 28, 1920, 1052), &PROJECTOR));
        assert!(landed_on(&Rect::new(1912, 0, 1920, 1080), &PROJECTOR));
    }

    #[test]
    fn a_window_left_on_the_other_monitor_counts_as_refused() {
        // What a tiling window manager does: the request is applied and then
        // immediately overridden, leaving the window where it started.
        assert!(!landed_on(&Rect::new(0, 0, 1920, 1200), &PROJECTOR));
    }

    #[test]
    fn a_window_straddling_two_monitors_goes_to_the_majority() {
        // Mostly on the projector.
        assert!(landed_on(&Rect::new(1620, 0, 1920, 1080), &PROJECTOR));
        // Mostly on the other one.
        assert!(!landed_on(&Rect::new(220, 0, 1920, 1080), &PROJECTOR));
    }

    #[test]
    fn a_zero_sized_window_is_not_evidence_of_placement() {
        assert!(!landed_on(&Rect::new(1920, 0, 0, 0), &PROJECTOR));
    }
}
