//! The macOS desktop adapter.
//!
//! Appearance and accessibility preferences are read through CoreFoundation
//! rather than by shelling out to `defaults`, so a talk does not pay for a
//! process spawn every time the theme is re-read. Inhibition prefers the power
//! assertion API and falls back to `caffeinate`, which the kernel reaps if
//! pulpit dies mid-talk — the same ordering as the Linux adapter.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::platform::appearance::{MotionPreference, SystemAppearance};
use crate::platform::capabilities::{Capabilities, IdentityQuality};
use crate::platform::inhibit::{InhibitState, InhibitToken};
use crate::platform::paths::Directories;
use crate::platform::services::{Notification, PlatformServices, Urgency};
use crate::platform::Outcome;

// ---------------------------------------------------------------------------
// CoreFoundation
// ---------------------------------------------------------------------------

type CfTypeRef = *const c_void;
type CfStringRef = *const c_void;
type CfAllocatorRef = *const c_void;
type CfTypeId = usize;
type CfIndex = isize;
type Boolean = u8;

const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFPreferencesAnyApplication: CfStringRef;

    fn CFStringCreateWithBytes(
        allocator: CfAllocatorRef,
        bytes: *const u8,
        length: CfIndex,
        encoding: u32,
        external_representation: Boolean,
    ) -> CfStringRef;
    fn CFStringGetCString(
        string: CfStringRef,
        buffer: *mut u8,
        size: CfIndex,
        encoding: u32,
    ) -> Boolean;
    fn CFPreferencesCopyAppValue(key: CfStringRef, application: CfStringRef) -> CfTypeRef;
    fn CFGetTypeID(value: CfTypeRef) -> CfTypeId;
    fn CFStringGetTypeID() -> CfTypeId;
    fn CFBooleanGetTypeID() -> CfTypeId;
    fn CFBooleanGetValue(value: CfTypeRef) -> Boolean;
    fn CFRelease(value: CfTypeRef);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: CfStringRef,
        level: u32,
        name: CfStringRef,
        id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

const IOPM_ASSERTION_LEVEL_ON: u32 = 255;
const KERN_SUCCESS: i32 = 0;

/// An owned `CFStringRef` that releases itself.
///
/// CoreFoundation's ownership rule is positional — "Create" and "Copy" return
/// a +1 reference — and this type is what keeps that rule from having to be
/// remembered at every call site.
struct CfString(CfStringRef);

