//! Atomic, schema-versioned settings persistence.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::settings::{Settings, SCHEMA_VERSION};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot parse settings: {0}")]
    Parse(String),
    #[error("cannot serialise settings: {0}")]
    Serialise(String),
    #[error("settings schema {found} is newer than this build understands ({known})")]
    FutureSchema { found: u32, known: u32 },
}

/// Where settings live. The platform crate owns the per-platform conventions
/// and the `PULPIT_CONFIG_DIR` override; this is a thin alias so callers
/// that only want the configuration directory need not know about them.
pub fn config_directory() -> PathBuf {
    crate::platform::Directories::detect().config
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::new(config_directory().join("settings.toml"))
    }
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn backup_path(&self) -> PathBuf {
        self.path.with_extension("toml.bak")
    }

    /// Load settings, falling back to the backup and then to defaults.
    /// Corruption is never fatal: a presentation must still start.
    pub fn load(&self) -> Settings {
        match self.try_load(&self.path) {
            Ok(settings) => settings,
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "settings unreadable");
                match self.try_load(&self.backup_path()) {
                    Ok(settings) => {
                        tracing::warn!("recovered settings from the last-known-good backup");
                        settings
                    }
                    Err(_) => {
                        tracing::warn!("falling back to default settings");
                        Settings::default()
                    }
                }
            }
        }
    }

    fn try_load(&self, path: &Path) -> Result<Settings, SettingsError> {
        let text = std::fs::read_to_string(path)?;
        let mut value: toml::Value =
            toml::from_str(&text).map_err(|e| SettingsError::Parse(e.to_string()))?;
        let schema = value
            .get("schema")
            .and_then(|schema| schema.as_integer())
            .unwrap_or(SCHEMA_VERSION as i64) as u32;
        if schema > SCHEMA_VERSION {
            return Err(SettingsError::FutureSchema {
                found: schema,
                known: SCHEMA_VERSION,
            });
        }
        migrate(&mut value, schema)?;
        let mut settings: Settings = value
            .try_into()
            .map_err(|e: toml::de::Error| SettingsError::Parse(e.to_string()))?;
        settings.signatures.sanitise();
        Ok(settings)
    }

    /// Write settings atomically: temporary file in the same directory,
    /// flushed and fsynced, then renamed over the destination, with the
    /// previous version retained as a backup.
    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        let mut settings = settings.clone();
        settings.schema = SCHEMA_VERSION;
        let text = toml::to_string_pretty(&settings)
            .map_err(|e| SettingsError::Serialise(e.to_string()))?;

        let directory = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)?;

        if self.path.exists() {
            let _ = std::fs::copy(&self.path, self.backup_path());
        }

        crate::platform::paths::write_atomically(&self.path, text.as_bytes())?;
        Ok(())
    }

    /// Resolve a managed credential only after validating the profile id.
    pub fn managed_credential_path(&self, id: &str) -> Option<PathBuf> {
        valid_profile_id(id).then(|| {
            self.path
                .parent()
                .unwrap_or(Path::new("."))
                .join("signatures")
                .join(format!("{id}.p12"))
        })
    }

    /// Atomically write a newly generated encrypted credential without ever
    /// exposing a partial or broadly-permissioned live file.
    pub fn write_managed_credential(
        &self,
        id: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, SettingsError> {
        let destination = self
            .managed_credential_path(id)
            .ok_or_else(|| SettingsError::Parse("invalid signature profile id".to_string()))?;
        let directory = destination.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)?;
        if destination.exists() {
            return Err(SettingsError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a credential already exists for this profile",
            )));
        }
        let temporary = destination.with_extension("p12.tmp");
        let result = (|| -> Result<(), SettingsError> {
            let mut file = crate::platform::paths::create_owner_private_file(&temporary)?;
            file.write_all(bytes)?;
            file.flush()?;
            file.sync_all()?;
            std::fs::rename(&temporary, &destination)?;
            if let Ok(handle) = std::fs::File::open(directory) {
                let _ = handle.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map(|()| destination)
    }

    pub fn remove_managed_credential(&self, id: &str) -> Result<(), SettingsError> {
        let path = self
            .managed_credential_path(id)
            .ok_or_else(|| SettingsError::Parse("invalid signature profile id".to_string()))?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn valid_profile_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Explicit, versioned migrations. Each step upgrades one version.
fn migrate(value: &mut toml::Value, from: u32) -> Result<(), SettingsError> {
    let mut version = from;
    while version < SCHEMA_VERSION {
        match version {
            // 0 is "a file written before schemas existed": nothing to do
            // beyond stamping the version.
            0 => {}
            // Colour overrides were added in schema 2. The section is sparse
            // and serde supplies an empty one, so existing files need no
            // value transformation.
            1 => {}
            // Shortcut customisation is deliberately unavailable for now.
            // Remove the complete persisted map so old settings cannot
            // silently override the fixed, documented keyboard vocabulary.
            2 => {
                if let Some(table) = value.as_table_mut() {
                    table.remove("keymap");
                }
            }
            // Reusable signing profiles were added in schema 4. Serde's
            // default is an empty profile list, so old files need no value
            // transformation.
            3 => {}
            other => {
                return Err(SettingsError::Parse(format!(
                    "no migration from schema {other}"
                )))
            }
        }
        version += 1;
    }
    if let Some(table) = value.as_table_mut() {
        table.insert("schema".into(), toml::Value::Integer(SCHEMA_VERSION as i64));
    }
    Ok(())
}

pub fn load_or_default() -> Settings {
    SettingsStore::default().load()
}

#[allow(dead_code)] // unreached, including by its own tests
pub fn save(settings: &Settings) -> Result<(), SettingsError> {
    SettingsStore::default().save(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{
        HotplugPolicy, SigningIdentityProfile, StoredCredential, StoredCredentialSummary,
        StoredSignatureAppearance,
    };
    use crate::theme::ColorRole;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path().join("settings.toml"));
        (dir, store)
    }

    fn profile(id: &str) -> SigningIdentityProfile {
        SigningIdentityProfile {
            id: id.to_string(),
            name: "Work".to_string(),
            identity: StoredCredentialSummary {
                subject: "CN=Jane Doe".to_string(),
                issuer: "CN=Jane Doe".to_string(),
                serial: "01".to_string(),
                not_before: "2026-01-01T00:00:00Z".to_string(),
                not_after: "2031-01-01T00:00:00Z".to_string(),
                sha256_fingerprint: "AA".repeat(32),
                key_algorithm: "ECDSA P-256".to_string(),
                key_bits: Some(256),
            },
            credential: StoredCredential::Managed,
            appearance: StoredSignatureAppearance::default(),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let (_dir, store) = store();
        let mut settings = Settings::default();
        settings.display.hotplug = HotplugPolicy::Ask;
        settings.rendering.cache_budget_mib = 512;
        settings.remember_recent(PathBuf::from("/decks/talk.pdf"));
        settings.appearance.colors.set(
            crate::settings::ColorScheme::Dark,
            ColorRole::Accent,
            "#123456".into(),
        );
        let id = "0123456789abcdef0123456789abcdef";
        settings.signatures.profiles.push(profile(id));
        settings.signatures.default_profile = Some(id.to_string());

        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.display.hotplug, HotplugPolicy::Ask);
        assert_eq!(loaded.rendering.cache_budget_mib, 512);
        assert_eq!(
            loaded
                .appearance
                .colors
                .value(crate::settings::ColorScheme::Dark, ColorRole::Accent),
            "#123456"
        );
        assert_eq!(
            loaded.recent.front().unwrap(),
            &PathBuf::from("/decks/talk.pdf")
        );
        assert_eq!(loaded.schema, SCHEMA_VERSION);
        assert_eq!(loaded.signatures, settings.signatures);
    }

    #[test]
    fn managed_credentials_are_private_atomic_and_removable() {
        let (_dir, store) = store();
        let id = "0123456789abcdef0123456789abcdef";
        let path = store
            .write_managed_credential(id, b"encrypted-p12")
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"encrypted-p12");
        assert!(store.write_managed_credential(id, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"encrypted-p12");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        store.remove_managed_credential(id).unwrap();
        assert!(!path.exists());
        store.remove_managed_credential(id).unwrap();
    }

    #[test]
    fn an_untrusted_profile_id_cannot_escape_the_signatures_directory() {
        let (_dir, store) = store();
        assert!(store.managed_credential_path("../../elsewhere").is_none());
        assert!(store
            .write_managed_credential("../../elsewhere", b"no")
            .is_err());
    }

    #[test]
    fn loaded_profiles_repair_duplicate_ids_and_unbounded_ink() {
        let (_dir, store) = store();
        let id = "0123456789abcdef0123456789abcdef";
        let mut first = profile(id);
        first.appearance.stroke_width = 100.0;
        first.appearance.strokes = vec![vec![
            crate::settings::SignaturePoint { x: -1.0, y: 0.5 },
            crate::settings::SignaturePoint { x: 2.0, y: 1.5 },
        ]];
        let mut settings = Settings::default();
        settings.signatures.profiles = vec![first, profile(id)];
        store.save(&settings).unwrap();

        let loaded = store.load();
        assert_eq!(loaded.signatures.profiles.len(), 1);
        let appearance = &loaded.signatures.profiles[0].appearance;
        assert_eq!(appearance.stroke_width, 12.0);
        assert_eq!(appearance.strokes[0][0].x, 0.0);
        assert_eq!(appearance.strokes[0][1].x, 1.0);
        assert_eq!(appearance.strokes[0][1].y, 1.0);
    }

    #[test]
    fn a_missing_file_yields_defaults() {
        let (_dir, store) = store();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn corruption_falls_back_to_the_backup_then_to_defaults() {
        let (_dir, store) = store();
        let mut good = Settings::default();
        good.rendering.workers = 4;
        store.save(&good).unwrap();
        // A second save moves the good copy to the backup.
        store.save(&good).unwrap();

        std::fs::write(store.path(), "this is not toml {{{").unwrap();
        assert_eq!(
            store.load().rendering.workers,
            4,
            "recovered from the backup"
        );

        std::fs::write(store.backup_path(), "also broken {{{").unwrap();
        assert_eq!(store.load(), Settings::default(), "defaults, not a crash");
    }

    #[test]
    fn a_partial_write_is_never_visible() {
        let (_dir, store) = store();
        let mut settings = Settings::default();
        settings.rendering.workers = 3;
        store.save(&settings).unwrap();

        // Simulate a crash mid-save: a temporary file exists but was never
        // renamed. The real settings must be untouched.
        std::fs::write(store.path().with_extension("toml.tmp"), "truncated conte").unwrap();
        assert_eq!(store.load().rendering.workers, 3);
        assert!(
            toml::from_str::<toml::Value>(&std::fs::read_to_string(store.path()).unwrap()).is_ok(),
            "the live file is always complete TOML"
        );
    }

    #[test]
    fn settings_from_the_future_are_refused_rather_than_misread() {
        let (_dir, store) = store();
        store.save(&Settings::default()).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        std::fs::write(
            store.path(),
            text.replace(&format!("schema = {SCHEMA_VERSION}"), "schema = 99"),
        )
        .unwrap();
        assert_eq!(
            store.load(),
            Settings::default(),
            "defaults beat a misparse"
        );
    }

    #[test]
    fn a_schemaless_legacy_file_is_migrated() {
        let (_dir, store) = store();
        std::fs::write(store.path(), "schema = 0\n[rendering]\nworkers = 7\n").unwrap();
        let loaded = store.load();
        assert_eq!(loaded.rendering.workers, 7);
        assert_eq!(loaded.schema, SCHEMA_VERSION);
    }

    #[test]
    fn unknown_keys_and_missing_sections_are_tolerated() {
        let (_dir, store) = store();
        std::fs::write(
            store.path(),
            "schema = 1\nsomething_new = true\n[timer]\ntarget_minutes = 45\n",
        )
        .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.timer.target_minutes, Some(45));
        assert_eq!(loaded.rendering, crate::settings::RenderSettings::default());
    }

    #[test]
    fn schema_one_gains_empty_color_overrides() {
        let (_dir, store) = store();
        std::fs::write(
            store.path(),
            "schema = 1\n[appearance]\nappearance = \"dark\"\n",
        )
        .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.schema, SCHEMA_VERSION);
        assert!(!loaded.appearance.colors.has_overrides());
    }

    #[test]
    fn schema_two_discards_the_persisted_custom_keymap() {
        let (_dir, store) = store();
        std::fs::write(
            store.path(),
            r#"schema = 2

[keymap]
bindings = [[{ kind = "named", key = "x" }, "next"]]
"#,
        )
        .unwrap();

        let loaded = store.load();
        assert_eq!(loaded.schema, SCHEMA_VERSION);
        assert_eq!(loaded.keymap.resolve(Some("x"), None), None);
        assert_eq!(
            loaded.keymap.resolve(Some("Right"), None),
            Some(crate::settings::Action::Next)
        );

        store.save(&loaded).unwrap();
        let saved = std::fs::read_to_string(store.path()).unwrap();
        assert!(!saved.contains("[keymap]"));
    }

    #[test]
    fn schema_three_gains_an_empty_signature_profile_list() {
        let (_dir, store) = store();
        std::fs::write(store.path(), "schema = 3\n").unwrap();
        let loaded = store.load();
        assert_eq!(loaded.schema, SCHEMA_VERSION);
        assert!(loaded.signatures.profiles.is_empty());
        assert!(loaded.signatures.default_profile.is_none());
    }
}
