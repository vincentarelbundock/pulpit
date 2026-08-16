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
#![allow(dead_code)]

pub mod diagnostics;
pub mod keys;
pub mod store;

pub use diagnostics::DiagnosticsBundle;
pub use keys::{Action, KeyBinding, Keymap};
pub use store::{load_or_default, SettingsStore};

use std::collections::VecDeque;
use std::path::PathBuf;

use pulpit_core::NotesMapping;
use pulpit_display::DisplayRoles;
use serde::{Deserialize, Serialize};

/// Current settings schema version. Bump it *and* add a migration.
pub const SCHEMA_VERSION: u32 = 2;

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
    pub keymap: Keymap,
    pub recent: VecDeque<PathBuf>,
    pub diagnostics: DiagnosticsSettings,
    pub layout: LayoutSettings,
    pub appearance: AppearanceSettings,
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
    /// The one-time "review this layout at your screen ratio" notice.
    pub ratio_notice_dismissed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            display: DisplaySettings::default(),
            rendering: RenderSettings::default(),
            notes: NotesSettings::default(),
            timer: TimerSettings::default(),
            keymap: Keymap::default(),
            recent: VecDeque::new(),
            diagnostics: DiagnosticsSettings::default(),
            layout: LayoutSettings::default(),
            appearance: AppearanceSettings::default(),
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
    /// What the primary blank key turns the audience screen into.
    ///
    /// Both colours remain available as explicit actions; this only decides
    /// which one the single most-reached-for key produces. Black disappears
    /// in a dark room, white is the one that reads as deliberate under bright
    /// house lights, and which is wanted is a property of the venue rather
    /// than of the deck.
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
    pub const ALL: [BlankColor; 2] = [BlankColor::Black, BlankColor::White];

    pub fn label(self) -> &'static str {
        match self {
            BlankColor::Black => "Black",
            BlankColor::White => "White",
        }
    }

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
    fn the_alternate_blank_is_always_the_colour_the_primary_is_not() {
        // The pairing is what lets one setting control both keys without a
        // second setting that could contradict it.
        for primary in BlankColor::ALL {
            let alternate = BlankColor::ALL
                .into_iter()
                .find(|other| *other != primary)
                .expect("there are exactly two colours");
            assert_ne!(primary.blank(), alternate.blank());
        }
    }

    #[test]
    fn a_keymap_stored_before_the_setting_existed_keeps_working() {
        use crate::settings::keys::{Action, Keymap};
        // `b` used to mean "blank black" outright. Loading that keymap must
        // give the presenter the primary blank key, not an unbound `b`.
        let older = r#"{"bindings":[[{"kind":"named","key":"b"},"blank-black"],
                                    [{"kind":"named","key":"w"},"blank-white"]]}"#;
        let keymap: Keymap = serde_json::from_str(older).expect("should load");
        assert_eq!(keymap.resolve(Some("b"), None), Some(Action::Blank));
        assert_eq!(
            keymap.resolve(Some("w"), None),
            Some(Action::BlankAlternate)
        );
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
