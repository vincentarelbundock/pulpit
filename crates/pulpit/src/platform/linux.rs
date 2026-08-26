//! The Linux desktop adapter.
//!
//! Everything here goes through the XDG desktop portal or a documented
//! fallback chain, and every step reports what actually happened. This module
//! is the only place in the workspace that knows what a D-Bus name is.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::platform::appearance::SystemAppearance;
use crate::platform::capabilities::{
    Capabilities, IdentityQuality, TOOLKIT_PUBLISHES_AN_ACCESSIBILITY_TREE,
};
use crate::platform::inhibit::{InhibitState, InhibitToken};
use crate::platform::paths::Directories;
use crate::platform::services::{Notification, PlatformServices, Urgency};
use crate::platform::Outcome;

const APP_ID: &str = "com.example.pulpit";
const REASON: &str = "Presentation in progress";
/// Portal flags: 4 = suspend, 8 = idle.
const INHIBIT_SUSPEND_AND_IDLE: u32 = 4 | 8;

#[derive(Debug)]
pub struct LinuxServices {
    wayland: bool,
    x11: bool,
}

impl Default for LinuxServices {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxServices {
    pub fn new() -> LinuxServices {
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
        LinuxServices {
            wayland: session == "wayland" || std::env::var_os("WAYLAND_DISPLAY").is_some(),
            x11: session != "wayland" && std::env::var_os("DISPLAY").is_some(),
        }
    }

    fn session_bus(&self) -> Option<Connection> {
        Connection::session().ok()
    }
}

impl PlatformServices for LinuxServices {
    fn name(&self) -> &'static str {
        "linux-xdg"
    }

    fn capabilities(&self) -> Capabilities {
        let portal_present = self.session_bus().is_some();
        Capabilities {
            backend: if self.wayland {
                "wayland".into()
            } else if self.x11 {
                "x11".into()
            } else {
                "headless".into()
            },
            // The display adapter refines this; what is reported here is what
            // the session type makes possible at best.
            identity: if self.x11 {
                IdentityQuality::Stable
            } else if self.wayland {
                IdentityQuality::Connector
            } else {
                IdentityQuality::None
            },
            // X11 places windows; Wayland leaves it to the compositor.
            arbitrary_placement: self.x11,
            safe_unfullscreen: self.x11,
            place_before_map: false,
            system_appearance: portal_present,
            high_contrast_detection: portal_present,
            sleep_inhibition: portal_present || which("systemd-inhibit"),
            native_dialogs: true,
            native_menus: false,
            // Both halves, not just the session's. AT-SPI is running on any
            // GNOME desktop with Orca installed, and answering "bridge
            // present" from that alone told a screen-reader user their reader
            // would work here — while Iced published no tree for it to read.
            accessibility_bridge: TOOLKIT_PUBLISHES_AN_ACCESSIBILITY_TREE
                && (std::env::var_os("AT_SPI_BUS").is_some()
                    || std::env::var_os("GTK_MODULES")
                        .is_some_and(|modules| modules.to_string_lossy().contains("atk-bridge"))),
            media_keys: true,
            notifications: portal_present,
            // A clipboard belongs to a display server: X11 has a selection
            // owner and Wayland has the data-control protocol, and a
            // headless session has neither. Answered from the session type
            // rather than by opening a clipboard at startup, which on
            // Wayland means a connection and a thread for a question nobody
            // has asked yet.
            image_clipboard: self.x11 || self.wayland,
        }
    }

    fn directories(&self) -> Directories {
        Directories::detect()
    }

