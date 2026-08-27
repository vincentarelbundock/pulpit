//! Schema-versioned settings with atomic, crash-safe persistence, plus the
//! diagnostics bundle.
//!
//! A crash or a full disk must yield either the previous complete settings or
//! the new complete settings — never a truncated file.

// These modules were standalone library crates until the workspace was
// consolidated. They keep their complete, tested APIs — the parts the
// application does not happen to call yet are exercised by the tests beside
// them, and pruning them would throw away working, documented behaviour to
// satisfy a lint about a boundary that no longer exists.

pub mod diagnostics;
pub mod keys;
pub mod store;

pub use diagnostics::DiagnosticsBundle;
pub use keys::{Action, KeyBinding, Keymap, Mods};
pub use store::{load_or_default, SettingsStore};

use std::collections::VecDeque;
use std::path::PathBuf;

use pulpit_core::speech::{LanguageSetting, LanguageTag, SpeechRate};
use pulpit_core::NotesMapping;
use pulpit_display::DisplayRoles;
use serde::{Deserialize, Serialize};

/// Current settings schema version. Bump it *and* add a migration.
pub const SCHEMA_VERSION: u32 = 4;

/// How pulpit reacts when a second display appears mid-talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HotplugPolicy {
    /// Apply the configured roles immediately.
    #[default]
    Automatic,
    /// Offer it in the presenter window and wait for confirmation.
    Ask,
    /// Do nothing until the user asks.
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version of this document, checked and migrated on load.
    pub schema: u32,
    pub display: DisplaySettings,
    pub rendering: RenderSettings,
    pub notes: NotesSettings,
    pub timer: TimerSettings,
    /// Unattended page turning, which belongs to the room rather than to
    /// the deck: a lobby loop is a property of the screen it is left on.
    pub autoadvance: AutoadvanceSettings,
    /// The fixed runtime keymap. It is skipped on disk: shortcut
    /// customisation is intentionally unavailable until the interaction and
    /// migration design is ready to support it well.
    #[serde(skip)]
    pub keymap: Keymap,
    pub recent: VecDeque<PathBuf>,
    pub diagnostics: DiagnosticsSettings,
    pub layout: LayoutSettings,
    pub appearance: AppearanceSettings,
    pub reading: ReadingSettings,
    pub signatures: SignatureSettings,
    pub speech: SpeechSettings,
}

/// Reading aloud (issue #20).
///
/// The voice is stored by catalog id rather than by language, because a
/// reader who chose a particular speaker meant that speaker; `language`
/// separately governs what `Auto` does when a page turns out to be in
/// something else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeechSettings {
    /// Catalog id of the preferred voice, if one has been chosen.
    ///
    /// `None` means "whichever installed voice fits the page", which is the
    /// honest default before anything has been downloaded.
    pub voice: Option<String>,
    /// `Auto`, or a language the reader pinned.
    pub language: LanguageSetting,
    pub rate: SpeechRate,
    // There is deliberately no default-scope field. Each scope has its own
    // key — `r` reads the document, `Shift+R` this page — so a persisted
    // preference would be a setting nothing consults, which is worse than no
    // setting: the reader changes it, nothing happens, and the honest
    // conclusion is that the page is broken. (One existed briefly; a stored
    // `scope` value in an old settings file is ignored on load.)
    /// Languages the reader has declined to download a voice for.
    ///
    /// Without this, a bilingual document asks on every page turn, which is
    /// the sort of thing that makes a reader turn the whole feature off.
    pub declined: Vec<LanguageTag>,
}

impl Default for SpeechSettings {
    fn default() -> Self {
        SpeechSettings {
            voice: None,
            language: LanguageSetting::Auto,
            rate: SpeechRate::NORMAL,

            declined: Vec::new(),
        }
    }
}

impl SpeechSettings {
    /// Whether the reader has already said no to this language.
    pub fn has_declined(&self, language: &LanguageTag) -> bool {
        self.declined
            .iter()
            .any(|declined| declined.same_language(language))
    }

    pub fn decline(&mut self, language: LanguageTag) {
        if !self.has_declined(&language) {
            self.declined.push(language);
        }
    }

    /// Forget every refusal, so `Auto` starts offering downloads again.
    pub fn clear_declined(&mut self) {
        self.declined.clear();
    }
}

/// Reusable signing identities known to this installation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SignatureSettings {
    pub profiles: Vec<SigningIdentityProfile>,
    pub default_profile: Option<String>,
}

impl SignatureSettings {
    pub fn profile(&self, id: &str) -> Option<&SigningIdentityProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn profile_mut(&mut self, id: &str) -> Option<&mut SigningIdentityProfile> {
        self.profiles.iter_mut().find(|profile| profile.id == id)
    }

    pub fn remove(&mut self, id: &str) -> Option<SigningIdentityProfile> {
        let index = self.profiles.iter().position(|profile| profile.id == id)?;
        let removed = self.profiles.remove(index);
        if self.default_profile.as_deref() == Some(id) {
            self.default_profile = self.profiles.first().map(|profile| profile.id.clone());
        }
        Some(removed)
    }

    pub fn sanitise(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.profiles.retain_mut(|profile| {
            let keep = profile.has_valid_id() && seen.insert(profile.id.clone());
            if keep {
                profile.appearance.sanitise();
            }
            keep
        });
        let default_exists = self
            .default_profile
            .as_deref()
            .is_some_and(|id| self.profiles.iter().any(|profile| profile.id == id));
        if !default_exists {
            self.default_profile = self.profiles.first().map(|profile| profile.id.clone());
        }
    }
}

