//! Runtime adapters and their discovery (`docs-src/internals.typ`).

#[cfg(feature = "chromium-runtime")]
pub mod chromium;

pub mod scale;

use std::path::{Path, PathBuf};

use pulpit_core::overlay::ContentKind;

use crate::capability::{
    Availability, ContentCapabilities, InputCapabilities, Limitation, PlaybackCapabilities,
    RenderingCapabilities, RuntimeProbe, SecurityCapabilities,
};
use crate::protocol::{PixelFormat, RuntimeId};
use crate::selection::{is_chromium_family, CHROMIUM_EXECUTABLES};

/// Look for `name` on `PATH`, without recursively scanning anything.
///
/// Discovery deliberately never walks disks and never executes a candidate it
/// has not identified first (`docs-src/internals.typ`).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    // Windows names its executables with an extension and does not mark them
    // executable, so a bare `chrome` finds nothing there however it is spelled.
    let names: Vec<String> = if cfg!(target_os = "windows") && !name.ends_with(".exe") {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&path)
        .flat_map(|directory| {
            names
                .iter()
                .map(move |name| directory.join(name))
                .collect::<Vec<_>>()
        })
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The standard installation locations checked before `PATH`, so a browser
/// installed outside the shell's environment is still found.
#[cfg(target_os = "linux")]
const CHROMIUM_INSTALL_PATHS: &[&str] = &[
    "/usr/bin/google-chrome-stable",
    "/usr/bin/google-chrome",
    "/opt/google/chrome/chrome",
    "/usr/bin/microsoft-edge-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/snap/bin/chromium",
    "/usr/bin/brave-browser",
];

#[cfg(target_os = "macos")]
const CHROMIUM_INSTALL_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
];

// Windows has its own table below, keyed by environment variable, so the
// empty fallback is for the platforms with neither.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const CHROMIUM_INSTALL_PATHS: &[&str] = &[];

/// Where a Chromium-family browser is installed on Windows, as
/// `(environment variable, path beneath it)`.
///
/// Windows has no `/usr/bin`: everything hangs off a root named by an
/// environment variable that differs between machines and architectures, so
/// these cannot be plain absolute strings like the Unix tables above.
///
/// Edge earns its place even though Chrome leads elsewhere: it is
/// preinstalled on every supported version of Windows and is Chromium-family,
/// so finding it means the recommended dependency of §2.3 is satisfied on
/// every machine without the user installing anything. Chrome still comes
/// first where both exist, because Chrome Stable is the implementation
/// covered by CI.
///
/// Compiled everywhere and used only on Windows, so that the table itself is
/// an ordinary unit test on any machine.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const WINDOWS_CHROMIUM_INSTALL_PATHS: &[(&str, &str)] = &[
    ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
    ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
    // A per-user Chrome install, which is what an ordinary account without
    // administrator rights gets.
    ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
    (
        "ProgramFiles(x86)",
        r"Microsoft\Edge\Application\msedge.exe",
    ),
    ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
    (
        "ProgramFiles",
        r"BraveSoftware\Brave-Browser\Application\brave.exe",
    ),
    (
        "LOCALAPPDATA",
        r"BraveSoftware\Brave-Browser\Application\brave.exe",
    ),
    ("LOCALAPPDATA", r"Chromium\Application\chrome.exe"),
];

/// The installation locations to check, resolved against this machine.
fn chromium_install_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        WINDOWS_CHROMIUM_INSTALL_PATHS
            .iter()
            .filter_map(|(root, suffix)| {
                std::env::var_os(root).map(|root| PathBuf::from(root).join(suffix))
            })
            .collect()
    }
    #[cfg(not(target_os = "windows"))]
    {
        CHROMIUM_INSTALL_PATHS.iter().map(PathBuf::from).collect()
    }
}

/// Find an installed Chromium-family browser.
///
/// An explicitly configured path leads, but being configured is not licence
/// to execute an arbitrary file: it still has to look like a Chromium-family
/// browser, and the worker still version-probes it before loading content.
pub fn discover_chromium(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        if is_executable(path) && is_chromium_family(&path.to_string_lossy()) {
            return Some(path.to_path_buf());
        }
        tracing::warn!(
            path = %path.display(),
            "the configured browser is not a usable Chromium-family executable"
        );
    }
    chromium_install_paths()
        .into_iter()
        .find(|candidate| is_executable(candidate))
        .or_else(|| CHROMIUM_EXECUTABLES.iter().find_map(|name| which(name)))
}