    fn system_appearance(&self) -> SystemAppearance {
        // org.freedesktop.appearance/color-scheme: 0 = no preference,
        // 1 = prefer dark, 2 = prefer light.
        let Some(connection) = self.session_bus() else {
            return SystemAppearance::Unknown;
        };
        let Ok(proxy) = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        ) else {
            return SystemAppearance::Unknown;
        };
        let contrast: Result<zbus::zvariant::OwnedValue, _> =
            proxy.call("ReadOne", &("org.freedesktop.appearance", "contrast"));
        if let Ok(value) = contrast {
            if u32::try_from(&value).ok() == Some(1) {
                return SystemAppearance::HighContrast;
            }
        }
        let scheme: Result<zbus::zvariant::OwnedValue, _> =
            proxy.call("ReadOne", &("org.freedesktop.appearance", "color-scheme"));
        match scheme.ok().and_then(|value| u32::try_from(&value).ok()) {
            Some(1) => SystemAppearance::Dark,
            Some(2) => SystemAppearance::Light,
            _ => SystemAppearance::Unknown,
        }
    }

    fn reduced_motion(&self) -> crate::platform::appearance::MotionPreference {
        use crate::platform::appearance::MotionPreference;
        // The portal does not carry this one, so it comes from GNOME's
        // interface schema, which KDE and others also honour through
        // xdg-desktop-portal-gtk. A desktop that has neither says nothing,
        // which is the honest answer.
        let Some(connection) = self.session_bus() else {
            return MotionPreference::Unknown;
        };
        let Ok(proxy) = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        ) else {
            return MotionPreference::Unknown;
        };
        let value: Result<zbus::zvariant::OwnedValue, _> = proxy.call(
            "ReadOne",
            &("org.gnome.desktop.interface", "enable-animations"),
        );
        match value.ok().and_then(|value| bool::try_from(&value).ok()) {
            Some(true) => MotionPreference::Full,
            Some(false) => MotionPreference::Reduced,
            None => MotionPreference::Unknown,
        }
    }

    fn reveal(&self, path: &Path) -> Outcome {
        // The portal's OpenURI.OpenDirectory needs a file descriptor; the
        // documented fallback is the desktop's own opener on the parent
        // directory, which every tested desktop honours.
        let Some(parent) = path.parent() else {
            return Outcome::failed("that file has no directory");
        };
        spawn("xdg-open", &[parent.as_os_str().to_string_lossy().as_ref()])
    }

    fn open(&self, target: &str) -> Outcome {
        spawn("xdg-open", &[target])
    }

    fn notify(&self, notification: &Notification) -> Outcome {
        let urgency = match notification.urgency {
            Urgency::Normal => "normal",
            Urgency::Critical => "critical",
        };
        spawn(
            "notify-send",
            &[
                "--app-name=pulpit",
                &format!("--urgency={urgency}"),
                &notification.title,
                &notification.body,
            ],
        )
    }

    /// On Wayland the region goes out as pixels and as a written PNG file,
    /// so a file manager can paste it as well as an image editor; see
    /// [`crate::platform::clipboard::copy_image_wayland`]. X11 keeps the
    /// one-format arboard path: multiple targets there would mean owning a
    /// selection by hand, for a session that is no longer the common case.
    fn copy_image(&self, image: &crate::platform::clipboard::ClipboardImage) -> Outcome {
        if self.wayland {
            let outcome = crate::platform::clipboard::copy_image_wayland(
                image,
                &self.directories().cache.join("clipboard"),
            );
            if !matches!(outcome, Outcome::Failed { .. }) {
                return outcome;
            }
            // The three-format offer failed — most likely a compositor
            // without the data-control protocol. Pixels alone still serve
            // every paste but the file manager's, so fall through rather
            // than give up the whole copy.
        }
        crate::platform::clipboard::copy_image(image)
    }

    fn inhibit(&self) -> InhibitState {
        let mut attempts = Vec::new();

        // 1. The portal: works under Flatpak, Wayland and X11.
        match self.session_bus() {
            Some(connection) => {
                let proxy = zbus::blocking::Proxy::new(
                    &connection,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Inhibit",
                );
                match proxy {
                    Ok(proxy) => {
                        let mut options: std::collections::HashMap<&str, Value> =
                            std::collections::HashMap::new();
                        options.insert("reason", Value::from(REASON));
                        let request: Result<OwnedObjectPath, _> =
                            proxy.call("Inhibit", &("", INHIBIT_SUSPEND_AND_IDLE, options));
                        match request {
                            Ok(path) => {
                                return InhibitState::Held {
                                    mechanism: "xdg-desktop-portal",
                                    token: InhibitToken::Handle(path.as_str().to_string()),
                                }
                            }
                            Err(e) => attempts.push(format!("portal: {e}")),
                        }
                    }
                    Err(e) => attempts.push(format!("portal proxy: {e}")),
                }

                // 2. The long-standing screensaver interface.
                let screensaver = zbus::blocking::Proxy::new(
                    &connection,
                    "org.freedesktop.ScreenSaver",
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                );
                match screensaver {
                    Ok(proxy) => match proxy.call::<_, _, u32>("Inhibit", &(APP_ID, REASON)) {
                        Ok(cookie) => {
                            return InhibitState::Held {
                                mechanism: "org.freedesktop.ScreenSaver",
                                token: InhibitToken::Cookie(cookie),
                            }
                        }
                        Err(e) => attempts.push(format!("screensaver: {e}")),
                    },
                    Err(e) => attempts.push(format!("screensaver proxy: {e}")),
                }
            }
            None => attempts.push("session bus: unavailable".into()),
        }

        // 3. A child process, which the kernel reaps if we die mid-talk.
        match Command::new("systemd-inhibit")
            .args([
                "--what=idle:sleep",
                "--who=pulpit",
                &format!("--why={REASON}"),
                "--mode=block",
                "sleep",
                "infinity",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => InhibitState::Held {
                mechanism: "systemd-inhibit",
                token: InhibitToken::Process(child.id()),
            },
            Err(e) => {
                attempts.push(format!("systemd-inhibit: {e}"));
                InhibitState::Unavailable {
                    reason: "no inhibition mechanism answered".into(),
                    attempts,
                }
            }
        }
    }

    fn release_inhibit(&self, state: &InhibitState) -> Outcome {
        let InhibitState::Held { token, .. } = state else {
            return Outcome::Done;
        };
        match token {
            InhibitToken::Handle(path) => {
                let Some(connection) = self.session_bus() else {
                    return Outcome::failed("the session bus went away");
                };
                match zbus::blocking::Proxy::new(
                    &connection,
                    "org.freedesktop.portal.Desktop",
                    path.as_str(),
                    "org.freedesktop.portal.Request",
                )
                .and_then(|proxy| proxy.call::<_, _, ()>("Close", &()))
                {
                    Ok(()) => Outcome::Done,
                    Err(e) => Outcome::failed(e.to_string()),
                }
            }
            InhibitToken::Cookie(cookie) => {
                let Some(connection) = self.session_bus() else {
                    return Outcome::failed("the session bus went away");
                };
                match zbus::blocking::Proxy::new(
                    &connection,
                    "org.freedesktop.ScreenSaver",
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                )
                .and_then(|proxy| proxy.call::<_, _, ()>("UnInhibit", &(*cookie,)))
                {
                    Ok(()) => Outcome::Done,
                    Err(e) => Outcome::failed(e.to_string()),
                }
            }
            InhibitToken::Process(pid) => {
                // The child is ours; SIGTERM releases the lock.
                match Command::new("kill").arg(pid.to_string()).status() {
                    Ok(status) if status.success() => Outcome::Done,
                    Ok(status) => Outcome::failed(format!("kill exited with {status}")),
                    Err(e) => Outcome::failed(e.to_string()),
                }
            }
            InhibitToken::None => Outcome::Done,
        }
    }

    fn recent_documents(&self) -> Option<Vec<PathBuf>> {
        // recently-used.xbel is a GTK convention rather than a portal one, and
        // parsing it would mean guessing at another desktop's private file.
        None
    }
}

