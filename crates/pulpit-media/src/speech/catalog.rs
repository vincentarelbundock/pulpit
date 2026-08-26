//! What can be spoken with, what is installed, and what would have to be
//! fetched first.
//!
//! Engines are *data*, not code. A voice is a language tag, a sample rate and
//! a list of files with pinned hashes; an engine build is a URL, a hash and
//! the path of the program inside the archive. Nothing here knows that piper
//! exists in particular, which is the property that matters: when some better
//! synthesiser turns up in three years, or when this one is abandoned,
//! supporting it is a catalog entry rather than a rewrite.
//!
//! Hashes are the security boundary, not a reproducibility nicety. These
//! artifacts are fetched over the network onto a user's machine and then
//! executed — the engine literally so, the model as a graph handed to an
//! inference runtime. Every file is verified before it is used and deleted if
//! it does not match.

use std::path::{Path, PathBuf};

use pulpit_core::speech::LanguageTag;
use serde::{Deserialize, Serialize};

/// The voice catalog shipped with the application.
const VOICES: &str = include_str!("../../assets/voices.json");
/// The engine builds shipped with the application.
const ENGINES: &str = include_str!("../../assets/engines.json");

/// How good a voice is meant to sound, as the publisher graded it.
///
/// Kept as the publisher's own word rather than a number: it is shown to the
/// reader beside the download size, and "medium, 63 MB" against "high,
/// 114 MB" is the trade they are actually making.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    XLow,
    Low,
    Medium,
    High,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Quality::XLow => "very low",
            Quality::Low => "low",
            Quality::Medium => "medium",
            Quality::High => "high",
        }
    }
}

/// One file a voice needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceFile {
    /// Filename on disk, inside the voice's own directory.
    pub name: String,
    pub url: String,
    /// Lowercase hex sha256. Verified before the file is ever used.
    pub sha256: String,
    pub bytes: u64,
}

/// A voice that can be installed and spoken with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voice {
    pub id: String,
    /// Which engine speaks it. Matches [`EngineBuild::id`]-style naming.
    pub engine: String,
    pub language: LanguageTag,
    pub speaker: String,
    pub language_name: String,
    pub country: String,
    pub quality: Quality,
    /// Hertz, from the voice's own configuration.
    ///
    /// Per voice, never per quality tier and never a constant: the shipped
    /// catalog contains 16000, 22050 and 44100 Hz voices, and two voices of
    /// the same language and the same tier differ. Playing a voice at the
    /// wrong rate is not subtle — it is chipmunk or slow-motion speech — and
    /// it is the single easiest thing to get wrong here.
    pub sample_rate: u32,
    pub files: Vec<VoiceFile>,
}

impl Voice {
    /// Total download size.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    /// "Thorsten — German (Germany), medium".
    pub fn label(&self) -> String {
        let speaker = capitalise(&self.speaker);
        if self.country.is_empty() {
            format!(
                "{speaker} — {}, {}",
                self.language_name,
                self.quality.label()
            )
        } else {
            format!(
                "{speaker} — {} ({}), {}",
                self.language_name,
                self.country,
                self.quality.label()
            )
        }
    }

    /// The model file: the one an engine is pointed at.
    pub fn model(&self) -> Option<&VoiceFile> {
        self.files.iter().find(|file| file.name.ends_with(".onnx"))
    }
}

fn capitalise(text: &str) -> String {
    // Speaker names arrive as catalog slugs: `hfc_female`, `upc_ona`.
    let spaced = text.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

/// How an engine archive is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

/// One platform's build of a synthesiser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineBuild {
    /// `linux`, `macos`, `windows`.
    pub os: String,
    /// `x86_64`, `aarch64`.
    pub arch: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub archive: ArchiveKind,
    /// Path of the executable *inside* the extracted archive.
    pub program: String,
}

impl EngineBuild {
    /// Whether this build is for the machine we are running on.
    pub fn matches_host(&self) -> bool {
        self.os == host_os() && self.arch == host_arch()
    }
}

pub fn host_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

pub fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// Everything installable, as shipped.
#[derive(Debug, Clone)]
pub struct Catalog {
    voices: Vec<Voice>,
    engines: Vec<EngineBuild>,
}

impl Catalog {
    /// The catalog compiled into the application.
    ///
    /// Panics only if the shipped assets are malformed, which is a build-time
    /// mistake rather than anything a user can cause — and a test below
    /// parses them, so it is caught in CI rather than on a stage.
    pub fn builtin() -> Catalog {
        Catalog {
            voices: serde_json::from_str(VOICES).expect("shipped voices.json parses"),
            engines: serde_json::from_str(ENGINES).expect("shipped engines.json parses"),
        }
    }

    pub fn voices(&self) -> &[Voice] {
        &self.voices
    }

    pub fn voice(&self, id: &str) -> Option<&Voice> {
        self.voices.iter().find(|voice| voice.id == id)
    }

    /// The engine build for this machine, if one is published.
    pub fn engine_for_host(&self) -> Option<&EngineBuild> {
        self.engines.iter().find(|build| build.matches_host())
    }

