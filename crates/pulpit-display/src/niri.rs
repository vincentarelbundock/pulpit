//! Niri placement over its command-line IPC client.
//!
//! The ordinary Wayland protocol lets us enumerate outputs, but Iced does not
//! expose the `xdg_toplevel` needed for targeted fullscreen. Niri's documented
//! IPC fills that gap: windows have compositor ids and the
//! `move-window-to-monitor` action sends a window to the selected output's
//! active workspace. Invoking `niri msg` instead of speaking the socket
//! protocol directly also keeps this adapter compatible with the exact IPC
//! version shipped by the running compositor.

use std::process::Command;

use serde::Deserialize;

use crate::backend::{BackendError, DisplayBackend, NativeWindow, PlacementOutcome};
use crate::identity::MonitorIdentity;
use crate::reconcile::{Capabilities, WindowMode};
use crate::roles::Role;
use crate::snapshot::DisplaySnapshot;
use crate::wayland::WaylandBackend;

/// Stable app ids assigned to the two Iced windows on Linux.
pub const PRESENTER_APP_ID: &str = "pulpit";
pub const AUDIENCE_APP_ID: &str = "pulpit-audience";

const PRESENTER_TOKEN: u64 = 1;
const AUDIENCE_TOKEN: u64 = 2;

#[derive(Debug, Deserialize)]
struct Window {
    id: u64,
    app_id: Option<String>,
}

/// Wayland enumeration plus Niri-specific window placement.
pub struct NiriBackend {
    outputs: WaylandBackend,
}

impl std::fmt::Debug for NiriBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NiriBackend").finish_non_exhaustive()
    }
}

impl NiriBackend {
    /// Check for a working Niri IPC client. `NIRI_SOCKET` is authoritative:
    /// merely having the binary installed does not mean this is the
    /// compositor for the current session.
    pub fn available() -> Result<(), BackendError> {
        if std::env::var_os("NIRI_SOCKET").is_none() {
            return Err(BackendError::Unavailable);
        }
        let probe = Command::new("niri")
            .args(["msg", "-j", "version"])
            .output()
            .map_err(|e| BackendError::Protocol(format!("cannot run niri IPC client: {e}")))?;
        if !probe.status.success() {
            return Err(BackendError::Protocol(command_error(
                "niri IPC probe",
                &probe,
            )));
        }
        Ok(())
    }

    pub fn new(outputs: WaylandBackend) -> Self {
        Self { outputs }
    }

    fn role_for_token(token: NativeWindow) -> Option<Role> {
        match token.0 {
            PRESENTER_TOKEN => Some(Role::Presenter),
            AUDIENCE_TOKEN => Some(Role::Audience),
            _ => None,
        }
    }

    fn app_id(role: Role) -> &'static str {
        match role {
            Role::Presenter => PRESENTER_APP_ID,
            Role::Audience => AUDIENCE_APP_ID,
        }
    }

    fn compositor_window(role: Role) -> Result<Option<u64>, BackendError> {
        let output = Command::new("niri")
            .args(["msg", "-j", "windows"])
            .output()
            .map_err(|e| BackendError::Protocol(format!("cannot list Niri windows: {e}")))?;
        if !output.status.success() {
            return Err(BackendError::Protocol(command_error(
                "listing Niri windows",
                &output,
            )));
        }
        let windows: Vec<Window> = serde_json::from_slice(&output.stdout)
            .map_err(|e| BackendError::Protocol(format!("invalid Niri window list: {e}")))?;
        Ok(windows
            .into_iter()
            .find(|window| window.app_id.as_deref() == Some(Self::app_id(role)))
            .map(|window| window.id))
    }
}

