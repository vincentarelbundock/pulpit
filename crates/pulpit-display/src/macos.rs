//! CoreGraphics / AppKit display adapter.
//!
//! Enumeration and identity come from CoreGraphics, which is the rare platform
//! that hands out a genuinely stable identifier without an EDID parse: the
//! vendor, model and serial triple is derived from the panel itself and
//! survives a reboot, a re-plug and a different port.
//!
//! Placement goes through AppKit, because CoreGraphics has no notion of a
//! window. The Objective-C runtime is called directly rather than through a
//! binding crate; the surface is four selectors.
//!
//! # Coordinate spaces
//!
//! CoreGraphics puts the origin at the top-left of the main display with `y`
//! growing downward, which is what [`crate::snapshot::Rect`] means. AppKit
//! puts it at the bottom-left of the main display with `y` growing upward.
//! [`flip_to_appkit`] is the only place that conversion happens.
//!
//! # Spaces
//!
//! With "Displays have separate Spaces" enabled — the default —
//! `toggleFullScreen:` moves the window to its own Space on whichever display
//! it currently occupies. The window is therefore moved onto the target
//! display *first* and made fullscreen second, which is why the two steps are
//! not merged.

use std::ffi::c_void;
use std::sync::Mutex;

use crate::backend::{BackendError, DisplayBackend, NativeWindow, PlacementOutcome};
use crate::identity::{self, MonitorIdentity};
use crate::reconcile::{Capabilities, WindowMode};
use crate::snapshot::{DisplaySnapshot, Monitor, Rect};

// ---------------------------------------------------------------------------
// CoreGraphics
// ---------------------------------------------------------------------------

type CgDirectDisplayId = u32;
type CgError = i32;
type CgDisplayModeRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CgPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CgSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CgRect {
    origin: CgPoint,
    size: CgSize,
}

const CG_ERROR_SUCCESS: CgError = 0;
const MAX_DISPLAYS: u32 = 32;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetActiveDisplayList(
        max_displays: u32,
        displays: *mut CgDirectDisplayId,
        count: *mut u32,
    ) -> CgError;
    fn CGMainDisplayID() -> CgDirectDisplayId;
    fn CGDisplayBounds(display: CgDirectDisplayId) -> CgRect;
    fn CGDisplayVendorNumber(display: CgDirectDisplayId) -> u32;
    fn CGDisplayModelNumber(display: CgDirectDisplayId) -> u32;
    fn CGDisplaySerialNumber(display: CgDirectDisplayId) -> u32;
    fn CGDisplayIsBuiltin(display: CgDirectDisplayId) -> i32;
    fn CGDisplayScreenSize(display: CgDirectDisplayId) -> CgSize;
    fn CGDisplayCopyDisplayMode(display: CgDirectDisplayId) -> CgDisplayModeRef;
    fn CGDisplayModeGetWidth(mode: CgDisplayModeRef) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: CgDisplayModeRef) -> usize;
    fn CGDisplayModeRelease(mode: CgDisplayModeRef);
}

// ---------------------------------------------------------------------------
// The Objective-C runtime
// ---------------------------------------------------------------------------

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn sel_registerName(name: *const u8) -> Sel;
    fn objc_msgSend();
}

/// `objc_msgSend` is declared without a signature because it has none: the
/// caller must transmute it to the exact prototype of the method being sent.
/// Each helper below does exactly that, once, next to the selector it uses.
///
/// # Safety
///
/// `receiver` must be a live object that responds to `selector` with the
/// signature `F`.
unsafe fn msg_send_fn<F>() -> F {
    // SAFETY: the caller commits to the correct prototype for the selector it
    // is about to send. This is the documented way to call `objc_msgSend` from
    // Rust; the symbol is an address, not a callable C function.
    std::mem::transmute_copy(&(objc_msgSend as *const c_void))
}

fn selector(name: &str) -> Sel {
    // Selector names must be NUL-terminated; the runtime interns them, so
    // repeating this is a hash lookup rather than an allocation.
    let mut bytes = Vec::with_capacity(name.len() + 1);
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    // SAFETY: `bytes` is NUL-terminated and lives across the call; the runtime
    // copies the name into its own table.
    unsafe { sel_registerName(bytes.as_ptr()) }
}