    /// Every voice for a language, best quality first.
    ///
    /// Used by the download prompt when `Auto` meets a language with nothing
    /// installed: the reader is offered the best voice for the language they
    /// are actually reading, not an alphabetical list of everything.
    pub fn for_language(&self, wanted: &LanguageTag) -> Vec<&Voice> {
        let mut found: Vec<&Voice> = self
            .voices
            .iter()
            .filter(|voice| voice.language.same_language(wanted))
            .collect();
        found.sort_by(|a, b| {
            // Exact region first, then quality, then a stable tie-break.
            let region = |voice: &Voice| voice.language.region() == wanted.region();
            region(b)
                .cmp(&region(a))
                .then(b.quality.cmp(&a.quality))
                .then(a.id.cmp(&b.id))
        });
        found
    }

    /// Every language the catalog can speak, for the settings picker.
    pub fn languages(&self) -> Vec<(LanguageTag, String)> {
        let mut seen: Vec<(LanguageTag, String)> = Vec::new();
        for voice in &self.voices {
            let bare = voice.language.without_region();
            if !seen.iter().any(|(tag, _)| *tag == bare) {
                seen.push((bare, voice.language_name.clone()));
            }
        }
        seen.sort_by(|a, b| a.1.cmp(&b.1));
        seen
    }
}

/// Where installed voices and engines live on this machine.
///
/// Under the platform data directory, not the cache: a 63 MB download the
/// reader deliberately asked for is not something a cache cleaner should be
/// free to delete. A system-wide root is searched first so a distribution
/// packager can pre-place voices and their users never see a download prompt
/// at all.
#[derive(Debug, Clone)]
pub struct Store {
    user: PathBuf,
    system: Vec<PathBuf>,
}

impl Store {
    pub fn new(data_directory: &Path) -> Store {
        Store {
            user: data_directory.join("speech"),
            system: system_roots(),
        }
    }

    /// A store rooted anywhere, for tests.
    pub fn under(root: &Path) -> Store {
        Store {
            user: root.to_path_buf(),
            system: Vec::new(),
        }
    }

    pub fn user_root(&self) -> &Path {
        &self.user
    }

    fn roots(&self) -> impl Iterator<Item = &Path> {
        self.system
            .iter()
            .map(PathBuf::as_path)
            .chain(std::iter::once(self.user.as_path()))
    }

    /// Where a voice's files live, if they are anywhere.
    pub fn voice_directory(&self, voice: &Voice) -> Option<PathBuf> {
        self.roots()
            .map(|root| root.join("voices").join(&voice.id))
            .find(|directory| {
                voice
                    .files
                    .iter()
                    .all(|file| directory.join(&file.name).is_file())
            })
    }

    /// Where a voice's files should be written.
    pub fn voice_target(&self, voice: &Voice) -> PathBuf {
        self.user.join("voices").join(&voice.id)
    }

    pub fn is_installed(&self, voice: &Voice) -> bool {
        self.voice_directory(voice).is_some()
    }

    /// The model file to hand an engine, if the voice is installed.
    pub fn model_path(&self, voice: &Voice) -> Option<PathBuf> {
        let directory = self.voice_directory(voice)?;
        let model = voice.model()?;
        Some(directory.join(&model.name))
    }

    /// Every installed voice, in catalog order.
    pub fn installed<'a>(&self, catalog: &'a Catalog) -> Vec<&'a Voice> {
        catalog
            .voices()
            .iter()
            .filter(|voice| self.is_installed(voice))
            .collect()
    }

    /// Where a downloaded engine is unpacked.
    pub fn engine_target(&self, engine: &str) -> PathBuf {
        self.user.join("engines").join(engine)
    }

    /// The downloaded engine program, if it is there and executable.
    pub fn engine_program(&self, engine: &str, build: &EngineBuild) -> Option<PathBuf> {
        self.roots()
            .map(|root| root.join("engines").join(engine).join(&build.program))
            .find(|path| path.is_file())
    }
}