impl DisplayBackend for NiriBackend {
    fn name(&self) -> &'static str {
        "niri-wayland"
    }

    fn snapshot(&self) -> Result<DisplaySnapshot, BackendError> {
        self.outputs.snapshot()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            targeted_fullscreen: true,
            arbitrary_position: false,
            unfullscreen_safe: true,
            place_before_map: false,
        }
    }

    fn window_token(&self, role: Role) -> Option<NativeWindow> {
        Some(NativeWindow(match role {
            Role::Presenter => PRESENTER_TOKEN,
            Role::Audience => AUDIENCE_TOKEN,
        }))
    }

    fn focus(&self, window: NativeWindow) -> PlacementOutcome {
        let Some(role) = Self::role_for_token(window) else {
            return PlacementOutcome::Failed("invalid Niri window token".into());
        };
        let compositor_window = match Self::compositor_window(role) {
            Ok(Some(id)) => id,
            Ok(None) => return PlacementOutcome::Refused,
            Err(e) => return PlacementOutcome::Failed(e.to_string()),
        };
        let id = compositor_window.to_string();
        match Command::new("niri")
            .args(["msg", "action", "focus-window", "--id", &id])
            .output()
        {
            Ok(output) if output.status.success() => PlacementOutcome::Applied,
            Ok(output) => {
                PlacementOutcome::Failed(command_error("focusing a window with Niri", &output))
            }
            Err(e) => PlacementOutcome::Failed(format!("cannot run niri IPC client: {e}")),
        }
    }

    fn place(
        &self,
        window: NativeWindow,
        identity: &MonitorIdentity,
        _mode: WindowMode,
    ) -> PlacementOutcome {
        let Some(role) = Self::role_for_token(window) else {
            return PlacementOutcome::Failed("invalid Niri window token".into());
        };

        // Re-resolve immediately before the IPC action. Output names are the
        // common identity between Wayland's xdg-output and Niri's IPC.
        let snapshot = match self.snapshot() {
            Ok(snapshot) => snapshot,
            Err(e) => return PlacementOutcome::Failed(e.to_string()),
        };
        let Some(output_name) = snapshot.monitors.iter().find_map(|monitor| {
            (&monitor.identity == identity || monitor.fallback_identity.as_ref() == Some(identity))
                .then(|| monitor.connector.clone())
                .flatten()
        }) else {
            return PlacementOutcome::Disappeared;
        };

        let compositor_window = match Self::compositor_window(role) {
            Ok(Some(id)) => id,
            // A hidden Wayland window is not visible to Niri yet. The caller
            // maps it and uses its bounded post-map placement retry.
            Ok(None) => return PlacementOutcome::Refused,
            Err(e) => return PlacementOutcome::Failed(e.to_string()),
        };
        let id = compositor_window.to_string();
        let result = Command::new("niri")
            .args([
                "msg",
                "action",
                "move-window-to-monitor",
                "--id",
                &id,
                &output_name,
            ])
            .output();
        match result {
            Ok(output) if output.status.success() => PlacementOutcome::Applied,
            Ok(output) => {
                PlacementOutcome::Failed(command_error("moving a window with Niri", &output))
            }
            Err(e) => PlacementOutcome::Failed(format!("cannot run niri IPC client: {e}")),
        }
    }
}

fn command_error(context: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("{context} failed with {}", output.status)
    } else {
        format!("{context} failed: {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_tokens_and_app_ids_do_not_overlap() {
        assert_eq!(
            NiriBackend::role_for_token(NativeWindow(PRESENTER_TOKEN)),
            Some(Role::Presenter)
        );
        assert_eq!(
            NiriBackend::role_for_token(NativeWindow(AUDIENCE_TOKEN)),
            Some(Role::Audience)
        );
        assert_ne!(PRESENTER_APP_ID, AUDIENCE_APP_ID);
    }

    #[test]
    fn window_list_tolerates_the_rest_of_niris_schema() {
        let windows: Vec<Window> = serde_json::from_str(
            r#"[{"id":7,"title":"slides","app_id":"pulpit-audience","workspace_id":3}]"#,
        )
        .unwrap();
        assert_eq!(windows[0].id, 7);
        assert_eq!(windows[0].app_id.as_deref(), Some(AUDIENCE_APP_ID));
    }
}