/// A profile combines one signing identity with its visible appearance.
/// Passphrases and private-key bytes are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigningIdentityProfile {
    /// 128 random bits, lowercase hexadecimal. Managed credential filenames
    /// are derived from this after validation, never read from settings.
    pub id: String,
    pub name: String,
    pub identity: StoredCredentialSummary,
    pub credential: StoredCredential,
    pub appearance: StoredSignatureAppearance,
}

impl SigningIdentityProfile {
    pub fn has_valid_id(&self) -> bool {
        self.id.len() == 32
            && self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum StoredCredential {
    /// An encrypted `{profile-id}.p12` below the application configuration
    /// directory. No persisted absolute path is needed.
    Managed,
    /// A credential owned elsewhere. Removing the profile never removes it.
    External { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredCredentialSummary {
    pub subject: String,
    pub issuer: String,
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    pub sha256_fingerprint: String,
    pub key_algorithm: String,
    pub key_bits: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StoredSignatureContent {
    Ink,
    #[default]
    Text,
    InkAndText,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignaturePoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StoredSignaturePosition {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StoredSignatureSize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredSignatureAppearance {
    pub content: StoredSignatureContent,
    pub strokes: Vec<Vec<SignaturePoint>>,
    pub stroke_width: f32,
    pub visible: bool,
    pub position: StoredSignaturePosition,
    pub size: StoredSignatureSize,
}

impl Default for StoredSignatureAppearance {
    fn default() -> Self {
        Self {
            content: StoredSignatureContent::Text,
            strokes: Vec::new(),
            stroke_width: 2.0,
            visible: true,
            position: StoredSignaturePosition::BottomRight,
            size: StoredSignatureSize::Medium,
        }
    }
}

impl StoredSignatureAppearance {
    fn sanitise(&mut self) {
        const MAX_STROKES: usize = 64;
        const MAX_POINTS_PER_STROKE: usize = 4096;

        if !self.stroke_width.is_finite() {
            self.stroke_width = 2.0;
        }
        self.stroke_width = self.stroke_width.clamp(1.0, 12.0);
        self.strokes.truncate(MAX_STROKES);
        for stroke in &mut self.strokes {
            stroke.truncate(MAX_POINTS_PER_STROKE);
            stroke.retain(|point| point.x.is_finite() && point.y.is_finite());
            for point in stroke.iter_mut() {
                point.x = point.x.clamp(0.0, 1.0);
                point.y = point.y.clamp(0.0, 1.0);
            }
        }
        self.strokes.retain(|stroke| stroke.len() >= 2);
    }
}

/// Which presenter layout is in use. Stored as a plain identifier so the
/// settings schema does not depend on the layout crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LayoutSettings {
    /// The active presenter layout, remembered across sessions.
    pub active: Option<String>,
    /// The active *document* layout, remembered separately.
    ///
    /// Separately on purpose (§2.3 of `SPEC-document.md`): presentation and
    /// document are two roots rather than two variants of one, so choosing a
    /// presenter layout must never change what a PDF opens into, and the
    /// reverse. One field would make each choice quietly overwrite the other.
    pub active_document: Option<String>,
    /// The layout a specific file was last put into by hand, keyed by the
    /// hash of its contents.
    ///
    /// Only a deliberate choice is recorded here. A file with no entry opens
    /// in the Reader; what the user chose while that file was open replaces
    /// that default for as long as the file is the same bytes.
    ///
    /// By content rather than by path so that moving or renaming a file does
    /// not lose the choice, and so that two copies of the same document agree.
    pub per_document: VecDeque<DocumentLayout>,
    /// The one-time "review this layout at your screen ratio" notice.
    pub ratio_notice_dismissed: bool,
}

/// One file's remembered layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentLayout {
    /// Lowercase hex BLAKE3 of the file's bytes.
    pub hash: String,
    /// The layout's identifier, as in [`LayoutSettings::active`].
    pub layout: String,
}

/// How many files remember a layout. Beyond this the least recently chosen
/// is dropped: the list is a convenience, and an unbounded one would grow
/// with every document ever opened for no benefit anybody would notice.
const MAX_REMEMBERED_LAYOUTS: usize = 200;

impl LayoutSettings {
    /// The layout this exact file was last put into by hand, if any.
    pub fn layout_for_document(&self, hash: &str) -> Option<&str> {
        self.per_document
            .iter()
            .find(|entry| entry.hash == hash)
            .map(|entry| entry.layout.as_str())
    }

    /// Record a deliberate choice for this file, most recent first.
    pub fn remember_layout_for_document(&mut self, hash: String, layout: String) {
        self.per_document.retain(|entry| entry.hash != hash);
        self.per_document
            .push_front(DocumentLayout { hash, layout });
        while self.per_document.len() > MAX_REMEMBERED_LAYOUTS {
            self.per_document.pop_back();
        }
    }

    /// Carry a file's choice onto the copy just written from it.
    ///
    /// Saving produces a second file with the same content in it and different
    /// bytes around it, so it hashes differently and would otherwise open as
    /// if it had never been seen. The source keeps its own entry: pulpit never
    /// writes over the file it opened (A6), so that file still exists and is
    /// still what its entry describes.
    pub fn carry_layout_to_saved_copy(&mut self, from: &str, to: String) {
        if let Some(layout) = self.layout_for_document(from).map(str::to_string) {
            self.remember_layout_for_document(to, layout);
        }
    }
}

/// Where the reader was in each document it has read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadingSettings {
    /// Most recently left first.
    pub positions: VecDeque<ReadingPosition>,
    /// Whether leaving a document records where you were. Off means the list
    /// stops growing; clearing it is a separate, deliberate act.
    pub remember: bool,
}

impl Default for ReadingSettings {
    fn default() -> Self {
        Self {
            positions: VecDeque::new(),
            // On: being put back where you were is the behaviour a reader
            // expects of a document, and one that has to be found and switched
            // on is one most readers never get.
            remember: true,
        }
    }
}

/// One document's remembered reading position.
///
/// Keyed *twice*, and the two keys answer different questions.
///
/// The content hash is the primary key, for the same reason it is the layout
/// store's only key: a preference should follow the document when it is moved,
/// renamed or copied, and two copies of one file should agree about it. But a
/// position is not quite a preference. The reader who most wants to be put
/// back where they were is the author running `make` on a paper, and every
/// recompile writes different bytes — so the content hash, which is exactly
/// right about identity, misses on the one workflow the feature is for.
///
/// The path is therefore kept as a weaker second key, used only when the hash
/// misses. It answers "the file that lives here, whatever it now contains",
/// which is a real but much softer claim: the file at a path can be a
/// different document altogether. What survives a path-only match is the page
/// number and nothing else — see [`RestoredPosition`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingPosition {
    /// Lowercase hex BLAKE3 of the file's bytes, or `None` when it could not
    /// be read at the moment the position was taken.
    pub hash: Option<String>,
    /// Where the file was when the position was taken.
    pub path: PathBuf,
    /// Zero-based page index.
    pub page: usize,
    pub zoom: StoredZoom,
    /// How far down `page` the window was, as a fraction of that page's
    /// height. Zero is the page's top.
    ///
    /// A fraction rather than an offset in points because points are a number
    /// about a particular zoom in a particular window, and neither is
    /// necessarily the one the document is reopened in.
    pub fraction: f32,
    /// Whether the outline rail was open.
    ///
    /// Lives beside the page rather than in a global preference because it
    /// is not one: it is part of where the reader was in *this* document,
    /// same as the page and the scroll fraction are, and it is exactly as
    /// meaningless as those are for a document nobody has opened yet.
    /// `serde(default)` reads an older record — written before the sidebar
    /// had a memory — as closed, which is the wanted answer for it.
    #[serde(default)]
    pub outline_open: bool,
    /// Whether the search pane was open. Same reasoning as `outline_open`.
    #[serde(default)]
    pub search_open: bool,
}

/// The reader's zoom, in the settings schema's own vocabulary.
///
/// Spelled out here rather than reusing the reader's `Zoom` for the same
/// reason [`LayoutSettings::active`] is a plain `String`: what is written to
/// disk is a format with a compatibility obligation, and it should not change
/// shape because a widget's enum did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StoredZoom {
    #[default]
    FitWidth,
    FitPage,
    FitHeight,
    Fixed(f32),
}

/// What a lookup found, and how much of it can be believed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RestoredPosition {
    /// The same bytes. Everything recorded still describes this document, so
    /// all of it is restored.
    Exact {
        page: usize,
        zoom: StoredZoom,
        fraction: f32,
        outline_open: bool,
        search_open: bool,
    },
    /// The same path, different bytes: the document was rebuilt or edited
    /// under us. The page number is the most that can honestly survive that —
    /// a recompiled paper is usually about as long and usually says roughly
    /// the same thing on page nine — and a fraction into a page whose content
    /// has moved is a precision the record no longer has. The zoom is dropped
    /// with it rather than applied to a document that may not be the same
    /// shape.
    ///
    /// The sidebar state survives this drop, unlike the fraction and the
    /// zoom: whether the rail was open is a UI choice, not a coordinate into
    /// the text, so it stays meaningful even when the bytes underneath moved
    /// and only the page number could honestly come with it.
    PageOnly {
        page: usize,
        outline_open: bool,
        search_open: bool,
    },
}

/// How many documents remember a position. The same bound and the same
/// reasoning as [`MAX_REMEMBERED_LAYOUTS`].
const MAX_REMEMBERED_POSITIONS: usize = 200;

impl ReadingSettings {
    /// Where this document was left, if anywhere.
    ///
    /// The content hash first, then the path. A hash of `None` — an unreadable
    /// file — matches nothing by hash, which is the right answer: it is not
    /// evidence that this is the same document, it is the absence of evidence.
    pub fn position_for(
        &self,
        hash: Option<&str>,
        path: &std::path::Path,
    ) -> Option<RestoredPosition> {
        if let Some(hash) = hash {
            if let Some(entry) = self
                .positions
                .iter()
                .find(|entry| entry.hash.as_deref() == Some(hash))
            {
                return Some(RestoredPosition::Exact {
                    page: entry.page,
                    zoom: entry.zoom,
                    fraction: entry.fraction,
                    outline_open: entry.outline_open,
                    search_open: entry.search_open,
                });
            }
        }
        self.positions
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| RestoredPosition::PageOnly {
                page: entry.page,
                outline_open: entry.outline_open,
                search_open: entry.search_open,
            })
    }