/// Probe the external Chromium-family HTML runtime.
pub fn probe_external_chromium(configured: Option<&Path>) -> RuntimeProbe {
    if !cfg!(feature = "chromium-runtime") {
        return RuntimeProbe::unavailable(RuntimeId::ExternalChromium, Availability::NotBuilt);
    }
    let Some(executable) = discover_chromium(configured) else {
        return RuntimeProbe::unavailable(
            RuntimeId::ExternalChromium,
            Availability::NotInstalled {
                detail: "no Google Chrome, Edge, Chromium or Brave installation was found".into(),
            },
        );
    };
    let version = chromium::browser_version(&executable);
    if version.is_none() {
        return RuntimeProbe::unavailable(
            RuntimeId::ExternalChromium,
            Availability::Incompatible {
                detail: format!(
                    "{} did not report a usable browser version",
                    executable.display()
                ),
            },
        );
    }
    RuntimeProbe {
        id: RuntimeId::ExternalChromium,
        availability: Availability::Available,
        version,
        executable: Some(executable),
        content: ContentCapabilities {
            // A browser decodes every animated image and video format a deck
            // is likely to carry. Claiming only HTML was leaving that on the
            // table and forcing pulpit to carry decoders of its own.
            kinds: vec![
                ContentKind::Web,
                ContentKind::Video,
                ContentKind::AnimatedImage,
            ],
            mime: vec![
                "text/html".into(),
                "video/mp4".into(),
                "video/webm".into(),
                "image/gif".into(),
                "image/apng".into(),
                "image/webp".into(),
            ],
            formats: vec![PixelFormat::Rgba8Straight],
            max_width: 8192,
            max_height: 8192,
            continuous_frames: true,
        },
        input: InputCapabilities {
            pointer: true,
            wheel: true,
            keyboard: true,
            text: true,
            touch: false,
            focus: true,
        },
        playback: PlaybackCapabilities {
            seek: true,
            accurate_seek: true,
            pause: true,
            looping: true,
            volume: true,
            audio: true,
        },
        rendering: RenderingCapabilities {
            // Headless Chrome owns no window, which is precisely why it works
            // on native Wayland where a child webview cannot be embedded.
            wayland_independent: true,
            webgl: true,
            raw_frames: false,
            device_scale: true,
        },
        security: SecurityCapabilities {
            javascript: true,
            enforces_network_policy: true,
            ephemeral_storage: true,
            private_profile: true,
            sandbox: true,
        },
        limitations: vec![Limitation::CompressedFrames {
            codec: "JPEG screencast".into(),
        }],
    }
}

/// Probe for a loadable libmpv.
///
/// Loading the library *is* the probe: mpv keeps its client ABI stable, so a
/// library that loads and resolves `mpv_create` will play.
pub fn probe_libmpv() -> RuntimeProbe {
    use pulpit_core::overlay::ContentKind;
    if let Err(error) = crate::worker::mpv::Api::load() {
        return RuntimeProbe::unavailable(
            RuntimeId::LibMpv,
            Availability::NotInstalled {
                detail: error.message,
            },
        );
    }
    RuntimeProbe {
        content: ContentCapabilities {
            kinds: vec![ContentKind::Video, ContentKind::AnimatedImage],
            mime: vec![
                "video/mp4".into(),
                "video/webm".into(),
                "image/gif".into(),
                "image/apng".into(),
                "image/webp".into(),
            ],
            formats: vec![PixelFormat::Rgba8Straight],
            max_width: 8192,
            max_height: 8192,
            continuous_frames: true,
        },
        input: InputCapabilities {
            pointer: true,
            ..Default::default()
        },
        playback: PlaybackCapabilities {
            seek: true,
            accurate_seek: true,
            pause: true,
            looping: true,
            volume: true,
            audio: true,
        },
        rendering: RenderingCapabilities {
            wayland_independent: true,
            webgl: false,
            raw_frames: true,
            device_scale: false,
        },
        security: SecurityCapabilities {
            javascript: false,
            enforces_network_policy: true,
            ephemeral_storage: true,
            private_profile: true,
            sandbox: false,
        },
        limitations: Vec::new(),
        ..RuntimeProbe::unavailable(RuntimeId::LibMpv, Availability::Available)
    }
}

/// Probe the system-webview snapshot runtimes.
///
/// These stay out of automatic selection until they pass the continuous
/// animation, input, scaling and hidden-window qualification gates.
pub fn probe_system_webview(id: RuntimeId) -> RuntimeProbe {
    RuntimeProbe::unavailable(
        id,
        Availability::NotQualified {
            detail: "has not passed the continuous-animation and input qualification suite".into(),
        },
    )
}

