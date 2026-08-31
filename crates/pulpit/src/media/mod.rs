//! Iced integration for media overlays (`docs-src/internals.typ`).
//!
//! This module maps overlay rectangles into slide widgets, hit-tests them,
//! routes input and holds the frames the view draws. It contains no runtime
//! discovery, no Chrome DevTools Protocol and no PDF dictionary logic: those
//! live in `pulpit-media` and `pulpit-render` respectively.

// The overlay model is complete and tested; the application happens to use a
// subset of it. `pointer_within` in particular is the panel-space twin of the
// page-space routing `App` performs, and the test that pins the two together
// is what keeps the specification's "one geometry function" rule honest —
// deleting it to satisfy a lint would throw away that guarantee.

pub mod coordinator;
pub mod gesture;
pub mod input;
pub mod overlay;

#[allow(unused_imports)]
pub use coordinator::{MediaCoordinator, Need, OverlayFrame, TransportCommand, TransportTarget};
pub use gesture::MediaGesture;
#[allow(unused_imports)]
pub use input::{InputRouter, Routed};
#[allow(unused_imports)]
pub use overlay::{place, pointer_within, viewport_for, PageBox};

use std::path::PathBuf;

use pulpit_media::{MediaConfig, RuntimeId, WorkerCommand};

/// How to start the worker for one runtime.
///
/// Every shipped runtime is a role of *this* executable, re-executed with a
/// flag — so it cannot be missing from a build or a package, which is a
/// mistake this made once already. A runtime with no role is one that would
/// need a worker pulpit does not ship; selection never reaches those, and
/// naming the missing binary makes the launch fail and fall through rather
/// than start something else.
pub fn worker_command(runtime: RuntimeId) -> WorkerCommand {
    match pulpit_media::worker::role_flag(runtime) {
        Some(role) => WorkerCommand::CurrentExe {
            arg: format!("--media-worker={role}"),
        },
        None => WorkerCommand::Explicit {
            program: PathBuf::from(runtime.worker_binary()),
            args: Vec::new(),
        },
    }
}

/// Build the supervisor configuration from application settings.
pub fn config_from_settings(
    image: Option<&str>,
    video: Option<&str>,
    web: Option<&str>,
    browser_path: Option<PathBuf>,
) -> MediaConfig {
    use pulpit_media::RuntimePolicy;
    let parse = |value: Option<&str>| {
        value
            .and_then(RuntimePolicy::parse)
            .unwrap_or(RuntimePolicy::Auto)
    };
    MediaConfig {
        image_runtime: parse(image),
        video_runtime: parse(video),
        web_runtime: parse(web),
        browser_path: browser_path.filter(|path| !path.as_os_str().is_empty()),
        ..MediaConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtimes_that_ship_with_pulpit_are_roles_of_this_executable() {
        // Not a stylistic preference: as separate binaries these were never
        // built by `cargo run`, so media silently never played. A role cannot
        // be missing from a build or a package.
        for runtime in [RuntimeId::ExternalChromium] {
            match worker_command(runtime) {
                WorkerCommand::CurrentExe { arg } => {
                    assert!(
                        arg.starts_with("--media-worker="),
                        "{runtime} spawns with {arg}"
                    );
                }
                other => panic!("{runtime} should be a role, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_runtime_pulpit_ships_no_worker_for_is_never_run_as_this_executable() {
        // Re-executing ourselves with an unknown role flag would start the
        // *application* again, on stage. Naming a program we do not ship
        // fails the launch instead, and selection falls through.
        for runtime in [
            RuntimeId::WebKitGtk,
            RuntimeId::WebView2,
            RuntimeId::WkWebView,
        ] {
            let WorkerCommand::Explicit { program, .. } = worker_command(runtime) else {
                panic!("{runtime} has no worker role and must not re-execute this binary");
            };
            assert!(program.to_string_lossy().starts_with("pulpit-"));
        }
    }

    #[test]
    fn unreadable_settings_fall_back_to_automatic_selection() {
        use pulpit_media::RuntimePolicy;
        let config = config_from_settings(Some("nonesuch"), None, Some(""), None);
        assert_eq!(config.image_runtime, RuntimePolicy::Auto);
        assert_eq!(config.video_runtime, RuntimePolicy::Auto);
        assert_eq!(config.web_runtime, RuntimePolicy::Auto);
    }

    #[test]
    fn settings_choose_a_runtime_policy() {
        use pulpit_media::RuntimePolicy;
        let config = config_from_settings(
            Some("webkitgtk"),
            Some("!external-chromium"),
            Some("external-chromium"),
            Some(PathBuf::from("/usr/bin/google-chrome")),
        );
        assert_eq!(
            config.image_runtime,
            RuntimePolicy::Prefer(RuntimeId::WebKitGtk)
        );
        assert_eq!(
            config.video_runtime,
            RuntimePolicy::Require(RuntimeId::ExternalChromium)
        );
        assert_eq!(
            config.browser_path,
            Some(PathBuf::from("/usr/bin/google-chrome"))
        );
    }

    #[test]
    fn an_empty_browser_path_is_not_treated_as_a_configured_one() {
        let config = config_from_settings(None, None, None, Some(PathBuf::new()));
        assert_eq!(config.browser_path, None);
    }
}