/// `[receiver window]` — the `NSWindow` owning an `NSView`.
///
/// # Safety
/// `view` must be a live `NSView*`.
unsafe fn view_window(view: Id) -> Id {
    let send: extern "C" fn(Id, Sel) -> Id = msg_send_fn();
    send(view, selector("window"))
}

/// `[window setFrame:display:]`.
///
/// # Safety
/// `window` must be a live `NSWindow*`.
unsafe fn set_frame(window: Id, frame: CgRect, display: bool) {
    let send: extern "C" fn(Id, Sel, CgRect, i8) = msg_send_fn();
    send(window, selector("setFrame:display:"), frame, display as i8);
}

/// `[window styleMask]`.
///
/// # Safety
/// `window` must be a live `NSWindow*`.
unsafe fn style_mask(window: Id) -> u64 {
    let send: extern "C" fn(Id, Sel) -> u64 = msg_send_fn();
    send(window, selector("styleMask"))
}

/// `[window toggleFullScreen:nil]`.
///
/// # Safety
/// `window` must be a live `NSWindow*`.
unsafe fn toggle_fullscreen(window: Id) {
    let send: extern "C" fn(Id, Sel, Id) = msg_send_fn();
    send(window, selector("toggleFullScreen:"), std::ptr::null_mut());
}

/// `[window frame]` — the window's current frame, in AppKit's own
/// coordinate space (bottom-left origin, `y` up). Used to verify a placement
/// the async `toggleFullScreen:` transition may not have completed yet.
///
/// # Safety
/// `window` must be a live `NSWindow*`.
unsafe fn window_frame(window: Id) -> CgRect {
    let send: extern "C" fn(Id, Sel) -> CgRect = msg_send_fn();
    send(window, selector("frame"))
}

/// `[window makeKeyAndOrderFront:nil]`.
///
/// # Safety
/// `window` must be a live `NSWindow*`.
unsafe fn make_key_and_order_front(window: Id) {
    let send: extern "C" fn(Id, Sel, Id) = msg_send_fn();
    send(
        window,
        selector("makeKeyAndOrderFront:"),
        std::ptr::null_mut(),
    );
}

/// `NSWindowStyleMaskFullScreen`.
const STYLE_MASK_FULLSCREEN: u64 = 1 << 14;

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

struct RawMonitor {
    monitor: Monitor,
    /// CoreGraphics bounds, kept for placement so the AppKit flip is computed
    /// from the same numbers the identity was resolved against.
    bounds: CgRect,
}

pub struct MacosBackend {
    sequence: Mutex<u64>,
}

impl std::fmt::Debug for MacosBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosBackend").finish_non_exhaustive()
    }
}

impl Default for MacosBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MacosBackend {
    pub fn new() -> Self {
        MacosBackend {
            sequence: Mutex::new(0),
        }
    }

    pub fn connect() -> Result<Self, BackendError> {
        crate::backend::connect_if_populated(Self::new(), Self::enumerate_raw)
    }

    fn next_sequence(&self) -> u64 {
        crate::backend::next_sequence(&self.sequence)
    }