    /// Record where a document was left, most recent first.
    ///
    /// An entry is replaced when *either* key matches, not only the hash. The
    /// recompiling author is the reason: one entry per build would fill the
    /// whole list with one paper in an afternoon and evict every other
    /// document to hold a hundred stale copies of that one.
    pub fn remember_position(&mut self, position: ReadingPosition) {
        self.positions.retain(|entry| {
            let same_document = match (&entry.hash, &position.hash) {
                (Some(existing), Some(new)) => existing == new,
                _ => false,
            };
            !same_document && entry.path != position.path
        });
        self.positions.push_front(position);
        while self.positions.len() > MAX_REMEMBERED_POSITIONS {
            self.positions.pop_back();
        }
    }

    /// Forget every remembered position. The privacy control: a reading list
    /// is a record of what someone has read, and it must be possible to say
    /// so and be rid of it.
    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub fn forget_all(&mut self) {
        self.positions.clear();
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            display: DisplaySettings::default(),
            rendering: RenderSettings::default(),
            notes: NotesSettings::default(),
            timer: TimerSettings::default(),
            autoadvance: AutoadvanceSettings::default(),
            keymap: Keymap::default(),
            recent: VecDeque::new(),
            diagnostics: DiagnosticsSettings::default(),
            layout: LayoutSettings::default(),
            appearance: AppearanceSettings::default(),
            reading: ReadingSettings::default(),
            signatures: SignatureSettings::default(),
            speech: SpeechSettings::default(),
        }
    }
}

