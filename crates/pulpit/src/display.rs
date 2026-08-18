//! Glue between Iced windows and the display extension.
//!
//! Iced 0.14 exposes no monitor enumeration, so this module resolves a native
//! window handle and hands it to a platform adapter for the duration of one
//! call — no native handle is ever stored in application state.

use std::sync::Arc;

use iced::window;
use pulpit_display::backend::NativeWindow;
use pulpit_display::{
    Capabilities, DisplayBackend, DisplayRoles, DisplaySnapshot, NullBackend, PlacementOutcome,
    Reconciler, Role, WindowMode, WindowState, Windows,
};

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
    pub fn refresh(&mut self) -> bool {
        match self.backend.snapshot() {
            Ok(snapshot) => {
                let changed = !snapshot.same_topology(&self.snapshot);
                self.snapshot = snapshot;
                changed
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot enumerate monitors");
                false
            }
        }
    }

    pub fn window_state_mut(&mut self, role: Role) -> &mut WindowState {
        self.windows.get_mut(role)
    }
}

/// Choose the platform adapter for this session.
fn detect_backend() -> (Arc<dyn DisplayBackend>, Capabilities) {
    #[cfg(all(unix, not(target_os = "macos")))]
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();

    #[cfg(target_os = "windows")]
    {
        match pulpit_display::windows::WindowsBackend::connect() {
            Ok(backend) => {
                let capabilities = backend.capabilities();
                return (Arc::new(backend), capabilities);
            }
            // A session with no interactive desktop — a service, a locked-down
            // session — enumerates nothing. The null backend below keeps the
            // application running and the UI explains what is missing.
            Err(e) => tracing::warn!(error = %e, "no Win32 display adapter"),
        }
    }

    #[cfg(target_os = "macos")]
    {
        match pulpit_display::macos::MacosBackend::connect() {
            Ok(backend) => {
                let capabilities = backend.capabilities();
                return (Arc::new(backend), capabilities);
            }
            Err(e) => tracing::warn!(error = %e, "no CoreGraphics display adapter"),
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if session != "wayland" {
            match pulpit_display::x11::X11Backend::connect() {
                Ok(backend) => {
                    let capabilities = backend.capabilities();
                    if let Err(e) = backend.subscribe_to_topology_changes() {
                        tracing::warn!(error = %e, "no XRandR change events; polling only");
                    }
                    return (Arc::new(backend), capabilities);
                }
                Err(e) => tracing::warn!(error = %e, "no X11 display adapter"),
            }
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if session == "wayland" || std::env::var_os("WAYLAND_DISPLAY").is_some() {
            match pulpit_display::wayland::WaylandBackend::connect() {
                Ok(backend) => {
                    // The Wayland backend only enumerates outputs; placement
                    // belongs to the compositor, and the UI says so.
                    let capabilities = backend.capabilities();
                    for check in backend.scale_checks().unwrap_or_default() {
                        if check.consistent {
                            tracing::info!(check = %check.describe(), "wayland scale check");
                        } else {
                            tracing::warn!(check = %check.describe(), "wayland scale check");
                        }
                    }
                    return (Arc::new(backend), capabilities);
                }
                Err(e) => tracing::warn!(error = %e, "no Wayland display adapter"),
            }
        }
    }

    (
        Arc::new(NullBackend::default()),
        Capabilities {
            arbitrary_position: false,
            unfullscreen_safe: true,
            place_before_map: false,
        },
    )
}

/// Give the compositor, and the user reading its window list, an unambiguous
/// way to distinguish the two top-level windows.
pub fn identify_window(settings: window::Settings, role: Role) -> window::Settings {
    #[cfg(target_os = "linux")]
    let settings = {
        let mut settings = settings;
        settings.platform_specific.application_id = match role {
            Role::Presenter => pulpit_display::wayland::PRESENTER_APP_ID,
            Role::Audience => pulpit_display::wayland::AUDIENCE_APP_ID,
        }
        .into();
        settings
    };
    // Windows and macOS identify their windows by the handle the toolkit
    // already hands out, so the role needs no name there.
    #[cfg(not(target_os = "linux"))]
    let _ = role;
    settings
}

/// Ask the toolkit for the native id of a window, then forget it.
pub fn native_window_id<Message: Send + 'static>(
    id: window::Id,
    to_message: impl Fn(Option<NativeWindow>) -> Message + Send + 'static,
) -> iced::Task<Message> {
    window::run(id, |handle| {
        use window::raw_window_handle::RawWindowHandle;
        let handle = handle.window_handle().ok()?;
        match handle.as_raw() {
            // Gated, not cast: `XID` is a `c_ulong`, which is 32 bits wide on
            // Windows and 64 on Linux, so an unconditional arm does not even
            // typecheck for a Windows target. An X11 handle cannot arrive
            // there in any case.
            #[cfg(all(unix, not(target_os = "macos")))]
            RawWindowHandle::Xlib(x) => Some(NativeWindow(x.window)),
            #[cfg(all(unix, not(target_os = "macos")))]
            RawWindowHandle::Xcb(x) => Some(NativeWindow(x.window.get() as u64)),
            // The `HWND` is the window itself.
            RawWindowHandle::Win32(w) => Some(NativeWindow(w.hwnd.get() as u64)),
            // AppKit hands out the *view*; the macOS backend asks it for its
            // window immediately before each native call and keeps neither.
            RawWindowHandle::AppKit(a) => Some(NativeWindow(a.ns_view.as_ptr() as u64)),
            _ => None,
        }
    })
    .map(to_message)
}

/// Map the domain's window mode onto Iced's.
pub fn iced_mode(mode: WindowMode) -> window::Mode {
    match mode {
        WindowMode::Windowed => window::Mode::Windowed,
        WindowMode::Fullscreen => window::Mode::Fullscreen,
        WindowMode::Hidden => window::Mode::Hidden,
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn compositor_can_distinguish_the_two_window_roles() {
        let presenter = identify_window(window::Settings::default(), Role::Presenter);
        let audience = identify_window(window::Settings::default(), Role::Audience);
        assert_eq!(
            presenter.platform_specific.application_id,
            pulpit_display::wayland::PRESENTER_APP_ID
        );
        assert_eq!(
            audience.platform_specific.application_id,
            pulpit_display::wayland::AUDIENCE_APP_ID
        );
    }
}