    fn enumerate_raw(&self) -> Result<Vec<RawMonitor>, BackendError> {
        let mut ids = [0 as CgDirectDisplayId; MAX_DISPLAYS as usize];
        let mut count = 0u32;
        // SAFETY: the buffer is `MAX_DISPLAYS` long and both out-pointers are
        // valid for the call.
        let status = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
        if status != CG_ERROR_SUCCESS {
            return Err(BackendError::Protocol(format!(
                "CGGetActiveDisplayList failed with {status}"
            )));
        }

        // SAFETY: no arguments; always valid.
        let main = unsafe { CGMainDisplayID() };

        let mut out = Vec::new();
        for &id in ids.iter().take(count as usize) {
            // SAFETY: `id` came from the active display list, so it is live for
            // as long as this enumeration; a display that vanishes mid-loop
            // returns zeroes rather than faulting.
            let (bounds, vendor, model, serial, builtin, size_mm) = unsafe {
                (
                    CGDisplayBounds(id),
                    CGDisplayVendorNumber(id),
                    CGDisplayModelNumber(id),
                    CGDisplaySerialNumber(id),
                    CGDisplayIsBuiltin(id) != 0,
                    CGDisplayScreenSize(id),
                )
            };

            let scale_factor = backing_scale(id);

            // Vendor/model/serial is the platform-stable identity. Serial is
            // frequently zero on panels that do not report one, so it is only
            // trusted when present: "same model on the same Mac" is a weaker
            // claim and belongs one rung down the ladder.
            let geometric = MonitorIdentity::Geometric {
                make: format!("{vendor:08X}"),
                model: format!("{model:08X}"),
                width_mm: size_mm.width.round().max(0.0) as u32,
                height_mm: size_mm.height.round().max(0.0) as u32,
                x: bounds.origin.x as i32,
                y: bounds.origin.y as i32,
            };
            let identity = if serial != 0 {
                MonitorIdentity::Stable {
                    id: format!("CG-{vendor:08X}-{model:08X}-{serial:08X}"),
                }
            } else {
                geometric.clone()
            };
            let (identity, fallback) = identity::ladder(identity, geometric);

            out.push(RawMonitor {
                monitor: Monitor {
                    identity,
                    fallback_identity: fallback,
                    // macOS exposes no connector name. Saying "the built-in
                    // display" is true and useful; inventing "HDMI-1" is not.
                    connector: Some(if builtin {
                        "Built-in".to_string()
                    } else {
                        format!("Display {id}")
                    }),
                    make: Some(format!("{vendor:08X}")),
                    model: Some(format!("{model:08X}")),
                    geometry: Rect::new(
                        bounds.origin.x as i32,
                        bounds.origin.y as i32,
                        bounds.size.width.max(0.0) as u32,
                        bounds.size.height.max(0.0) as u32,
                    ),
                    scale_factor,
                    physical_size_mm: (size_mm.width > 0.0 && size_mm.height > 0.0)
                        .then_some((size_mm.width.round() as u32, size_mm.height.round() as u32)),
                    builtin,
                    primary: id == main,
                    handle: id as u64,
                },
                bounds,
            });
        }
        Ok(out)
    }
}

/// Backing scale, as the ratio of a mode's pixel width to its point width.
/// `NSScreen.backingScaleFactor` would need an AppKit round trip on the main
/// thread; this is the same number from the layer that already owns it.
fn backing_scale(id: CgDirectDisplayId) -> f64 {
    // SAFETY: `CGDisplayCopyDisplayMode` returns a +1 reference or null; it is
    // released on every path below.
    unsafe {
        let mode = CGDisplayCopyDisplayMode(id);
        if mode.is_null() {
            return 1.0;
        }
        let points = CGDisplayModeGetWidth(mode);
        let pixels = CGDisplayModeGetPixelWidth(mode);
        CGDisplayModeRelease(mode);
        if points == 0 {
            1.0
        } else {
            pixels as f64 / points as f64
        }
    }
}

/// Convert a CoreGraphics rectangle (origin top-left of the main display, `y`
/// down) into an AppKit frame (origin bottom-left of the main display, `y` up).
fn flip_to_appkit(rect: CgRect, main_height: f64) -> CgRect {
    CgRect {
        origin: CgPoint {
            x: rect.origin.x,
            y: main_height - rect.origin.y - rect.size.height,
        },
        size: rect.size,
    }
}

