//! The Windows desktop adapter.
//!
//! Appearance comes from the Personalize registry values and the accessibility
//! system parameters; inhibition from the power-request API; opening and
//! revealing from the shell. Anything Windows cannot do without a packaged
//! app identity — a global menu — is reported as unsupported rather than
//! faked.
//!
//! As in the Linux adapter, the FFI is declared here so the handful of structs
//! whose layout matters sit next to the code that fills them in.

use std::ffi::c_void;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::platform::appearance::{MotionPreference, SystemAppearance};
use crate::platform::capabilities::{Capabilities, IdentityQuality};
use crate::platform::inhibit::{InhibitState, InhibitToken};
use crate::platform::paths::Directories;
use crate::platform::services::PlatformServices;
use crate::platform::Outcome;

type Bool = i32;
type Handle = *mut c_void;
type Hkey = *mut c_void;

const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as usize as Hkey;
/// `RRF_RT_REG_DWORD`.
const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
const ERROR_SUCCESS: i32 = 0;

const SPI_GET_HIGH_CONTRAST: u32 = 0x0042;
const SPI_GET_CLIENT_AREA_ANIMATION: u32 = 0x1042;
const HCF_HIGH_CONTRAST_ON: u32 = 0x0000_0001;

/// `POWER_REQUEST_CONTEXT_SIMPLE_STRING`.
const POWER_REQUEST_CONTEXT_SIMPLE_STRING: u32 = 0x0000_0001;
const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;
const POWER_REQUEST_DISPLAY_REQUIRED: u32 = 0;
const POWER_REQUEST_SYSTEM_REQUIRED: u32 = 1;

const SW_SHOWNORMAL: i32 = 1;

#[repr(C)]
struct HighContrastW {
    cb_size: u32,
    dw_flags: u32,
    lpsz_default_scheme: *mut u16,
}

#[repr(C)]
struct ReasonContext {
    version: u32,
    flags: u32,
    /// The simple-string arm of the union. Only ever used with
    /// `POWER_REQUEST_CONTEXT_SIMPLE_STRING`, which is why the detailed arm is
    /// not modelled.
    simple_reason_string: *mut u16,
}

#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(
        key: Hkey,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        data_size: *mut u32,
    ) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void, ini: u32) -> Bool;
}

#[link(name = "kernel32")]
extern "system" {
    fn PowerCreateRequest(context: *mut ReasonContext) -> Handle;
    fn PowerSetRequest(request: Handle, kind: u32) -> Bool;
    fn PowerClearRequest(request: Handle, kind: u32) -> Bool;
    fn CloseHandle(object: Handle) -> Bool;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        hwnd: *mut c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> *mut c_void;
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Debug, Default)]
pub struct WindowsServices;

impl WindowsServices {
    pub fn new() -> WindowsServices {
        WindowsServices
    }
}

/// Read a `DWORD` from `HKEY_CURRENT_USER`.
fn registry_dword(subkey: &str, value: &str) -> Option<u32> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: both names are NUL-terminated and live across the call; `data`
    // and `size` are valid out-pointers sized to match `RRF_RT_REG_DWORD`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

fn high_contrast_on() -> bool {
    let mut info = HighContrastW {
        cb_size: std::mem::size_of::<HighContrastW>() as u32,
        dw_flags: 0,
        lpsz_default_scheme: std::ptr::null_mut(),
    };
    // SAFETY: `cb_size` describes the struct, as the API requires. The scheme
    // name is left null: only the flags are read, so Windows has nothing to
    // write a string into and does not try.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GET_HIGH_CONTRAST,
            std::mem::size_of::<HighContrastW>() as u32,
            &mut info as *mut HighContrastW as *mut c_void,
            0,
        )
    };
    ok != 0 && info.dw_flags & HCF_HIGH_CONTRAST_ON != 0
}

