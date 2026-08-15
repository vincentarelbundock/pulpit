//! Win32 display adapter.
//!
//! Enumeration through `EnumDisplayMonitors`, identity through
//! `QueryDisplayConfig` (which carries the EDID vendor/product pair and the
//! stable device path), per-monitor scale through `GetDpiForMonitor`, and
//! placement through `SetWindowPos`.
//!
//! The FFI is declared here rather than pulled from a binding crate: the
//! surface is a dozen calls, and declaring it keeps the struct layouts this
//! module depends on visible next to the code that reads them.
//!
//! # Coordinate spaces
//!
//! A per-monitor-DPI-aware process sees the Windows desktop in *physical*
//! pixels, while [`crate::snapshot::Rect`] is documented as logical. The two
//! are reconciled in one place: [`WindowsBackend::enumerate_raw`] returns both,
//! snapshots carry the logical rectangle, and [`DisplayBackend::place`] uses
//! the physical one it re-reads immediately before the native call.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use crate::backend::{BackendError, DisplayBackend, NativeWindow, PlacementOutcome};
use crate::identity::MonitorIdentity;
use crate::reconcile::{Capabilities, WindowMode};
use crate::snapshot::{DisplaySnapshot, Monitor, Rect};

// ---------------------------------------------------------------------------
// Win32 types
// ---------------------------------------------------------------------------

