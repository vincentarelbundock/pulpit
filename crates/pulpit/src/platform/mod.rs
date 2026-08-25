//! The platform boundary.
//!
//! Four contracts separate "what pulpit wants" from "what this desktop
//! can do": [`PlatformServices`], [`WindowPolicy`], [`InputPolicy`] and the
//! [`Capabilities`] snapshot. (The fifth, `DisplayBackend`, lives in
//! `pulpit-display` with the reconciliation logic it serves.)
//!
//! Two rules hold this crate together:
//!
//! 1. **Ask what the environment can do, never what it is called.** Views and
//!    state machines consume [`Capabilities`]; only the adapters inside this
//!    crate know an operating system exists.
//! 2. **An unavailable capability produces a deliberate outcome** — a
//!    documented fallback, a specific manual instruction, or a disabled
//!    command with a reason. [`Outcome`] makes silent no-ops impossible to
//!    express.
//!
//! Every adapter has a [`null::NullPlatform`] counterpart so the application
//! can be tested with no desktop at all.

// These modules were standalone library crates until the workspace was
// consolidated. They keep their complete, tested APIs — the parts the
// application does not happen to call yet are exercised by the tests beside
// them, and pruning them would throw away working, documented behaviour to
// satisfy a lint about a boundary that no longer exists.
#![allow(dead_code)]

pub mod appearance;
pub mod capabilities;
pub mod clipboard;
pub mod inhibit;
pub mod input;
pub mod instance;
pub mod null;
pub mod paths;
pub mod services;
pub mod window;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

pub use appearance::{Appearance, Motion, MotionSetting};
pub use capabilities::Capabilities;
pub use clipboard::ClipboardImage;
pub use inhibit::Inhibitor;
pub use input::{InputPolicy, Shortcut};
pub use instance::{acquire as acquire_claim, is_claimed, Instance, InstanceLock};
pub use paths::Directories;
pub use services::PlatformServices;
pub use window::{Bounds, WindowPolicy};

/// The result of asking the platform to do something.
///
/// There is deliberately no `Ok(())`: a caller must distinguish "it happened"
/// from "the platform declined" from "this build cannot do that", because the
/// user-visible consequence differs in each case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The platform performed the operation.
    Done,
    /// The platform understood and refused — compositor policy, a permission,
    /// a user cancelling a dialog.
    Refused { reason: String },
    /// This build or this desktop has no implementation. The caller must fall
    /// back or disable the command.
    Unsupported { what: &'static str },
    /// It was attempted and failed.
    Failed { reason: String },
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, Outcome::Done)
    }

    /// One sentence for the user, or `None` when nothing needs saying.
    pub fn describe(&self) -> Option<String> {
        match self {
            Outcome::Done => None,
            Outcome::Refused { reason } => Some(reason.clone()),
            Outcome::Unsupported { what } => {
                Some(format!("{what} is not available in this session."))
            }
            Outcome::Failed { reason } => Some(reason.clone()),
        }
    }

    pub fn refused(reason: impl Into<String>) -> Outcome {
        Outcome::Refused {
            reason: reason.into(),
        }
    }

    pub fn failed(reason: impl Into<String>) -> Outcome {
        Outcome::Failed {
            reason: reason.into(),
        }
    }
}

/// Everything the application needs from the desktop, in one value.
///
/// Constructed once at startup by [`detect`], and re-derived when the session
/// changes. Holding the adapters together makes the dependency obvious at the
/// call site and keeps every platform decision in this crate.
pub struct Platform {
    pub services: Box<dyn PlatformServices>,
    pub window: Box<dyn WindowPolicy>,
    pub input: Box<dyn InputPolicy>,
    pub capabilities: Capabilities,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform")
            .field("services", &self.services.name())
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl Platform {
    /// Choose the adapters for the running desktop.
    pub fn detect() -> Platform {
        // Each arm builds the same shape: an adapter, then the capability
        // snapshot *it* reports. Nothing above this function may ask which arm
        // ran.
        #[cfg(all(unix, not(target_os = "macos")))]
        let services: Box<dyn PlatformServices> = Box::new(linux::LinuxServices::new());
        #[cfg(target_os = "macos")]
        let services: Box<dyn PlatformServices> = Box::new(macos::MacosServices::new());
        #[cfg(target_os = "windows")]
        let services: Box<dyn PlatformServices> = Box::new(windows::WindowsServices::new());
        // A target with no adapter still runs, and says so.
        #[cfg(not(any(
            all(unix, not(target_os = "macos")),
            target_os = "macos",
            target_os = "windows"
        )))]
        let services: Box<dyn PlatformServices> = Box::new(null::NullPlatform::new("portable"));

        let capabilities = services.capabilities();
        Platform {
            services,
            window: Box::new(window::DesktopWindowPolicy),
            input: Box::new(input::DesktopInput),
            capabilities,
        }
    }

    /// A fully deterministic platform for tests.
    pub fn null() -> Platform {
        let services = null::NullPlatform::new("null");
        let capabilities = services.capabilities();
        Platform {
            services: Box::new(services),
            window: Box::new(window::DesktopWindowPolicy),
            input: Box::new(input::DesktopInput),
            capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_always_explains_itself_unless_it_worked() {
        assert_eq!(Outcome::Done.describe(), None);
        assert!(Outcome::Done.succeeded());

        for outcome in [
            Outcome::refused("the compositor declined"),
            Outcome::Unsupported {
                what: "Targeted fullscreen",
            },
            Outcome::failed("the connection dropped"),
        ] {
            assert!(!outcome.succeeded());
            let explanation = outcome.describe().expect("a failure must explain itself");
            assert!(
                explanation.len() > 10,
                "{explanation:?} is not an explanation"
            );
        }
    }

    #[test]
    fn the_null_platform_is_complete_and_honest() {
        let platform = Platform::null();
        assert_eq!(platform.services.name(), "null");
        // Nothing is claimed that a headless test environment cannot do.
        assert!(!platform.capabilities.native_dialogs);
        assert!(!platform.capabilities.sleep_inhibition);
        assert!(!platform.capabilities.system_appearance);
    }
}