/// Whether a window ended up on the monitor it was sent to.
///
/// Majority overlap rather than an exact match, for the same reason the X11
/// adapter uses one: a rounded size or an in-flight animation frame is not
/// evidence of a refused placement. Both rectangles are assumed to be in the
/// same (CoreGraphics) coordinate space.
fn landed_on(window: &CgRect, target: &CgRect) -> bool {
    let window_area = window.size.width * window.size.height;
    if window_area <= 0.0 {
        return false;
    }
    let x0 = window.origin.x.max(target.origin.x);
    let y0 = window.origin.y.max(target.origin.y);
    let x1 = (window.origin.x + window.size.width).min(target.origin.x + target.size.width);
    let y1 = (window.origin.y + window.size.height).min(target.origin.y + target.size.height);
    if x1 <= x0 || y1 <= y0 {
        return false;
    }
    (x1 - x0) * (y1 - y0) * 2.0 > window_area
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

impl DisplayBackend for MacosBackend {
    fn name(&self) -> &'static str {
        "coregraphics"
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
        Capabilities {
            // AppKit honours a frame on any display.
            arbitrary_position: true,
            // Leaving fullscreen restores AppKit's own saved frame, which it
            // clamps to the visible desktop.
            unfullscreen_safe: true,
            // A frame set before the window is ordered in is honoured.
            place_before_map: true,
        }
    }

    fn focus(&self, window: NativeWindow) -> PlacementOutcome {
        let Some(window) = resolve_window(window) else {
            return PlacementOutcome::Failed("that window is gone".into());
        };
        // SAFETY: `resolve_window` returned a non-null `NSWindow*`.
        unsafe { make_key_and_order_front(window) };
        PlacementOutcome::Applied
    }

    fn place(
        &self,
        window: NativeWindow,
        identity: &MonitorIdentity,
        mode: WindowMode,
    ) -> PlacementOutcome {
        let Some(ns_window) = resolve_window(window) else {
            return PlacementOutcome::Failed("that window is gone".into());
        };

        // Fresh enumeration immediately before the native call.
        let monitors = match self.enumerate_raw() {
            Ok(monitors) => monitors,
            Err(e) => return PlacementOutcome::Failed(e.to_string()),
        };
        let Some(target) = monitors
            .iter()
            .find(|raw| raw.monitor.matches_exactly(identity))
        else {
            return PlacementOutcome::Disappeared;
        };

        // SAFETY: no arguments.
        let main_bounds = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        let main_height = main_bounds.size.height;

        // SAFETY: `ns_window` is a live `NSWindow*`; every selector below is
        // part of the public `NSWindow` API and is sent with its documented
        // signature.
        unsafe {
            let fullscreen_now = style_mask(ns_window) & STYLE_MASK_FULLSCREEN != 0;

            match mode {
                WindowMode::Fullscreen => {
                    // Already fullscreen on the wrong display: leave first, so
                    // the move is not swallowed by the Space it is pinned to.
                    if fullscreen_now {
                        toggle_fullscreen(ns_window);
                    }
                    set_frame(ns_window, flip_to_appkit(target.bounds, main_height), true);
                    toggle_fullscreen(ns_window);
                    // `toggleFullScreen:` kicks off an asynchronous Space
                    // transition and returns immediately: nothing here knows
                    // whether it actually landed (a fast subsequent request,
                    // a full-screen-hostile app, or the animation simply not
                    // having finished can all leave AppKit to restore the
                    // frame just set above once the transition settles).
                    // `Applied` cannot be claimed from an async result that
                    // has not been observed to complete; the X11 adapter
                    // models this the same way. (§77.1, unverified on real
                    // hardware — this crate cannot be compiled here.)
                    PlacementOutcome::Pending
                }
                WindowMode::Windowed => {
                    if fullscreen_now {
                        toggle_fullscreen(ns_window);
                    }
                    // Inset rather than filling the display: a windowed
                    // presenter that exactly covers a screen is
                    // indistinguishable from a fullscreen one.
                    let inset = CgRect {
                        origin: CgPoint {
                            x: target.bounds.origin.x + target.bounds.size.width * 0.05,
                            y: target.bounds.origin.y + target.bounds.size.height * 0.05,
                        },
                        size: CgSize {
                            width: target.bounds.size.width * 0.9,
                            height: target.bounds.size.height * 0.9,
                        },
                    };
                    set_frame(ns_window, flip_to_appkit(inset, main_height), true);
                    PlacementOutcome::Applied
                }
            }
        }
    }

    fn verify_placement(
        &self,
        window: NativeWindow,
        identity: &MonitorIdentity,
        final_attempt: bool,
    ) -> PlacementOutcome {
        let Some(ns_window) = resolve_window(window) else {
            return PlacementOutcome::Disappeared;
        };
        let monitors = match self.enumerate_raw() {
            Ok(monitors) => monitors,
            Err(e) => return PlacementOutcome::Failed(e.to_string()),
        };
        let Some(target) = monitors
            .iter()
            .find(|raw| raw.monitor.matches_exactly(identity))
        else {
            return PlacementOutcome::Disappeared;
        };

        // SAFETY: no arguments.
        let main_bounds = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        // SAFETY: `ns_window` was just resolved live.
        let observed_appkit = unsafe { window_frame(ns_window) };
        // `flip_to_appkit` is its own inverse (see the module tests), so
        // applying it a second time turns the observed AppKit frame back
        // into the CoreGraphics space `target.bounds` is in.
        let observed = flip_to_appkit(observed_appkit, main_bounds.size.height);

        if landed_on(&observed, &target.bounds) {
            PlacementOutcome::Applied
        } else if final_attempt {
            PlacementOutcome::Refused
        } else {
            PlacementOutcome::Pending
        }
    }
}