type Bool = i32;
type Hmonitor = *mut c_void;
type Hwnd = *mut c_void;
type Hdc = *mut c_void;
type Lparam = isize;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RectW {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MonitorInfoExW {
    cb_size: u32,
    rc_monitor: RectW,
    rc_work: RectW,
    dw_flags: u32,
    sz_device: [u16; 32],
}

impl Default for MonitorInfoExW {
    fn default() -> Self {
        MonitorInfoExW {
            cb_size: std::mem::size_of::<MonitorInfoExW>() as u32,
            rc_monitor: RectW::default(),
            rc_work: RectW::default(),
            dw_flags: 0,
            sz_device: [0; 32],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Luid {
    low_part: u32,
    high_part: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PathSourceInfo {
    adapter_id: Luid,
    id: u32,
    mode_info_idx: u32,
    status_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rational {
    numerator: u32,
    denominator: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PathTargetInfo {
    adapter_id: Luid,
    id: u32,
    mode_info_idx: u32,
    output_technology: u32,
    rotation: u32,
    scaling: u32,
    refresh_rate: Rational,
    scan_line_ordering: u32,
    target_available: Bool,
    status_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PathInfo {
    source_info: PathSourceInfo,
    target_info: PathTargetInfo,
    flags: u32,
}

/// `DISPLAYCONFIG_MODE_INFO`. The trailing union is never read here — only the
/// size matters, because the array is passed through to `QueryDisplayConfig`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ModeInfo {
    info_type: u32,
    id: u32,
    adapter_id: Luid,
    payload: [u8; 48],
}

impl Default for ModeInfo {
    fn default() -> Self {
        ModeInfo {
            info_type: 0,
            id: 0,
            adapter_id: Luid::default(),
            payload: [0; 48],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DeviceInfoHeader {
    kind: u32,
    size: u32,
    adapter_id: Luid,
    id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TargetDeviceName {
    header: DeviceInfoHeader,
    flags: u32,
    output_technology: u32,
    edid_manufacture_id: u16,
    edid_product_code_id: u16,
    connector_instance: u32,
    monitor_friendly_device_name: [u16; 64],
    monitor_device_path: [u16; 128],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SourceDeviceName {
    header: DeviceInfoHeader,
    view_gdi_device_name: [u16; 32],
}

const QDC_ONLY_ACTIVE_PATHS: u32 = 2;
const ERROR_SUCCESS: i32 = 0;
const DEVICE_INFO_GET_SOURCE_NAME: u32 = 1;
const DEVICE_INFO_GET_TARGET_NAME: u32 = 2;
const OUTPUT_TECHNOLOGY_INTERNAL: u32 = 0x8000_0000;
const MONITORINFOF_PRIMARY: u32 = 1;
const MDT_EFFECTIVE_DPI: u32 = 0;

const GWL_STYLE: i32 = -16;
const GWL_EXSTYLE: i32 = -20;
const WS_POPUP: u32 = 0x8000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const SWP_FRAMECHANGED: u32 = 0x0020;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_SHOWWINDOW: u32 = 0x0040;
const SWP_NOSIZE: u32 = 0x0001;
const SW_SHOWNORMAL: i32 = 1;
const HWND_TOP: Hwnd = std::ptr::null_mut();

#[link(name = "user32")]
extern "system" {
    fn EnumDisplayMonitors(
        hdc: Hdc,
        clip: *const RectW,
        proc: extern "system" fn(Hmonitor, Hdc, *mut RectW, Lparam) -> Bool,
        data: Lparam,
    ) -> Bool;
    fn GetMonitorInfoW(monitor: Hmonitor, info: *mut MonitorInfoExW) -> Bool;
    fn GetDisplayConfigBufferSizes(flags: u32, paths: *mut u32, modes: *mut u32) -> i32;
    fn QueryDisplayConfig(
        flags: u32,
        path_count: *mut u32,
        paths: *mut PathInfo,
        mode_count: *mut u32,
        modes: *mut ModeInfo,
        current_topology: *mut u32,
    ) -> i32;
    fn DisplayConfigGetDeviceInfo(header: *mut DeviceInfoHeader) -> i32;
    fn SetWindowPos(
        hwnd: Hwnd,
        insert_after: Hwnd,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> Bool;
    fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
    fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
    fn IsWindow(hwnd: Hwnd) -> Bool;
    fn ShowWindow(hwnd: Hwnd, cmd: i32) -> Bool;
    fn SetForegroundWindow(hwnd: Hwnd) -> Bool;
}

#[link(name = "shcore")]
extern "system" {
    fn GetDpiForMonitor(monitor: Hmonitor, kind: u32, x: *mut u32, y: *mut u32) -> i32;
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/// One monitor as Win32 reports it, before the logical/physical split.
struct RawMonitor {
    monitor: Monitor,
    /// The desktop rectangle in physical pixels, which is what `SetWindowPos`
    /// takes.
    physical: Rect,
}

/// Identity data recovered from `QueryDisplayConfig`, keyed by GDI device name
/// (`\\.\DISPLAY1`), which is the one field both APIs agree on.
#[derive(Default, Clone)]
struct TargetInfo {
    friendly_name: Option<String>,
    device_path: Option<String>,
    make: Option<String>,
    internal: bool,
}

pub struct WindowsBackend {
    sequence: Mutex<u64>,
    /// Window styles saved before a window was made borderless-fullscreen, so
    /// leaving fullscreen restores what the toolkit had. Keyed by `HWND` value:
    /// a number, re-validated with `IsWindow` before use, never a live handle
    /// held across a turn.
    saved_styles: Mutex<HashMap<u64, (isize, isize)>>,
}

impl std::fmt::Debug for WindowsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsBackend").finish_non_exhaustive()
    }
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsBackend {
    pub fn new() -> Self {
        WindowsBackend {
            sequence: Mutex::new(0),
            saved_styles: Mutex::new(HashMap::new()),
        }
    }

    /// Win32 has no "is a display server present" question to answer: the
    /// desktop either enumerates monitors or the session is not interactive.
    pub fn connect() -> Result<Self, BackendError> {
        let backend = Self::new();
        match backend.enumerate_raw() {
            Ok(monitors) if monitors.is_empty() => Err(BackendError::Unavailable),
            Ok(_) => Ok(backend),
            Err(e) => Err(e),
        }
    }

    fn next_sequence(&self) -> u64 {
        let mut sequence = self.sequence.lock().unwrap();
        *sequence += 1;
        *sequence
    }

    fn enumerate_raw(&self) -> Result<Vec<RawMonitor>, BackendError> {
        let handles = enum_monitors();
        let targets = query_display_config();

        let mut out = Vec::new();
        for handle in handles {
            let mut info = MonitorInfoExW::default();
            // SAFETY: `info.cb_size` is set by `Default`, as the API requires.
            if unsafe { GetMonitorInfoW(handle, &mut info) } == 0 {
                continue;
            }
            let device = wide_to_string(&info.sz_device);
            let target = targets.get(&device).cloned().unwrap_or_default();

            let dpi = monitor_dpi(handle).unwrap_or(96);
            let scale_factor = dpi as f64 / 96.0;

            let physical = Rect::new(
                info.rc_monitor.left,
                info.rc_monitor.top,
                (info.rc_monitor.right - info.rc_monitor.left).max(0) as u32,
                (info.rc_monitor.bottom - info.rc_monitor.top).max(0) as u32,
            );
            // Snapshots speak logical units everywhere else in the workspace.
            let logical = Rect::new(
                (physical.x as f64 / scale_factor).round() as i32,
                (physical.y as f64 / scale_factor).round() as i32,
                (physical.width as f64 / scale_factor).round() as u32,
                (physical.height as f64 / scale_factor).round() as u32,
            );

            let model = target.friendly_name.clone();
            let make = target.make.clone();
            // The device path (`\\?\DISPLAY#GSM5A87#5&...#{guid}`) carries the
            // EDID vendor and product plus the adapter instance. It survives a
            // reboot and a reconnect on the same port, which is the strongest
            // identity Windows offers without reading raw EDID from the
            // registry.
            let identity = match &target.device_path {
                Some(path) => MonitorIdentity::Stable { id: path.clone() },
                None => MonitorIdentity::Connector {
                    connector: device.clone(),
                    make: make.clone().unwrap_or_default(),
                    model: model.clone().unwrap_or_default(),
                },
            };
            let fallback = Some(MonitorIdentity::Connector {
                connector: device.clone(),
                make: make.clone().unwrap_or_default(),
                model: model.clone().unwrap_or_default(),
            })
            .filter(|f| f != &identity);

            out.push(RawMonitor {
                monitor: Monitor {
                    identity,
                    fallback_identity: fallback,
                    connector: Some(device),
                    make,
                    model,
                    geometry: logical,
                    scale_factor,
                    // Win32 reports millimetres only through the EDID blob in
                    // the registry; the display-config path does not carry it.
                    physical_size_mm: None,
                    builtin: target.internal,
                    primary: info.dw_flags & MONITORINFOF_PRIMARY != 0,
                    handle: handle as u64,
                },
                physical,
            });
        }
        Ok(out)
    }
}

/// Collect the `HMONITOR`s. The callback appends to a `Vec` reached through
/// the `LPARAM`, which is the documented way to use this API.
fn enum_monitors() -> Vec<Hmonitor> {
    extern "system" fn callback(
        monitor: Hmonitor,
        _hdc: Hdc,
        _rect: *mut RectW,
        data: Lparam,
    ) -> Bool {
        // SAFETY: `data` is the `&mut Vec` handed to `EnumDisplayMonitors`
        // below, alive for the duration of the call.
        let out = unsafe { &mut *(data as *mut Vec<Hmonitor>) };
        out.push(monitor);
        1
    }

    let mut out: Vec<Hmonitor> = Vec::new();
    // SAFETY: the pointer is valid until `EnumDisplayMonitors` returns, and
    // the callback is only invoked during the call.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            callback,
            &mut out as *mut Vec<Hmonitor> as Lparam,
        );
    }
    out
}

fn monitor_dpi(monitor: Hmonitor) -> Option<u32> {
    let mut x = 0u32;
    let mut y = 0u32;
    // SAFETY: both out-pointers are valid for the call.
    let status = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    (status == 0 && x > 0).then_some(x)
}

/// Walk the active display paths, pairing each GDI device name with the target
/// identity behind it.
fn query_display_config() -> HashMap<String, TargetInfo> {
    let mut out = HashMap::new();

    let mut path_count = 0u32;
    let mut mode_count = 0u32;
    // SAFETY: out-pointers only.
    let status = unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
    };
    if status != ERROR_SUCCESS || path_count == 0 {
        return out;
    }

    let mut paths = vec![PathInfo::default(); path_count as usize];
    let mut modes = vec![ModeInfo::default(); mode_count as usize];
    // SAFETY: both buffers are sized by the call above and are passed with
    // their counts.
    let status = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return out;
    }

    for path in paths.iter().take(path_count as usize) {
        let Some(source) = source_name(path) else {
            continue;
        };
        let target = target_name(path);
        out.insert(source, target);
    }
    out
}

fn source_name(path: &PathInfo) -> Option<String> {
    let mut request = SourceDeviceName {
        header: DeviceInfoHeader {
            kind: DEVICE_INFO_GET_SOURCE_NAME,
            size: std::mem::size_of::<SourceDeviceName>() as u32,
            adapter_id: path.source_info.adapter_id,
            id: path.source_info.id,
        },
        view_gdi_device_name: [0; 32],
    };
    // SAFETY: the header is the first field, as the API requires, and `size`
    // describes the whole struct.
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut request.header) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let name = wide_to_string(&request.view_gdi_device_name);
    (!name.is_empty()).then_some(name)
}

fn target_name(path: &PathInfo) -> TargetInfo {
    let mut request = TargetDeviceName {
        header: DeviceInfoHeader {
            kind: DEVICE_INFO_GET_TARGET_NAME,
            size: std::mem::size_of::<TargetDeviceName>() as u32,
            adapter_id: path.target_info.adapter_id,
            id: path.target_info.id,
        },
        flags: 0,
        output_technology: 0,
        edid_manufacture_id: 0,
        edid_product_code_id: 0,
        connector_instance: 0,
        monitor_friendly_device_name: [0; 64],
        monitor_device_path: [0; 128],
    };
    // SAFETY: as above.
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut request.header) };
    if status != ERROR_SUCCESS {
        return TargetInfo {
            internal: path.target_info.output_technology == OUTPUT_TECHNOLOGY_INTERNAL,
            ..TargetInfo::default()
        };
    }
    let friendly = wide_to_string(&request.monitor_friendly_device_name);
    let path_string = wide_to_string(&request.monitor_device_path);
    TargetInfo {
        friendly_name: (!friendly.is_empty()).then_some(friendly),
        device_path: (!path_string.is_empty()).then_some(path_string),
        make: pnp_id(request.edid_manufacture_id),
        internal: request.output_technology == OUTPUT_TECHNOLOGY_INTERNAL
            || path.target_info.output_technology == OUTPUT_TECHNOLOGY_INTERNAL,
    }
}

/// Decode the three-letter PNP manufacturer id packed into the EDID vendor
/// field. Windows stores it big-endian, five bits per letter.
fn pnp_id(raw: u16) -> Option<String> {
    if raw == 0 {
        return None;
    }
    let bits = raw.swap_bytes();
    let letter = |shift: u16| -> char {
        let value = ((bits >> shift) & 0x1F) as u8;
        (b'A' + value.saturating_sub(1)) as char
    };
    let make: String = [letter(10), letter(5), letter(0)].iter().collect();
    make.chars().all(|c| c.is_ascii_uppercase()).then_some(make)
}

fn wide_to_string(raw: &[u16]) -> String {
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..end]).trim().to_string()
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

impl DisplayBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "win32"
    }

    fn snapshot(&self) -> Result<DisplaySnapshot, BackendError> {
        let monitors = self
            .enumerate_raw()?
            .into_iter()
            .map(|raw| raw.monitor)
            .collect();
        Ok(DisplaySnapshot::new(monitors, self.next_sequence()))
    }

    fn capabilities(&self) -> Capabilities {
        // Win32 places top-level windows wherever it is told, before or after
        // the window is shown, and restoring a saved rectangle cannot strand
        // it: `SetWindowPos` is checked against the live desktop.
        Capabilities {
            arbitrary_position: true,
            unfullscreen_safe: true,
            place_before_map: true,
        }
    }

    fn focus(&self, window: NativeWindow) -> PlacementOutcome {
        let hwnd = window.0 as Hwnd;
        // SAFETY: the handle is re-validated immediately before use.
        if unsafe { IsWindow(hwnd) } == 0 {
            return PlacementOutcome::Failed("that window is gone".into());
        }
        // SAFETY: `hwnd` is a live window, checked above.
        let raised = unsafe { SetForegroundWindow(hwnd) };
        if raised == 0 {
            // The foreground lock is deliberate Windows policy, not a fault:
            // a background process cannot steal focus from the active one.
            PlacementOutcome::Refused
        } else {
            PlacementOutcome::Applied
        }
    }

    fn place(
        &self,
        window: NativeWindow,
        identity: &MonitorIdentity,
        mode: WindowMode,
    ) -> PlacementOutcome {
        let hwnd = window.0 as Hwnd;
        // SAFETY: no handle was kept; this is the validity check.
        if unsafe { IsWindow(hwnd) } == 0 {
            return PlacementOutcome::Failed("that window is gone".into());
        }

        // Resolve the identity against a fresh enumeration, immediately before
        // the native call.
        let monitors = match self.enumerate_raw() {
            Ok(monitors) => monitors,
            Err(e) => return PlacementOutcome::Failed(e.to_string()),
        };
        let Some(target) = monitors.iter().find(|raw| {
            &raw.monitor.identity == identity
                || raw.monitor.fallback_identity.as_ref() == Some(identity)
        }) else {
            return PlacementOutcome::Disappeared;
        };
        let area = target.physical;

        match mode {
            WindowMode::Fullscreen => {
                // Borderless fullscreen on the target monitor. A real
                // `WS_POPUP` covering exactly the monitor rectangle is what
                // presentation software wants: no mode switch, no exclusive
                // device, and an instant swap when the role changes.
                // SAFETY: `hwnd` is live; both calls take plain integers.
                unsafe {
                    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                    let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    self.saved_styles
                        .lock()
                        .unwrap()
                        .entry(window.0)
                        .or_insert((style, ex_style));

                    SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE) as isize);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, WS_EX_TOPMOST as isize);
                    let ok = SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        area.x,
                        area.y,
                        area.width as i32,
                        area.height as i32,
                        SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOACTIVATE,
                    );
                    if ok == 0 {
                        return PlacementOutcome::Failed("SetWindowPos refused".into());
                    }
                }
                PlacementOutcome::Applied
            }
            WindowMode::Windowed => {
                let restored = self.saved_styles.lock().unwrap().remove(&window.0);
                // SAFETY: `hwnd` is live.
                unsafe {
                    if let Some((style, ex_style)) = restored {
                        SetWindowLongPtrW(hwnd, GWL_STYLE, style);
                        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style);
                    } else {
                        SetWindowLongPtrW(
                            hwnd,
                            GWL_STYLE,
                            (WS_OVERLAPPEDWINDOW | WS_VISIBLE) as isize,
                        );
                        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, 0);
                    }
                    ShowWindow(hwnd, SW_SHOWNORMAL);
                    // Move onto the target monitor without resizing: leaving
                    // fullscreen must not shrink a window the user sized.
                    let ok = SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        area.x + (area.width as i32 / 16),
                        area.y + (area.height as i32 / 16),
                        0,
                        0,
                        SWP_FRAMECHANGED | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    if ok == 0 {
                        return PlacementOutcome::Failed("SetWindowPos refused".into());
                    }
                }
                PlacementOutcome::Applied
            }
            // Hiding is the toolkit's business; the backend has nothing to add
            // and must not claim it did something.
            WindowMode::Hidden => PlacementOutcome::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pnp_ids_decode_to_three_letters() {
        // "GSM" packed as EDID stores it, byte-swapped the way the display
        // config API hands it over.
        let bits: u16 = ((7 << 10) | (19 << 5) | 13) as u16;
        assert_eq!(pnp_id(bits.swap_bytes()).as_deref(), Some("GSM"));
        assert_eq!(pnp_id(0), None);
    }

    #[test]
    fn wide_strings_stop_at_the_nul() {
        let mut raw = [0u16; 32];
        for (slot, c) in raw.iter_mut().zip("\\\\.\\DISPLAY1".encode_utf16()) {
            *slot = c;
        }
        assert_eq!(wide_to_string(&raw), "\\\\.\\DISPLAY1");
        assert_eq!(wide_to_string(&[0u16; 8]), "");
    }

    #[test]
    fn the_display_config_structs_have_the_documented_layout() {
        // These sizes are part of the ABI: `DisplayConfigGetDeviceInfo` reads
        // `size` and rejects anything else. A silent layout change here would
        // be a runtime failure on a machine this test cannot reach, so it is
        // asserted at compile-and-test time instead.
        assert_eq!(std::mem::size_of::<PathInfo>(), 72);
        assert_eq!(std::mem::size_of::<ModeInfo>(), 64);
        assert_eq!(std::mem::size_of::<DeviceInfoHeader>(), 20);
        assert_eq!(std::mem::size_of::<TargetDeviceName>(), 420);
        assert_eq!(std::mem::size_of::<SourceDeviceName>(), 84);
    }
}