impl PlatformServices for WindowsServices {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "win32".into(),
            // `QueryDisplayConfig` carries the monitor device path, which is
            // stable across a reboot and a re-plug.
            identity: IdentityQuality::Stable,
            arbitrary_placement: true,
            safe_unfullscreen: true,
            place_before_map: true,
            system_appearance: true,
            high_contrast_detection: true,
            sleep_inhibition: true,
            native_dialogs: true,
            // A menu bar belongs to the window on Windows, not to the desktop.
            native_menus: false,
            // UI Automation exists, but nothing reaches it while the toolkit
            // exposes no accessibility tree. Claiming otherwise would tell a
            // screen-reader user their software works when it does not.
            accessibility_bridge: false,
            media_keys: true,
            // Toasts need a registered AppUserModelID, which is a packaging
            // decision rather than a runtime one.
            image_clipboard: true,
            // Windows has no "show the print dialog for this file" call:
            // the shell verb takes no options and shows nothing, and the
            // dialog `PrintDlgEx` puts up hands back a device context for
            // the application to draw every page onto. Drawing pages is the
            // job `crate::printing` says pulpit does not take on, so this
            // platform has no system print dialog until it does.
            system_print_dialog: false,
            // The shell's `print` verb hands a PDF to whatever is registered
            // for it. Every Windows install has *something*; whether that
            // something can print is between it and the machine, and the
            // outcome of the call is where that is found out.
            printing: true,
            // …and the verb takes nothing else: no range, no copies, no
            // queue. See `print` below.
            print_options: false,
            // Filled in by the application once the speech catalog has been
            // probed. Whether a voice is installed on disk is not a question
            // a window backend can answer.
            speech: crate::platform::capabilities::Speech::default(),
        }
    }

    fn directories(&self) -> Directories {
        Directories::detect()
    }

    fn system_appearance(&self) -> SystemAppearance {
        if high_contrast_on() {
            return SystemAppearance::HighContrast;
        }
        // `AppsUseLightTheme` is the application-facing value; `SystemUsesLightTheme`
        // governs the taskbar and is deliberately not read.
        match registry_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "AppsUseLightTheme",
        ) {
            Some(0) => SystemAppearance::Dark,
            Some(_) => SystemAppearance::Light,
            None => SystemAppearance::Unknown,
        }
    }

    fn reduced_motion(&self) -> MotionPreference {
        let mut enabled: Bool = 0;
        // SAFETY: the out-parameter is a `BOOL`, which is what this action
        // writes.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GET_CLIENT_AREA_ANIMATION,
                0,
                &mut enabled as *mut Bool as *mut c_void,
                0,
            )
        };
        if ok == 0 {
            MotionPreference::Unknown
        } else if enabled != 0 {
            MotionPreference::Full
        } else {
            MotionPreference::Reduced
        }
    }

    fn reveal(&self, path: &Path) -> Outcome {
        // `/select,` needs the file itself, and Explorer wants it quoted when
        // it contains spaces.
        match Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            // Explorer exits non-zero even on success, so the spawn is the
            // only thing worth checking.
            Ok(_) => Outcome::Done,
            Err(e) => Outcome::failed(e.to_string()),
        }
    }

    fn open(&self, target: &str) -> Outcome {
        let operation = wide("open");
        let file = wide(target);
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Legacy API: values above 32 mean success, and the error codes below
        // it are the ones worth distinguishing.
        match result as usize {
            code if code > 32 => Outcome::Done,
            // SE_ERR_NOASSOC / SE_ERR_ASSOCINCOMPLETE.
            27 | 31 => Outcome::Unsupported {
                what: "Opening this kind of file",
            },
            2 | 3 => Outcome::failed("that file or folder no longer exists"),
            5 => Outcome::refused("Windows denied access to that file"),
            code => Outcome::failed(format!("the shell refused to open it (code {code})")),
        }
    }

    /// One clipboard library serves all three desktops; what differs is
    /// whether the session offers a clipboard at all, which the outcome says.
    fn copy_image(&self, image: &crate::platform::clipboard::ClipboardImage) -> Outcome {
        crate::platform::clipboard::copy_image(image)
    }

    /// Print through the shell's `print` verb.
    ///
    /// This is the hand-off in its plainest form: the file goes to whatever
    /// application is registered for PDFs, which prints it on the default
    /// printer. That is the whole of what the verb does — it takes no page
    /// range, no copy count and no queue — which is why this adapter reports
    /// `print_options: false` and why a job that names any of them is
    /// refused here rather than silently printed whole. Forty pages when
    /// four were asked for is not a partial success.
    ///
    /// The way out of that is `PrintDlgEx` and a GDI device context, or
    /// writing the wanted pages into the spooled copy before handing it over.
    /// Neither is a thing to do on the way past.
    fn print(&self, job: &crate::platform::services::PrintJob) -> Outcome {
        if !job.pages.is_empty() {
            return Outcome::Unsupported {
                what: "Printing a range of pages on Windows",
            };
        }
        if job.copies > 1 {
            return Outcome::Unsupported {
                what: "Printing more than one copy on Windows",
            };
        }
        if job.destination.is_some() {
            return Outcome::Unsupported {
                what: "Choosing a printer on Windows",
            };
        }
        if !job.file.is_file() {
            return Outcome::failed("there is nothing at that path to print");
        }
        let operation = wide("print");
        let file = wide(&job.file.to_string_lossy());
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        match result as usize {
            code if code > 32 => Outcome::Done,
            // SE_ERR_NOASSOC / SE_ERR_ASSOCINCOMPLETE: nothing on this
            // machine has registered itself as able to print a PDF.
            27 | 31 => Outcome::Unsupported {
                what: "Printing PDFs on this machine",
            },
            2 | 3 => Outcome::failed("that file no longer exists"),
            5 => Outcome::refused("Windows denied access to that file"),
            code => Outcome::failed(format!("the shell refused to print it (code {code})")),
        }
    }

    fn inhibit(&self) -> InhibitState {
        let mut reason = wide("Presentation in progress");
        let mut context = ReasonContext {
            version: POWER_REQUEST_CONTEXT_VERSION,
            flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            simple_reason_string: reason.as_mut_ptr(),
        };
        // SAFETY: `reason` outlives the call, which copies the string.
        let request = unsafe { PowerCreateRequest(&mut context) };
        if request.is_null() {
            return InhibitState::Unavailable {
                reason: "Windows refused a power request".into(),
                attempts: vec!["PowerCreateRequest: null".into()],
            };
        }

        // Display *and* system: a projector that blanks is as bad as a machine
        // that suspends.
        // SAFETY: `request` is a live power-request handle.
        let display = unsafe { PowerSetRequest(request, POWER_REQUEST_DISPLAY_REQUIRED) };
        let system = unsafe { PowerSetRequest(request, POWER_REQUEST_SYSTEM_REQUIRED) };
        if display == 0 && system == 0 {
            // SAFETY: closing a handle we created and never shared.
            unsafe { CloseHandle(request) };
            return InhibitState::Unavailable {
                reason: "Windows accepted no power request".into(),
                attempts: vec!["PowerSetRequest: refused for display and system".into()],
            };
        }

        // `InhibitToken` carries no native handle type, and it should not: the
        // pointer is stored as text and parsed back at release, so nothing in
        // the application's state is a live OS handle.
        InhibitState::Held {
            mechanism: "PowerSetRequest",
            token: InhibitToken::Handle(format!("{}", request as usize)),
        }
    }

    fn release_inhibit(&self, state: &InhibitState) -> Outcome {
        let InhibitState::Held { token, .. } = state else {
            return Outcome::Done;
        };
        let InhibitToken::Handle(handle) = token else {
            return Outcome::Done;
        };
        let Ok(value) = handle.parse::<usize>() else {
            return Outcome::failed("the power request handle was not readable");
        };
        let request = value as Handle;
        if request.is_null() {
            return Outcome::Done;
        }
        // SAFETY: the handle came from `PowerCreateRequest` in `inhibit` and
        // is released exactly once — `Inhibitor` keeps acquire and release
        // balanced.
        unsafe {
            PowerClearRequest(request, POWER_REQUEST_DISPLAY_REQUIRED);
            PowerClearRequest(request, POWER_REQUEST_SYSTEM_REQUIRED);
            if CloseHandle(request) == 0 {
                return Outcome::failed("the power request handle would not close");
            }
        }
        Outcome::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_nul_terminated() {
        let encoded = wide("ok");
        assert_eq!(encoded, vec![b'o' as u16, b'k' as u16, 0]);
    }

    #[test]
    fn the_power_reason_context_matches_the_documented_layout() {
        // Two `DWORD`s then a pointer-aligned union.
        assert_eq!(
            std::mem::size_of::<ReasonContext>(),
            8 + std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn releasing_nothing_is_fine() {
        let services = WindowsServices::new();
        assert_eq!(
            services.release_inhibit(&InhibitState::Released),
            Outcome::Done
        );
        // A held state with no handle is also inert rather than a panic.
        assert_eq!(
            services.release_inhibit(&InhibitState::Held {
                mechanism: "test",
                token: InhibitToken::None,
            }),
            Outcome::Done
        );
    }

    #[test]
    fn the_adapter_does_not_claim_an_accessibility_bridge() {
        // Until the toolkit exposes a tree, nothing reaches UI Automation.
        assert!(!WindowsServices::new().capabilities().accessibility_bridge);
    }
}