/// Turn the toolkit's token into a live `NSWindow*`.
///
/// `raw-window-handle` hands out the `NSView`, so the owning window is asked
/// for on the spot and immediately forgotten.
fn resolve_window(window: NativeWindow) -> Option<Id> {
    let view = window.0 as Id;
    if view.is_null() {
        return None;
    }
    // SAFETY: the application only ever supplies a pointer it obtained from
    // the toolkit for a window that is currently open.
    let ns_window = unsafe { view_window(view) };
    (!ns_window.is_null()).then_some(ns_window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_appkit_flip_is_its_own_inverse() {
        // A 1080p display sitting below a 1200px-tall main display.
        let main_height = 1200.0;
        let cg = CgRect {
            origin: CgPoint { x: 0.0, y: 1200.0 },
            size: CgSize {
                width: 1920.0,
                height: 1080.0,
            },
        };
        let appkit = flip_to_appkit(cg, main_height);
        // Below the main display in CoreGraphics means negative in AppKit.
        assert_eq!(appkit.origin.y, -1080.0);
        assert_eq!(flip_to_appkit(appkit, main_height).origin.y, cg.origin.y);
    }

    #[test]
    fn the_main_display_flips_to_the_origin() {
        let main = CgRect {
            origin: CgPoint { x: 0.0, y: 0.0 },
            size: CgSize {
                width: 1920.0,
                height: 1200.0,
            },
        };
        assert_eq!(
            flip_to_appkit(main, 1200.0).origin,
            CgPoint { x: 0.0, y: 0.0 }
        );
    }

    #[test]
    fn a_window_on_the_requested_monitor_counts_as_placed() {
        let target = CgRect {
            origin: CgPoint { x: 1920.0, y: 0.0 },
            size: CgSize {
                width: 1920.0,
                height: 1080.0,
            },
        };
        assert!(landed_on(&target, &target));
    }

    #[test]
    fn a_window_left_on_the_other_monitor_counts_as_refused() {
        let target = CgRect {
            origin: CgPoint { x: 1920.0, y: 0.0 },
            size: CgSize {
                width: 1920.0,
                height: 1080.0,
            },
        };
        let elsewhere = CgRect {
            origin: CgPoint { x: 0.0, y: 0.0 },
            size: CgSize {
                width: 1920.0,
                height: 1200.0,
            },
        };
        assert!(!landed_on(&elsewhere, &target));
    }

    #[test]
    fn a_zero_sized_window_is_not_evidence_of_placement() {
        let target = CgRect {
            origin: CgPoint { x: 0.0, y: 0.0 },
            size: CgSize {
                width: 1920.0,
                height: 1080.0,
            },
        };
        let empty = CgRect {
            origin: CgPoint { x: 0.0, y: 0.0 },
            size: CgSize {
                width: 0.0,
                height: 0.0,
            },
        };
        assert!(!landed_on(&empty, &target));
    }

    #[test]
    fn coregraphics_structs_match_the_c_abi() {
        // `CGRect` is passed and returned by value; a wrong size here is
        // silent stack corruption on a machine this test cannot reach.
        assert_eq!(std::mem::size_of::<CgPoint>(), 16);
        assert_eq!(std::mem::size_of::<CgSize>(), 16);
        assert_eq!(std::mem::size_of::<CgRect>(), 32);
    }
}