fn spawn(program: &str, arguments: &[&str]) -> Outcome {
    match Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => Outcome::Done,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Outcome::Unsupported {
            what: "This desktop integration",
        },
        Err(e) => Outcome::failed(e.to_string()),
    }
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|directory| directory.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapter_reports_the_session_it_is_in() {
        let services = LinuxServices::new();
        let capabilities = services.capabilities();
        assert!(["x11", "wayland", "headless"].contains(&capabilities.backend.as_str()));
        // Wayland cannot place windows; X11 can. Either way the claim matches
        // the session rather than the operating system.
        if capabilities.backend == "wayland" {
            assert!(!capabilities.arbitrary_placement);
        }
        assert!(
            capabilities.native_dialogs,
            "a portal file dialog is always offered"
        );
    }

    #[test]
    fn a_missing_helper_is_unsupported_rather_than_a_crash() {
        assert!(matches!(
            spawn("pulpit-definitely-not-a-real-program", &[]),
            Outcome::Unsupported { .. }
        ));
    }

    #[test]
    fn releasing_nothing_is_fine() {
        let services = LinuxServices::new();
        assert_eq!(
            services.release_inhibit(&InhibitState::Released),
            Outcome::Done
        );
    }

    /// The one capability whose false positive is worse than its false
    /// negative.
    ///
    /// This is the only adapter that *detects* an accessibility bridge rather
    /// than hardcoding its absence, and AT-SPI is running on any GNOME desktop
    /// with a screen reader installed — so this machine may well have the bus.
    /// Having the bus is not having a bridge: Iced publishes no tree, so the
    /// honest answer is "absent" no matter what the session offers, and a
    /// screen-reader user reading the diagnostics is told the truth rather
    /// than the encouraging half of it.
    ///
    /// The environment is set here rather than sampled, so this fails on a CI
    /// runner with no desktop as surely as it would on a full GNOME session.
    #[test]
    fn a_session_bus_alone_is_not_an_accessibility_bridge() {
        // SAFETY: single-threaded test; no other thread reads the environment
        // while this runs.
        unsafe { std::env::set_var("AT_SPI_BUS", "unix:path=/run/user/1000/at-spi/bus") };
        let capabilities = LinuxServices::new().capabilities();
        unsafe { std::env::remove_var("AT_SPI_BUS") };
        assert!(
            !capabilities.accessibility_bridge,
            "the session's accessibility bus was reported as a bridge pulpit \
             cannot publish anything on; assistive technology reads nothing \
             here until the toolkit exposes a tree"
        );
    }
}