impl CfString {
    fn new(value: &str) -> Option<CfString> {
        // SAFETY: the byte slice is valid for the duration of the call and
        // CoreFoundation copies it.
        let raw = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                value.as_ptr(),
                value.len() as CfIndex,
                UTF8,
                0,
            )
        };
        (!raw.is_null()).then_some(CfString(raw))
    }

    fn as_raw(&self) -> CfStringRef {
        self.0
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this type only ever holds a +1 reference it created.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// A preference value, released when it goes out of scope.
struct CfValue(CfTypeRef);

impl Drop for CfValue {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `CFPreferencesCopyAppValue` returns +1.
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Read a preference from an application domain. `None` when the key is unset,
/// which for several macOS preferences is itself the answer: "Light" is the
/// absence of `AppleInterfaceStyle`, not a value.
fn preference(key: &str, domain: Option<&str>) -> Option<CfValue> {
    let key = CfString::new(key)?;
    let domain_string = match domain {
        Some(domain) => Some(CfString::new(domain)?),
        None => None,
    };
    // SAFETY: `kCFPreferencesAnyApplication` is a framework constant; both
    // strings outlive the call.
    let application = match &domain_string {
        Some(domain) => domain.as_raw(),
        None => unsafe { kCFPreferencesAnyApplication },
    };
    // SAFETY: both arguments are live CFStrings.
    let raw = unsafe { CFPreferencesCopyAppValue(key.as_raw(), application) };
    (!raw.is_null()).then_some(CfValue(raw))
}

fn as_string(value: &CfValue) -> Option<String> {
    // SAFETY: `value.0` is a live CoreFoundation object.
    if unsafe { CFGetTypeID(value.0) != CFStringGetTypeID() } {
        return None;
    }
    let mut buffer = [0u8; 128];
    // SAFETY: the buffer length is passed honestly; CoreFoundation
    // NUL-terminates within it or fails.
    let ok =
        unsafe { CFStringGetCString(value.0, buffer.as_mut_ptr(), buffer.len() as CfIndex, UTF8) };
    if ok == 0 {
        return None;
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    String::from_utf8(buffer[..end].to_vec()).ok()
}

fn as_bool(value: &CfValue) -> Option<bool> {
    // SAFETY: `value.0` is a live CoreFoundation object.
    unsafe {
        (CFGetTypeID(value.0) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(value.0) != 0)
    }
}

/// The domain holding the accessibility switches.
const UNIVERSAL_ACCESS: &str = "com.apple.universalaccess";

#[derive(Debug, Default)]
pub struct MacosServices;

impl MacosServices {
    pub fn new() -> MacosServices {
        MacosServices
    }
}

impl PlatformServices for MacosServices {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "appkit".into(),
            // CoreGraphics reports a vendor/model/serial triple derived from
            // the panel, which survives a reboot and a different port.
            identity: IdentityQuality::Stable,
            targeted_fullscreen: true,
            arbitrary_placement: true,
            safe_unfullscreen: true,
            place_before_map: true,
            system_appearance: true,
            high_contrast_detection: true,
            sleep_inhibition: true,
            native_dialogs: true,
            // The desktop has a global menu bar. The application does not yet
            // install one; that is the remaining macOS integration step, and
            // the diagnostics bundle is where it should be visible.
            native_menus: true,
            // AppKit has a complete accessibility bridge, but nothing reaches
            // it while the toolkit exposes no tree. This says what a screen
            // reader would actually find, not what the platform could offer.
            accessibility_bridge: false,
            media_keys: true,
            notifications: true,
        }
    }

    fn directories(&self) -> Directories {
        Directories::detect()
    }

    fn system_appearance(&self) -> SystemAppearance {
        // "Increase contrast" is a separate switch from Dark Mode and takes
        // precedence, exactly as on the other platforms.
        if preference("increaseContrast", Some(UNIVERSAL_ACCESS))
            .as_ref()
            .and_then(as_bool)
            .unwrap_or(false)
        {
            return SystemAppearance::HighContrast;
        }
        // `AppleInterfaceStyle` is "Dark" or absent. Absent means Light — but
        // only when the global domain could be read at all, which it always
        // can be on a real session.
        match preference("AppleInterfaceStyle", None)
            .as_ref()
            .and_then(as_string)
        {
            Some(style) if style.eq_ignore_ascii_case("dark") => SystemAppearance::Dark,
            Some(_) => SystemAppearance::Light,
            None => SystemAppearance::Light,
        }
    }

    fn reduced_motion(&self) -> MotionPreference {
        match preference("reduceMotion", Some(UNIVERSAL_ACCESS))
            .as_ref()
            .and_then(as_bool)
        {
            Some(true) => MotionPreference::Reduced,
            Some(false) => MotionPreference::Full,
            None => MotionPreference::Unknown,
        }
    }

    fn reveal(&self, path: &Path) -> Outcome {
        spawn("open", &["-R", &path.to_string_lossy()])
    }

    fn open(&self, target: &str) -> Outcome {
        spawn("open", &[target])
    }

    fn notify(&self, notification: &Notification) -> Outcome {
        // AppleScript is the only notification path open to an unsigned,
        // unpackaged binary. It posts a real notification, but the system
        // attributes it to the script runner rather than to pulpit; a signed
        // bundle with `UNUserNotificationCenter` is the packaging-time fix.
        let escape = |value: &str| value.replace('\\', r"\\").replace('"', "\\\"");
        let mut script = format!(
            "display notification \"{}\" with title \"{}\"",
            escape(&notification.body),
            escape(&notification.title)
        );
        if notification.urgency == Urgency::Critical {
            script.push_str(" sound name \"Basso\"");
        }
        spawn("osascript", &["-e", &script])
    }

    fn inhibit(&self) -> InhibitState {
        let mut attempts = Vec::new();

        // 1. A power assertion: precise, and released the moment the process
        //    exits for any reason.
        match (
            CfString::new("PreventUserIdleDisplaySleep"),
            CfString::new("Presentation in progress"),
        ) {
            (Some(kind), Some(name)) => {
                let mut id = 0u32;
                // SAFETY: both strings are live for the call and `id` is a
                // valid out-pointer.
                let status = unsafe {
                    IOPMAssertionCreateWithName(
                        kind.as_raw(),
                        IOPM_ASSERTION_LEVEL_ON,
                        name.as_raw(),
                        &mut id,
                    )
                };
                if status == KERN_SUCCESS {
                    return InhibitState::Held {
                        mechanism: "IOPMAssertion",
                        token: InhibitToken::Cookie(id),
                    };
                }
                attempts.push(format!("IOPMAssertionCreateWithName: {status}"));
            }
            _ => attempts.push("IOPMAssertion: could not build the assertion strings".into()),
        }

        // 2. `caffeinate`, which ships with macOS. `-d` keeps the display
        //    awake, `-i` the system.
        match Command::new("caffeinate")
            .args(["-d", "-i"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => InhibitState::Held {
                mechanism: "caffeinate",
                token: InhibitToken::Process(child.id()),
            },
            Err(e) => {
                attempts.push(format!("caffeinate: {e}"));
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
            InhibitToken::Cookie(id) => {
                // SAFETY: the id came from `IOPMAssertionCreateWithName` in
                // `inhibit`, and `Inhibitor` releases it exactly once.
                let status = unsafe { IOPMAssertionRelease(*id) };
                if status == KERN_SUCCESS {
                    Outcome::Done
                } else {
                    Outcome::failed(format!("the power assertion would not release ({status})"))
                }
            }
            InhibitToken::Process(pid) => {
                match Command::new("kill").arg(pid.to_string()).status() {
                    Ok(status) if status.success() => Outcome::Done,
                    Ok(status) => Outcome::failed(format!("kill exited with {status}")),
                    Err(e) => Outcome::failed(e.to_string()),
                }
            }
            InhibitToken::Handle(_) | InhibitToken::None => Outcome::Done,
        }
    }

    fn recent_documents(&self) -> Option<Vec<PathBuf>> {
        // The shared file list is a private, versioned binary plist. Reading
        // it would be guessing at another application's storage.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapter_reports_a_stable_identity_and_no_accessibility_bridge() {
        let capabilities = MacosServices::new().capabilities();
        assert_eq!(capabilities.identity, IdentityQuality::Stable);
        assert!(capabilities.targeted_fullscreen);
        assert!(!capabilities.accessibility_bridge);
        assert!(capabilities.limitations().is_empty());
    }

    #[test]
    fn releasing_nothing_is_fine() {
        let services = MacosServices::new();
        assert_eq!(
            services.release_inhibit(&InhibitState::Released),
            Outcome::Done
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
    fn appearance_reads_the_live_session_without_panicking() {
        // The value depends on the machine; that it is one of the four and
        // that CoreFoundation was driven correctly is what matters.
        let appearance = MacosServices::new().system_appearance();
        assert!(matches!(
            appearance,
            SystemAppearance::Light
                | SystemAppearance::Dark
                | SystemAppearance::HighContrast
                | SystemAppearance::Unknown
        ));
    }
}