const MAX_RECENT: usize = 10;

impl Settings {
    pub fn remember_recent(&mut self, path: PathBuf) {
        self.recent.retain(|existing| existing != &path);
        self.recent.push_front(path);
        while self.recent.len() > MAX_RECENT {
            self.recent.pop_back();
        }
    }

    /// Notes mapping remembered for a specific document, else the default.
    pub fn mapping_for(&self, path: &std::path::Path) -> NotesMapping {
        self.notes
            .per_document
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.mapping.clone())
            .unwrap_or_else(|| self.notes.default_mapping.clone())
    }

    pub fn remember_mapping(&mut self, path: PathBuf, mapping: NotesMapping) {
        match self
            .notes
            .per_document
            .iter_mut()
            .find(|entry| entry.path == path)
        {
            Some(entry) => entry.mapping = mapping,
            None => self
                .notes
                .per_document
                .push(DocumentMapping { path, mapping }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    pub roles: DisplayRoles,
    pub hotplug: HotplugPolicy,
    /// Milliseconds to let the desktop geometry settle before reconciling.
    pub settle_ms: u64,
    /// How often to poll for topology changes when no native listener is
    /// available.
    pub poll_ms: u64,
    /// Keep the screensaver away while the audience output is fullscreen.
    pub inhibit_screensaver: bool,
    /// What the blank key turns the audience screen into.
    ///
    /// Blanking is one key, so this is the only place the colour is chosen.
    /// Black disappears in a dark room, white is the one that reads as
    /// deliberate under bright house lights, and which is wanted is a
    /// property of the venue rather than of the deck.
    pub blank_color: BlankColor,
}

/// Which colour a blanked audience screen becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BlankColor {
    #[default]
    Black,
    White,
}

impl BlankColor {
    /// The presentation state this colour asks for.
    pub fn blank(self) -> pulpit_core::Blank {
        match self {
            BlankColor::Black => pulpit_core::Blank::Black,
            BlankColor::White => pulpit_core::Blank::White,
        }
    }
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            roles: DisplayRoles::default(),
            hotplug: HotplugPolicy::default(),
            settle_ms: 250,
            poll_ms: 1000,
            inhibit_screensaver: true,
            blank_color: BlankColor::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderSettings {
    /// How many renderer processes to run. Defaults to the machine's
    /// parallelism less one, so warming a deck's thumbnails — the only work
    /// that ever saturates the pool — uses the cores that are there while
    /// leaving one for the event loop. See [`default_workers`].
    pub workers: usize,
    /// Combined CPU+GPU frame budget in mebibytes.
    pub cache_budget_mib: u64,
    /// Width of the coarse first pass, in pixels.
    pub coarse_width: u32,
    /// Seconds before an unresponsive worker is replaced.
    pub worker_deadline_secs: u64,
    /// Watch the open PDF and reload it when it is rebuilt.
    pub watch_document: bool,
    /// Debounce window for file changes, in milliseconds.
    pub watch_debounce_ms: u64,
}

/// How many renderer workers to run when nothing has been configured.
///
/// Two was a starting point chosen for safety, and it is the wrong shape for
/// the one job that is genuinely parallel: warming a five-hundred-page deck's
/// thumbnails, which is embarrassingly parallel and finishes in proportion to
/// the pool. One core is left for the event loop, and the ceiling is low
/// enough that a many-core machine does not spawn a pool whose resident PDFium
/// copies cost more than the warming saves.
pub fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|cores| cores.get().saturating_sub(1))
        .unwrap_or(2)
        .clamp(2, 6)
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            workers: default_workers(),
            cache_budget_mib: 256,
            coarse_width: 640,
            worker_deadline_secs: 10,
            watch_document: true,
            watch_debounce_ms: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentMapping {
    pub path: PathBuf,
    pub mapping: NotesMapping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotesSettings {
    pub default_mapping: NotesMapping,
    /// Whether a recognised metadata contract may set the mapping. It never
    /// overrides a mapping the user chose for that document.
    pub honour_metadata_contract: bool,
    /// Whether a doubled page may set a split mapping by itself.
    ///
    /// On by default: the doubled page is a fact stated by the file, and the
    /// presenter who opens a beamer deck five minutes before a talk should not
    /// have to find a menu. It is announced when it fires, it can be swapped
    /// or replaced in one press, and it yields to both an explicit choice and
    /// a metadata contract.
    pub detect_split: bool,
    pub per_document: Vec<DocumentMapping>,
}

impl Default for NotesSettings {
    fn default() -> Self {
        Self {
            default_mapping: NotesMapping::default(),
            honour_metadata_contract: false,
            detect_split: true,
            per_document: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimerSettings {
    /// How long the talk is, in seconds. Seconds because the menu now takes
    /// them: a target typed as 5:30 must come back as 5:30 next launch.
    pub target_seconds: Option<u64>,
    /// What the same setting was called when it could only hold whole minutes.
    /// Read once, to carry an existing settings file across, and never written.
    #[serde(skip_serializing)]
    pub target_minutes: Option<u64>,
    /// Count down towards that target rather than up from zero.
    ///
    /// Here rather than in the layout for the same reason the alarms are: a
    /// layout is reused next month, "twenty-five minutes, counting down" is
    /// this talk.
    pub count_down: bool,
    /// Start the clock on the first navigation rather than at launch.
    pub start_on_first_navigation: bool,
    /// Wall-clock cues, as seconds since local midnight.
    ///
    /// Kept here rather than in the layout because they belong to the talk:
    /// a layout is reused next month, "14:20, handoff" is not.
    pub alarms: Vec<crate::widgets::Alarm>,
    /// How long "snooze" puts a cue off for, in minutes. One number for both
    /// the alarms and the timer: a presenter who wants five more minutes wants
    /// five more minutes, whichever thing just told them they were out.
    pub snooze_minutes: u32,
}

impl TimerSettings {
    /// The saved target, whichever of the two spellings the file on disk uses.
    /// A file written before the menu took seconds still says minutes, and a
    /// presenter should not lose their length to a rename.
    pub fn target(&self) -> Option<u32> {
        self.target_seconds
            .or(self.target_minutes.map(|minutes| minutes * 60))
            .map(|seconds| seconds as u32)
    }
}

impl Default for TimerSettings {
    fn default() -> Self {
        Self {
            target_seconds: None,
            target_minutes: None,
            count_down: false,
            start_on_first_navigation: true,
            alarms: Vec::new(),
            snooze_minutes: crate::widgets::timing::model::DEFAULT_SNOOZE_MINUTES,
        }
    }
}

/// Unattended page turning: the kiosk case.
///
/// Named for what it does rather than for "slideshow", which in this codebase
/// already means the presenter's deck. Autoadvance is neither a mode nor a
/// layout: it turns pages in whichever viewer is up, in whatever the viewer
/// has open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoadvanceSettings {
    /// How long each page stays up, in seconds. Seconds rather than
    /// milliseconds because that is the unit the reader thinks in, and stored
    /// rather than derived so `1:30` comes back as `1:30` next launch — the
    /// same reason [`TimerSettings::target_seconds`] is what it is.
    pub interval_seconds: u64,
    /// Wrap to the first page at the end rather than stopping there. A lobby
    /// loop wants to wrap; a talk left running does not.
    pub wrap_at_end: bool,
    /// A page turn, a zoom or a mark by hand holds the loop rather than
    /// fighting the reader for control. Held until it is started again.
    pub pause_on_interaction: bool,
}

impl AutoadvanceSettings {
    /// The dwell, floored by the domain rather than by this file: a settings
    /// document edited by hand can say anything.
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_seconds)
            .max(pulpit_core::autoadvance::MIN_INTERVAL)
    }
}

impl Default for AutoadvanceSettings {
    fn default() -> Self {
        Self {
            interval_seconds: pulpit_core::autoadvance::DEFAULT_INTERVAL.as_secs(),
            wrap_at_end: true,
            pause_on_interaction: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// Light, dark, or follow the system.
    pub appearance: crate::platform::Appearance,
    /// User overrides for the two ordinary palettes. High contrast remains
    /// controlled by the operating system and is deliberately not editable.
    pub colors: ColorSettings,
    /// Whether pulpit keeps its own motion down. Follows the desktop by
    /// default; an explicit choice here wins, because someone who reached
    /// for it meant this application in particular.
    pub motion: crate::platform::MotionSetting,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        AppearanceSettings {
            appearance: crate::platform::Appearance::System,
            colors: ColorSettings::default(),
            motion: crate::platform::MotionSetting::default(),
        }
    }
}

/// Which half of the theme is being edited. This is transient UI state, not
/// another appearance preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorScheme {
    Light,
    Dark,
}

impl ColorScheme {
    pub const ALL: [ColorScheme; 2] = [ColorScheme::Light, ColorScheme::Dark];
}

impl std::fmt::Display for ColorScheme {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            ColorScheme::Light => "Light",
            ColorScheme::Dark => "Dark",
        })
    }
}