/// Probe every runtime this build knows about.
pub fn probe_all(configured_browser: Option<&Path>) -> Vec<RuntimeProbe> {
    vec![
        probe_external_chromium(configured_browser),
        probe_libmpv(),
        probe_system_webview(RuntimeId::WebKitGtk),
        probe_system_webview(RuntimeId::WebView2),
        probe_system_webview(RuntimeId::WkWebView),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_browser_runtime_claims_every_content_kind_a_deck_can_carry() {
        // The one runtime has to cover all three kinds, because there is no
        // longer a second one to fall back to for images or video.
        let probe = probe_external_chromium(None);
        if probe.is_available() {
            for kind in [
                ContentKind::Web,
                ContentKind::Video,
                ContentKind::AnimatedImage,
            ] {
                assert!(probe.content.supports(kind), "{kind:?} is not covered");
            }
        }
    }

    #[test]
    fn an_unimplemented_runtime_is_reported_as_unavailable_not_assumed_working() {
        let probe = probe_system_webview(RuntimeId::WebKitGtk);
        assert!(!probe.is_available());
        assert!(
            !probe.content.supports(ContentKind::Web),
            "an unavailable runtime must claim no capability at all"
        );
    }

    #[test]
    fn system_webviews_stay_out_of_automatic_selection_until_qualified() {
        for id in [
            RuntimeId::WebKitGtk,
            RuntimeId::WebView2,
            RuntimeId::WkWebView,
        ] {
            let probe = probe_system_webview(id);
            assert!(matches!(
                probe.availability,
                Availability::NotQualified { .. }
            ));
        }
    }

    #[test]
    fn probing_covers_every_runtime_the_settings_vocabulary_names() {
        let probes = probe_all(None);
        for runtime in RuntimeId::ALL {
            assert!(
                probes.iter().any(|probe| probe.id == runtime),
                "{runtime} was not probed"
            );
        }
    }

    #[test]
    fn a_configured_browser_that_is_not_chromium_family_is_refused() {
        // Firefox must never be launched through the CDP adapter, even when
        // the user points the setting straight at it.
        assert_eq!(
            discover_chromium(Some(Path::new("/usr/bin/firefox"))).as_deref(),
            discover_chromium(None).as_deref(),
            "a Firefox path is ignored, falling back to ordinary discovery"
        );
    }

    #[test]
    fn a_configured_path_that_does_not_exist_is_ignored() {
        let missing = Path::new("/nonexistent/google-chrome");
        assert_eq!(
            discover_chromium(Some(missing)).as_deref(),
            discover_chromium(None).as_deref()
        );
    }

    /// Windows discovery was empty: `CHROMIUM_INSTALL_PATHS` had no entries
    /// and the `PATH` fallback searched Unix names, so the Edge preinstalled
    /// on every machine was never found and media overlays fell back to
    /// posters on a stock system.
    #[test]
    fn the_windows_table_names_edge_and_chrome_under_a_variable_root() {
        assert!(
            !WINDOWS_CHROMIUM_INSTALL_PATHS.is_empty(),
            "an empty table is the bug this replaces"
        );
        for (root, suffix) in WINDOWS_CHROMIUM_INSTALL_PATHS {
            assert!(
                !root.is_empty(),
                "every entry hangs off an environment variable, never an \
                 absolute path: {suffix}"
            );
            assert!(
                suffix.ends_with(".exe"),
                "{suffix} must name the executable itself"
            );
            assert!(
                !suffix.starts_with('\\'),
                "{suffix} is joined onto a root and must be relative"
            );
        }
        let suffixes: Vec<&str> = WINDOWS_CHROMIUM_INSTALL_PATHS
            .iter()
            .map(|(_, suffix)| *suffix)
            .collect();
        assert!(
            suffixes.iter().any(|s| s.ends_with("msedge.exe")),
            "Edge is the one browser present on a stock Windows machine"
        );
        assert!(
            suffixes.iter().any(|s| s.ends_with("chrome.exe")),
            "Chrome Stable is the implementation CI covers"
        );
    }

    /// Every path the table can produce must be recognised by the family
    /// check, or discovery finds a browser and then refuses to launch it.
    #[test]
    fn everything_the_windows_table_finds_is_recognised_as_chromium_family() {
        for (_, suffix) in WINDOWS_CHROMIUM_INSTALL_PATHS {
            let full = format!(r"C:\Program Files\{suffix}");
            assert!(
                crate::selection::is_chromium_family(&full),
                "{full} was found but would not be launched"
            );
        }
    }

    #[test]
    fn which_finds_a_program_that_exists_and_not_one_that_does_not() {
        assert!(which("pulpit-definitely-not-a-program").is_none());
        // `sh` is present on every platform this is tested on.
        #[cfg(unix)]
        assert!(which("sh").is_some());
    }
}