/// Read-only locations a packager may have populated.
fn system_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(extra) = std::env::var_os("PULPIT_SPEECH_DIR") {
        roots.push(PathBuf::from(extra));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        roots.push(PathBuf::from("/usr/share/pulpit/speech"));
        roots.push(PathBuf::from("/usr/local/share/pulpit/speech"));
    }
    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from("/Library/Application Support/pulpit/speech"));
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_catalog_parses_and_is_not_empty() {
        let catalog = Catalog::builtin();
        assert!(catalog.voices().len() >= 120, "a useful spread of voices");
        assert!(catalog.engine_for_host().is_some(), "a build for this host");
    }

    /// Breadth, not just presence.
    ///
    /// The catalog is generated, and the generator's selection rule is easy
    /// to narrow by accident — an earlier one took a single voice per
    /// language, which left forty-one languages with one take-it-or-leave-it
    /// speaker. A reader who dislikes that voice has no second option, and
    /// nothing about the code would look wrong. So the shape of the selection
    /// is asserted here rather than trusted.
    #[test]
    fn the_widely_read_languages_offer_a_choice_of_voice() {
        let catalog = Catalog::builtin();
        for (tag, least) in [("en", 10), ("de", 3), ("fr", 3), ("es", 3)] {
            let wanted = LanguageTag::language(tag);
            let found = catalog.for_language(&wanted).len();
            assert!(
                found >= least,
                "{tag} offers {found} voices, expected at least {least}"
            );
        }

        // And most languages should have more than one, even if some
        // genuinely only have one published.
        let languages = catalog.languages();
        let sole = languages
            .iter()
            .filter(|(tag, _)| catalog.for_language(tag).len() == 1)
            .count();
        assert!(
            sole * 2 < languages.len(),
            "{sole} of {} languages have a single voice",
            languages.len()
        );
    }

    #[test]
    fn every_shipped_voice_is_pinned_and_plausible() {
        for voice in Catalog::builtin().voices() {
            assert!(!voice.files.is_empty(), "{} has files", voice.id);
            assert!(voice.model().is_some(), "{} has a model", voice.id);
            assert!(
                (8_000..=48_000).contains(&voice.sample_rate),
                "{} has a plausible rate, got {}",
                voice.id,
                voice.sample_rate
            );
            for file in &voice.files {
                assert_eq!(
                    file.sha256.len(),
                    64,
                    "{} / {} is pinned to a sha256",
                    voice.id,
                    file.name
                );
                assert!(
                    file.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                    "{} / {} pin is hex",
                    voice.id,
                    file.name
                );
                assert!(
                    file.url.starts_with("https://"),
                    "{} is fetched over TLS",
                    file.name
                );
                assert!(file.bytes > 0);
            }
        }
    }

    #[test]
    fn every_engine_build_is_pinned() {
        for build in Catalog::builtin().engines {
            assert_eq!(build.sha256.len(), 64, "{}/{} pinned", build.os, build.arch);
            assert!(build.url.starts_with("https://"));
            assert!(!build.program.is_empty());
        }
    }

    #[test]
    fn sample_rates_really_do_differ_between_voices() {
        // The catalog is the evidence for the per-voice rule: if this ever
        // collapses to one value, the rule is still right but this test has
        // stopped defending it.
        let catalog = Catalog::builtin();
        let mut rates: Vec<u32> = catalog.voices().iter().map(|v| v.sample_rate).collect();
        rates.sort_unstable();
        rates.dedup();
        assert!(
            rates.len() > 1,
            "voices disagree about sample rate: {rates:?}"
        );
    }

    #[test]
    fn languages_resolve_to_their_best_voice_first() {
        let catalog = Catalog::builtin();
        let english = LanguageTag::parse("en-US").unwrap();
        let found = catalog.for_language(&english);
        assert!(!found.is_empty());
        // A US voice before a GB one, and high before medium within that.
        assert_eq!(found[0].language.region(), Some("US"));
        assert_eq!(found[0].quality, Quality::High);

        assert!(catalog
            .for_language(&LanguageTag::parse("xx").unwrap())
            .is_empty());
    }

    #[test]
    fn the_language_list_is_deduplicated_and_named() {
        let catalog = Catalog::builtin();
        let languages = catalog.languages();
        assert!(languages.len() >= 40);
        let english: Vec<_> = languages
            .iter()
            .filter(|(tag, _)| tag.primary() == "en")
            .collect();
        assert_eq!(english.len(), 1, "en-US and en-GB collapse to one entry");
        assert!(languages.iter().all(|(_, name)| !name.is_empty()));
    }

    #[test]
    fn an_empty_store_has_nothing_installed() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::under(temporary.path());
        let catalog = Catalog::builtin();
        assert!(store.installed(&catalog).is_empty());
        let voice = &catalog.voices()[0];
        assert!(!store.is_installed(voice));
        assert!(store.model_path(voice).is_none());
    }

    #[test]
    fn a_voice_counts_as_installed_only_when_every_file_is_present() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::under(temporary.path());
        let catalog = Catalog::builtin();
        let voice = catalog.voice("en_US-lessac-medium").expect("shipped");

        let target = store.voice_target(voice);
        std::fs::create_dir_all(&target).unwrap();
        // The model alone is not enough: piper needs its configuration too,
        // and a half-finished download must not look ready.
        std::fs::write(target.join(&voice.model().unwrap().name), b"x").unwrap();
        assert!(
            !store.is_installed(voice),
            "a partial install is not installed"
        );

        for file in &voice.files {
            std::fs::write(target.join(&file.name), b"x").unwrap();
        }
        assert!(store.is_installed(voice));
        assert_eq!(store.installed(&catalog).len(), 1);
        let model = store.model_path(voice).unwrap();
        assert_eq!(model.extension().and_then(|e| e.to_str()), Some("onnx"));
        assert!(model.starts_with(store.user_root()));
    }

    #[test]
    fn labels_read_as_english_rather_than_as_slugs() {
        let catalog = Catalog::builtin();
        let voice = catalog.voice("en_US-hfc_female-medium").expect("shipped");
        let label = voice.label();
        assert!(label.starts_with("Hfc female"), "got {label}");
        assert!(label.contains("English"));
        assert!(label.contains("medium"));
    }
}