/// Sparse overrides keep the built-in Pulpit theme authoritative. A new
/// role introduced in a future release therefore receives its new default,
/// and resetting is exactly `clear()` rather than a copied palette.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ColorSettings {
    pub light: std::collections::BTreeMap<crate::theme::ColorRole, String>,
    pub dark: std::collections::BTreeMap<crate::theme::ColorRole, String>,
}

impl ColorSettings {
    fn overrides(
        &self,
        scheme: ColorScheme,
    ) -> &std::collections::BTreeMap<crate::theme::ColorRole, String> {
        match scheme {
            ColorScheme::Light => &self.light,
            ColorScheme::Dark => &self.dark,
        }
    }

    fn overrides_mut(
        &mut self,
        scheme: ColorScheme,
    ) -> &mut std::collections::BTreeMap<crate::theme::ColorRole, String> {
        match scheme {
            ColorScheme::Light => &mut self.light,
            ColorScheme::Dark => &mut self.dark,
        }
    }

    pub fn palette(&self, scheme: ColorScheme) -> crate::theme::Palette {
        let mut palette = match scheme {
            ColorScheme::Light => crate::theme::tokens::LIGHT,
            ColorScheme::Dark => crate::theme::tokens::DARK,
        };
        for (&role, value) in self.overrides(scheme) {
            if let Some(color) = parse_hex_color(value) {
                palette = palette.with(role, color);
            }
        }
        palette
    }

    pub fn value(&self, scheme: ColorScheme, role: crate::theme::ColorRole) -> String {
        self.overrides(scheme)
            .get(&role)
            .cloned()
            .unwrap_or_else(|| format_hex_color(self.default_palette(scheme).color(role)))
    }

    pub fn set(&mut self, scheme: ColorScheme, role: crate::theme::ColorRole, value: String) {
        let normalized = value.trim().to_ascii_uppercase();
        let default = format_hex_color(self.default_palette(scheme).color(role));
        let overrides = self.overrides_mut(scheme);
        if normalized == default {
            overrides.remove(&role);
        } else {
            overrides.insert(role, normalized);
        }
    }

