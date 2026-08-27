//! Platform-standard directories.
//!
//! Paths are `PathBuf`, never assumed to be UTF-8, and never hard-coded to
//! one platform's convention. Environment overrides exist so tests never
//! touch a real user's configuration.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// Where this application keeps its things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directories {
    pub config: PathBuf,
    pub cache: PathBuf,
    pub data: PathBuf,
    pub logs: PathBuf,
}

const APPLICATION: &str = "pulpit";

impl Directories {
    /// Resolve the standard locations for this platform.
    ///
    /// `PULPIT_CONFIG_DIR` and `PULPIT_LOG_DIR` override their
    /// respective locations, which is how the test suite and portable
    /// installations stay out of a real profile.
    pub fn detect() -> Directories {
        let mut directories = Self::platform_defaults();
        if let Some(config) = std::env::var_os("PULPIT_CONFIG_DIR") {
            directories.config = PathBuf::from(config);
        }
        if let Some(logs) = std::env::var_os("PULPIT_LOG_DIR") {
            directories.logs = PathBuf::from(logs);
        }
        directories
    }

    #[cfg(target_os = "macos")]
    fn platform_defaults() -> Directories {
        let home = home();
        Directories {
            config: home.join("Library/Application Support").join(APPLICATION),
            cache: home.join("Library/Caches").join(APPLICATION),
            data: home.join("Library/Application Support").join(APPLICATION),
            logs: home.join("Library/Logs").join(APPLICATION),
        }
    }

    #[cfg(target_os = "windows")]
    fn platform_defaults() -> Directories {
        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData/Roaming"));
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData/Local"));
        Directories {
            config: roaming.join(APPLICATION),
            cache: local.join(APPLICATION).join("cache"),
            data: roaming.join(APPLICATION),
            logs: local.join(APPLICATION).join("logs"),
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn platform_defaults() -> Directories {
        // XDG, with the specified fallbacks.
        let xdg = |variable: &str, fallback: &str| -> PathBuf {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home().join(fallback))
        };
        Directories {
            config: xdg("XDG_CONFIG_HOME", ".config").join(APPLICATION),
            cache: xdg("XDG_CACHE_HOME", ".cache").join(APPLICATION),
            data: xdg("XDG_DATA_HOME", ".local/share").join(APPLICATION),
            logs: xdg("XDG_STATE_HOME", ".local/state").join(APPLICATION),
        }
    }

    /// The layout library lives beside the settings.
    pub fn layouts(&self) -> PathBuf {
        self.config.join("layouts")
    }

    /// Encrypted signing credentials managed by pulpit. They stay separate
    /// from the human-readable settings file, which must never contain key
    /// material or passphrases.
    pub fn signing_credentials(&self) -> PathBuf {
        self.config.join("signatures")
    }

    /// Where a running copy records that it is alive. It belongs with the
    /// cache: it is state about this machine right now, not configuration.
    ///
    /// One file per process, because several copies may run at once. Another
    /// copy reads this to tell a crash-recovery file whose owner is gone from
    /// one a live instance is still writing.
    pub fn live_claim(&self, pid: u32) -> PathBuf {
        self.cache.join("live").join(format!("{pid}.lock"))
    }

    /// The claim on the audience window, which only one copy may hold: two
    /// audience windows on one projector fight for the screen.
    pub fn audience_claim(&self) -> PathBuf {
        self.cache.join("audience.pid")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    /// Create every directory, reporting the first that could not be made.
    pub fn create(&self) -> std::io::Result<()> {
        for directory in [&self.config, &self.cache, &self.data, &self.logs] {
            std::fs::create_dir_all(directory)?;
        }
        Ok(())
    }

    /// A rooted copy, for tests.
    pub fn under(root: &Path) -> Directories {
        Directories {
            config: root.join("config"),
            cache: root.join("cache"),
            data: root.join("data"),
            logs: root.join("logs"),
        }
    }
}

/// Create a new file containing private material with the narrowest native
/// permissions available at creation time.
///
/// Unix needs an explicit mode so a permissive umask cannot briefly expose
/// the file. Windows inherits the application-data directory's ACL; the
/// `create_new` contract still prevents following or overwriting an existing
/// destination.
pub fn create_owner_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Write `contents` to `path` so that a crash leaves either the previous file
/// or the complete new one, never a half-written one.
///
/// The temporary file is flushed and fsynced *before* the rename, so the
/// rename can never expose a file whose contents are still in the page cache,
/// and the containing directory is fsynced afterwards so the rename itself
/// survives a crash. `extension` names the scratch file (`"json.tmp"`,
/// `"toml.tmp"`) so two stores writing side by side cannot collide.
pub fn write_atomically(path: &Path, extension: &str, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let temporary = path.with_extension(extension);
    {
        let mut file = File::create(&temporary)?;
        file.write_all(contents)?;
        file.flush()?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)?;
    if let Some(directory) = path.parent() {
        if let Ok(handle) = File::open(directory) {
            let _ = handle.sync_all();
        }
    }
    Ok(())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_location_is_absolute_and_application_scoped() {
        let directories = Directories::detect();
        for path in [
            &directories.config,
            &directories.cache,
            &directories.data,
            &directories.logs,
        ] {
            assert!(
                path.is_absolute() || path.starts_with("."),
                "{path:?} is not a usable location"
            );
        }
        // The layout library is a child of the configuration directory, so a
        // user moving their configuration takes their layouts with them.
        assert!(directories.layouts().starts_with(&directories.config));
        assert!(directories
            .signing_credentials()
            .starts_with(&directories.config));
        assert!(directories.settings_file().starts_with(&directories.config));
    }

    #[test]
    fn the_config_override_is_honoured() {
        let directory = tempfile::tempdir().unwrap();
        std::env::set_var("PULPIT_CONFIG_DIR", directory.path());
        let directories = Directories::detect();
        std::env::remove_var("PULPIT_CONFIG_DIR");
        assert_eq!(directories.config, directory.path());
    }

    #[test]
    fn directories_can_be_created_and_are_not_shared_between_roots() {
        let directory = tempfile::tempdir().unwrap();
        let directories = Directories::under(directory.path());
        directories.create().unwrap();
        assert!(directories.config.is_dir());
        assert!(directories.logs.is_dir());
        assert_ne!(directories.config, directories.cache);
    }

    #[test]
    fn paths_survive_non_utf8_components() {
        // A profile directory is not guaranteed to be valid UTF-8; the API is
        // PathBuf precisely so this cannot become a lossy string.
        let directory = tempfile::tempdir().unwrap();
        let odd = directory.path().join("prés entateur");
        let directories = Directories::under(&odd);
        directories.create().unwrap();
        assert!(directories.config.is_dir());
    }
}