    pub fn has_overrides(&self) -> bool {
        !self.light.is_empty() || !self.dark.is_empty()
    }

    pub fn reset(&mut self) {
        self.light.clear();
        self.dark.clear();
    }

    fn default_palette(&self, scheme: ColorScheme) -> crate::theme::Palette {
        match scheme {
            ColorScheme::Light => crate::theme::tokens::LIGHT,
            ColorScheme::Dark => crate::theme::tokens::DARK,
        }
    }
}

pub fn parse_hex_color(value: &str) -> Option<iced::Color> {
    let hex = value.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |offset| u8::from_str_radix(&hex[offset..offset + 2], 16).ok();
    Some(iced::Color::from_rgb8(
        channel(0)?,
        channel(2)?,
        channel(4)?,
    ))
}

pub fn format_hex_color(color: iced::Color) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsSettings {
    /// Write a rotating log file suitable for display bug reports.
    pub persistent_log: bool,
    pub level: String,
}

impl Default for DiagnosticsSettings {
    fn default() -> Self {
        Self {
            persistent_log: true,
            level: "info".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorRole;

    #[test]
    fn a_settings_file_written_before_autoadvance_still_loads() {
        // The whole block is absent, which is what every file on disk today
        // looks like. It must come back as the defaults rather than as a
        // parse failure that costs the reader every other setting they have.
        let settings: Settings = toml::from_str("schema = 1\n").expect("older files still load");
        assert_eq!(
            settings.autoadvance.interval_seconds,
            pulpit_core::autoadvance::DEFAULT_INTERVAL.as_secs()
        );
        assert!(settings.autoadvance.wrap_at_end);
        assert!(settings.autoadvance.pause_on_interaction);
    }

    #[test]
    fn a_dwell_edited_to_nonsense_by_hand_is_floored() {
        let settings: Settings = toml::from_str("[autoadvance]\ninterval_seconds = 0\n")
            .expect("a hand-edited file still loads");
        assert_eq!(
            settings.autoadvance.interval(),
            pulpit_core::autoadvance::MIN_INTERVAL,
            "a zero-second dwell is a strobe, not a setting"
        );
    }

    fn position(hash: Option<&str>, path: &str, page: usize) -> ReadingPosition {
        ReadingPosition {
            hash: hash.map(str::to_string),
            path: PathBuf::from(path),
            page,
            zoom: StoredZoom::FitPage,
            fraction: 0.5,
            outline_open: false,
            search_open: false,
        }
    }

    #[test]
    fn the_same_bytes_get_the_whole_position_back() {
        let mut reading = ReadingSettings::default();
        reading.remember_position(position(Some("abc"), "/papers/draft.pdf", 8));

        assert_eq!(
            reading.position_for(Some("abc"), std::path::Path::new("/papers/draft.pdf")),
            Some(RestoredPosition::Exact {
                page: 8,
                zoom: StoredZoom::FitPage,
                fraction: 0.5,
                outline_open: false,
                search_open: false,
            })
        );
    }

    #[test]
    fn the_sidebar_state_comes_back_with_the_rest_of_an_exact_match() {
        let mut reading = ReadingSettings::default();
        let mut open = position(Some("abc"), "/papers/draft.pdf", 8);
        open.outline_open = true;
        open.search_open = true;
        reading.remember_position(open);

        assert_eq!(
            reading.position_for(Some("abc"), std::path::Path::new("/papers/draft.pdf")),
            Some(RestoredPosition::Exact {
                page: 8,
                zoom: StoredZoom::FitPage,
                fraction: 0.5,
                outline_open: true,
                search_open: true,
            })
        );
    }

    #[test]
    fn a_moved_file_is_found_by_its_contents_and_not_by_where_it_was() {
        let mut reading = ReadingSettings::default();
        reading.remember_position(position(Some("abc"), "/papers/draft.pdf", 8));

        // The content hash outranks the path, which is the whole reason it is
        // the primary key: the same document under a new name is the same
        // document.
        assert!(matches!(
            reading.position_for(Some("abc"), std::path::Path::new("/elsewhere/final.pdf")),
            Some(RestoredPosition::Exact { page: 8, .. })
        ));
    }

    #[test]
    fn a_recompiled_document_keeps_its_page_and_nothing_else() {
        let mut reading = ReadingSettings::default();
        reading.remember_position(position(Some("abc"), "/papers/draft.pdf", 8));

        // Same path, different bytes: `make` ran. The page survives; the
        // fraction into it and the zoom do not, because the text under them
        // has moved.
        assert_eq!(
            reading.position_for(Some("def"), std::path::Path::new("/papers/draft.pdf")),
            Some(RestoredPosition::PageOnly {
                page: 8,
                outline_open: false,
                search_open: false,
            })
        );
    }

    #[test]
    fn a_recompiled_document_keeps_the_sidebar_unlike_the_fraction_and_zoom() {
        let mut reading = ReadingSettings::default();
        let mut open = position(Some("abc"), "/papers/draft.pdf", 8);
        open.outline_open = true;
        open.search_open = true;
        reading.remember_position(open);

        // The rail is a UI choice, not a coordinate into text that may have
        // moved, so it comes back even though the fraction and zoom do not.
        assert_eq!(
            reading.position_for(Some("def"), std::path::Path::new("/papers/draft.pdf")),
            Some(RestoredPosition::PageOnly {
                page: 8,
                outline_open: true,
                search_open: true,
            })
        );
    }

    #[test]
    fn an_unreadable_file_borrows_nobody_elses_position() {
        let mut reading = ReadingSettings::default();
        reading.remember_position(position(Some("abc"), "/papers/draft.pdf", 8));

        // No hash is the absence of evidence, not evidence of sameness — but
        // the path is still a path, and still the weaker answer.
        assert_eq!(
            reading.position_for(None, std::path::Path::new("/papers/draft.pdf")),
            Some(RestoredPosition::PageOnly {
                page: 8,
                outline_open: false,
                search_open: false,
            })
        );
        assert_eq!(
            reading.position_for(None, std::path::Path::new("/papers/other.pdf")),
            None
        );
    }

    #[test]
    fn rebuilding_a_paper_all_afternoon_leaves_one_entry_not_a_hundred() {
        let mut reading = ReadingSettings::default();
        for build in 0..50 {
            reading.remember_position(position(
                Some(&format!("hash-{build}")),
                "/papers/draft.pdf",
                build,
            ));
        }
        // Every build is a new hash at the same path, and the path match is
        // what keeps them from accumulating and evicting every other document.
        assert_eq!(reading.positions.len(), 1);
        assert_eq!(reading.positions[0].page, 49);
    }

    #[test]
    fn the_position_list_is_bounded_and_drops_the_least_recent() {
        let mut reading = ReadingSettings::default();
        for document in 0..MAX_REMEMBERED_POSITIONS + 10 {
            reading.remember_position(position(
                Some(&format!("hash-{document}")),
                &format!("/papers/{document}.pdf"),
                document,
            ));
        }
        assert_eq!(reading.positions.len(), MAX_REMEMBERED_POSITIONS);
        // The first ten opened are the ten that went.
        assert_eq!(
            reading.position_for(Some("hash-0"), std::path::Path::new("/papers/0.pdf")),
            None
        );
        assert!(reading
            .position_for(Some("hash-10"), std::path::Path::new("/papers/10.pdf"))
            .is_some());
    }

    #[test]
    fn positions_survive_a_round_trip_through_the_settings_file() {
        let mut settings = Settings::default();
        settings
            .reading
            .remember_position(position(Some("abc"), "/papers/draft.pdf", 8));
        let written = toml::to_string_pretty(&settings).expect("settings serialise");
        let read: Settings = toml::from_str(&written).expect("settings parse");
        assert_eq!(read.reading, settings.reading);
    }

    #[test]
    fn a_settings_file_from_before_reading_positions_still_loads() {
        // The field is `serde(default)` like every other, so an older file is
        // read as one that has simply never remembered a position.
        let read: Settings = toml::from_str("schema = 2\n").expect("settings parse");
        assert_eq!(read.reading, ReadingSettings::default());
        assert!(read.reading.remember);
    }

    #[test]
    fn a_position_recorded_before_the_sidebar_had_memory_reads_as_closed() {
        // A record written by a pulpit that predates `outline_open` and
        // `search_open` has neither key in its TOML table; `serde(default)`
        // reads that as `false` for both, which is exactly the wanted
        // "never seen a sidebar preference for this file" answer.
        let toml = r#"
            schema = 2

            [[reading.positions]]
            hash = "abc"
            path = "/papers/draft.pdf"
            page = 8
            zoom = "fit-page"
            fraction = 0.5
        "#;
        let read: Settings = toml::from_str(toml).expect("settings parse");
        let entry = &read.reading.positions[0];
        assert!(!entry.outline_open);
        assert!(!entry.search_open);
    }

    #[test]
    fn an_open_sidebar_survives_a_round_trip_through_the_settings_file() {
        let mut settings = Settings::default();
        let mut open = position(Some("abc"), "/papers/draft.pdf", 8);
        open.outline_open = true;
        open.search_open = true;
        settings.reading.remember_position(open);
        let written = toml::to_string_pretty(&settings).expect("settings serialise");
        let read: Settings = toml::from_str(&written).expect("settings parse");
        assert_eq!(read.reading, settings.reading);
        assert!(read.reading.positions[0].outline_open);
        assert!(read.reading.positions[0].search_open);
    }

    #[test]
    fn a_files_remembered_layout_is_found_by_its_contents_and_replaced_in_place() {
        let mut layout = LayoutSettings::default();
        assert_eq!(layout.layout_for_document("abc"), None);

        layout.remember_layout_for_document("abc".into(), "presenter-default".into());
        assert_eq!(layout.layout_for_document("abc"), Some("presenter-default"));

        // Changing one's mind replaces the answer rather than adding a second.
        layout.remember_layout_for_document("abc".into(), "reader-default".into());
        assert_eq!(layout.layout_for_document("abc"), Some("reader-default"));
        assert_eq!(layout.per_document.len(), 1);
    }

    #[test]
    fn the_remembered_layouts_are_bounded_and_drop_the_least_recent_first() {
        let mut layout = LayoutSettings::default();
        for i in 0..MAX_REMEMBERED_LAYOUTS + 10 {
            layout.remember_layout_for_document(format!("hash-{i}"), "reader-default".into());
        }
        assert_eq!(layout.per_document.len(), MAX_REMEMBERED_LAYOUTS);
        // The first ten are gone and the most recent is at the front.
        assert_eq!(layout.layout_for_document("hash-0"), None);
        assert_eq!(
            layout.per_document.front().unwrap().hash,
            format!("hash-{}", MAX_REMEMBERED_LAYOUTS + 9)
        );
    }

    /// Saving writes a second file, which hashes differently. The copy should
    /// open the way the original did — and the original, which pulpit never
    /// overwrites, should keep its own answer.
    #[test]
    fn a_saved_copy_inherits_the_choice_and_the_original_keeps_it() {
        let mut layout = LayoutSettings::default();
        layout.remember_layout_for_document("source".into(), "presenter-default".into());
        layout.carry_layout_to_saved_copy("source", "annotated".into());

        assert_eq!(
            layout.layout_for_document("annotated"),
            Some("presenter-default")
        );
        assert_eq!(
            layout.layout_for_document("source"),
            Some("presenter-default")
        );
    }

    /// A file nobody chose a layout for hands its copy nothing, so the copy
    /// opens in the Reader like any other file opened for the first time.
    #[test]
    fn a_copy_of_an_unremembered_file_stays_unremembered() {
        let mut layout = LayoutSettings::default();
        layout.carry_layout_to_saved_copy("source", "annotated".into());
        assert!(layout.per_document.is_empty());
    }

    #[test]
    fn recent_documents_are_deduplicated_and_bounded() {
        let mut settings = Settings::default();
        for i in 0..20 {
            settings.remember_recent(PathBuf::from(format!("/decks/{i}.pdf")));
        }
        settings.remember_recent(PathBuf::from("/decks/19.pdf"));
        assert_eq!(settings.recent.len(), MAX_RECENT);
        assert_eq!(
            settings.recent.front().unwrap(),
            &PathBuf::from("/decks/19.pdf")
        );
        assert_eq!(
            settings
                .recent
                .iter()
                .filter(|p| p.ends_with("19.pdf"))
                .count(),
            1,
            "no duplicates"
        );
    }

    #[test]
    fn per_document_mappings_win_over_the_default() {
        let mut settings = Settings::default();
        settings.notes.default_mapping = NotesMapping::SlidesOnly;
        let path = PathBuf::from("/decks/talk.pdf");
        settings.remember_mapping(
            path.clone(),
            NotesMapping::PairedPages(pulpit_core::PairedRule::Alternating { notes_first: false }),
        );
        assert!(settings.mapping_for(&path).has_notes());
        assert!(!settings
            .mapping_for(std::path::Path::new("/decks/other.pdf"))
            .has_notes());
    }

    #[test]
    fn color_overrides_are_sparse_and_reset_together() {
        let mut colors = ColorSettings::default();
        colors.set(ColorScheme::Dark, ColorRole::Accent, "#123456".into());
        assert_eq!(
            format_hex_color(colors.palette(ColorScheme::Dark).accent),
            "#123456"
        );
        assert!(colors.light.is_empty());
        assert!(colors.has_overrides());

        colors.reset();
        assert!(!colors.has_overrides());
        assert_eq!(
            colors.palette(ColorScheme::Dark),
            crate::theme::tokens::DARK
        );
        assert_eq!(
            colors.palette(ColorScheme::Light),
            crate::theme::tokens::LIGHT
        );
    }

    #[test]
    fn hex_colors_round_trip_and_invalid_drafts_do_not_change_the_palette() {
        let color = parse_hex_color("#3dd6ef").expect("valid HEX");
        assert_eq!(format_hex_color(color), "#3DD6EF");
        assert!(parse_hex_color("3DD6EF").is_none());
        assert!(parse_hex_color("#XYZXYZ").is_none());

        let mut colors = ColorSettings::default();
        colors.set(ColorScheme::Dark, ColorRole::Accent, "#123".into());
        assert_eq!(
            colors.palette(ColorScheme::Dark).accent,
            crate::theme::tokens::DARK.accent
        );
        assert_eq!(colors.value(ColorScheme::Dark, ColorRole::Accent), "#123");
    }

    #[test]
    fn blanking_defaults_to_black_which_is_what_a_dark_hall_wants() {
        assert_eq!(DisplaySettings::default().blank_color, BlankColor::Black);
        assert_eq!(BlankColor::Black.blank(), pulpit_core::Blank::Black);
        assert_eq!(BlankColor::White.blank(), pulpit_core::Blank::White);
    }

    #[test]
    fn a_settings_file_written_before_the_blank_color_existed_still_loads() {
        // `#[serde(default)]` is what makes an older file forward-compatible;
        // losing it would make every stored settings file unreadable.
        let older = r#"{ "settle_ms": 400 }"#;
        let settings: DisplaySettings = serde_json::from_str(older).expect("should load");
        assert_eq!(settings.settle_ms, 400);
        assert_eq!(settings.blank_color, BlankColor::Black);
    }

    #[test]
    fn a_keymap_stored_before_the_setting_existed_keeps_working() {
        use crate::settings::keys::{Action, Keymap};
        // `b` used to mean "blank black" outright. Loading that keymap must
        // give the presenter the blank key, not an unbound `b`. `w` named the
        // second blanking key, which no longer exists: that binding goes, and
        // the file still loads.
        let older = r#"{"bindings":[[{"kind":"named","key":"b"},"blank-black"],
                                    [{"kind":"named","key":"w"},"blank-white"]]}"#;
        let keymap: Keymap = serde_json::from_str(older).expect("should load");
        assert_eq!(keymap.resolve(Some("b"), None), Some(Action::Blank));
        assert_eq!(keymap.resolve(Some("w"), None), None);
    }

    #[test]
    fn the_blank_color_round_trips_through_settings() {
        let settings = DisplaySettings {
            blank_color: BlankColor::White,
            ..Default::default()
        };
        let encoded = serde_json::to_string(&settings).unwrap();
        let decoded: DisplaySettings = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.blank_color, BlankColor::White);
        assert!(
            encoded.contains("white"),
            "the stored form is the readable one: {encoded}"
        );
    }

    #[test]
    fn setting_a_default_color_removes_the_override() {
        let mut colors = ColorSettings::default();
        colors.set(ColorScheme::Light, ColorRole::Text, "#123456".into());
        colors.set(
            ColorScheme::Light,
            ColorRole::Text,
            format_hex_color(crate::theme::tokens::LIGHT.text),
        );
        assert!(!colors.has_overrides());
    }
}
