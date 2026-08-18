//! The layout library: built-ins, custom layouts on disk, and the export
//! and import format.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::builtin::built_in_layouts;
use crate::layout::model::{AspectRatio, Layout, LayoutId, Origin};
use crate::layout::tree::{Frame, Node};
use crate::layout::validate::{is_blocked, validate, Issue};
use crate::widgets::WidgetKind;

/// Bumped whenever the exported shape changes. Import refuses anything newer
/// and migrates anything older.
///
/// Version 2 identifies a placed widget by its stable [`crate::widgets::registry::WidgetId`]
/// (`widget_id`, e.g. `"pulpit.document.page"`) rather than by the Rust-side
/// `kind` tag, so a saved layout survives a variant being renamed and a
/// widget id this build does not know is preserved rather than refused. See
/// [`normalise_for_import`] and [`normalise_for_export`].
pub const FORMAT_VERSION: u32 = 2;

/// The validation area used when a layout is checked outside the editor.
pub const REFERENCE_AREA: Frame = Frame {
    x: 0.0,
    y: 0.0,
    width: 1600.0,
    height: 900.0,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot read layout: {0}")]
    Parse(String),
    #[error(
        "this layout was written by a newer version of pulpit (format {found}, this build \
         understands {known}). Update pulpit to open it."
    )]
    FutureFormat { found: u32, known: u32 },
    #[error("the layout is not usable: {0}")]
    Invalid(String),
    #[error("built-in layouts cannot be {0}")]
    BuiltIn(&'static str),
    #[error("no layout {0}")]
    NoSuchLayout(LayoutId),
    #[error("a layout named {0} already exists")]
    NameTaken(String),
}

/// The exported file: a format version, the name, the design ratio and the
/// tree with every property.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutFile {
    pub format_version: u32,
    pub name: String,
    #[serde(default)]
    pub design_ratio: AspectRatio,
    pub root: Node,
}

impl LayoutFile {
    pub fn from_layout(layout: &Layout) -> LayoutFile {
        LayoutFile {
            format_version: FORMAT_VERSION,
            name: layout.name.clone(),
            design_ratio: layout.design_ratio,
            root: layout.root.clone(),
        }
    }

    /// An imported layout is always custom, even when exported from a
    /// built-in.
    pub fn into_layout(self) -> Layout {
        Layout::from_parts(
            LayoutId::from_name(&self.name),
            self.name,
            Origin::Custom,
            self.design_ratio,
            self.root,
        )
    }
}

/// Serialise a layout for export. Always writes the current format: a
/// placed widget appears as `{ "widget_id": "...", "config": ..., "style":
/// ... }`, and an [`crate::layout::UnavailableWidget`] placeholder round-trips
/// under the same `widget` key, untouched.
pub fn export_to_string(layout: &Layout) -> Result<String, StoreError> {
    let mut value = serde_json::to_value(LayoutFile::from_layout(layout))
        .map_err(|e| StoreError::Parse(e.to_string()))?;
    if let Some(root) = value.get_mut("root") {
        normalise_for_export(root)?;
    }
    serde_json::to_string_pretty(&value).map_err(|e| StoreError::Parse(e.to_string()))
}

/// Parse, migrate and validate an exported layout.
///
/// An invalid structure blocks; ordinary validation warnings are returned
/// alongside the layout and do not block. A `widget_id` this build does not
/// recognise does not block either: the cell is kept as an
/// [`crate::layout::UnavailableWidget`] placeholder instead.
pub fn import_from_string(text: &str) -> Result<(Layout, Vec<Issue>), StoreError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| StoreError::Parse(e.to_string()))?;
    let found = value
        .get("format_version")
        .and_then(|version| version.as_u64())
        .unwrap_or(0) as u32;
    if found > FORMAT_VERSION {
        return Err(StoreError::FutureFormat {
            found,
            known: FORMAT_VERSION,
        });
    }
    let mut value = migrate(value, found)?;
    if let Some(root) = value.get_mut("root") {
        normalise_for_import(root)?;
    }

    // A malformed structure fails here, which is what we want: it is a
    // blocking condition, reported in the message. A widget id that is
    // merely unknown never reaches this deserialize as a `widget` field —
    // `normalise_for_import` has already turned it into `unavailable`.
    let file: LayoutFile = serde_json::from_value(value).map_err(|e| {
        StoreError::Parse(format!(
            "{e}. A widget or property in this file is not known to this version."
        ))
    })?;

    let layout = file.into_layout();
    let issues = validate(&layout, REFERENCE_AREA);
    if is_blocked(&issues) {
        let reasons: Vec<String> = issues
            .iter()
            .filter(|issue| issue.severity == crate::layout::validate::Severity::Blocking)
            .map(|issue| issue.message.clone())
            .collect();
        return Err(StoreError::Invalid(reasons.join("; ")));
    }
    Ok((layout, issues))
}

/// Bring an older file forward. Each arm upgrades exactly one version.
fn migrate(mut value: serde_json::Value, from: u32) -> Result<serde_json::Value, StoreError> {
    let mut version = from;
    while version < FORMAT_VERSION {
        match version {
            // 0 is "written before the format was versioned": the shape is
            // the same, the field was simply absent.
            0 => {}
            // 1 named a placed widget by its Rust-side `kind` tag; 2 names
            // it by its stable id instead. The shape is otherwise the same.
            1 => {
                if let Some(root) = value.get_mut("root") {
                    migrate_kind_to_widget_id(root)?;
                }
            }
            other => {
                return Err(StoreError::Parse(format!(
                    "no migration from layout format {other}"
                )))
            }
        }
        version += 1;
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "format_version".into(),
            serde_json::Value::from(FORMAT_VERSION),
        );
    }
    Ok(value)
}

fn node_object(
    value: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>, StoreError> {
    value
        .as_object_mut()
        .ok_or_else(|| StoreError::Parse("a layout node must be an object".into()))
}

fn each_child(
    object: &mut serde_json::Map<String, serde_json::Value>,
    mut f: impl FnMut(&mut serde_json::Value) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if let Some(children) = object.get_mut("children").and_then(|c| c.as_array_mut()) {
        for child in children {
            f(child)?;
        }
    }
    Ok(())
}

/// Version 1 → 2: rename a placed widget's `kind` tag to its stable
/// `widget_id`. An unrecognised kind blocks here, same as version 1 always
/// blocked on one — version 2's tolerance is for ids this build does not
/// know, which cannot occur yet in a version-1 file written by this crate.
fn migrate_kind_to_widget_id(node: &mut serde_json::Value) -> Result<(), StoreError> {
    let object = node_object(node)?;
    if let Some(widget) = object.get_mut("widget") {
        if !widget.is_null() {
            let widget = widget
                .as_object_mut()
                .ok_or_else(|| StoreError::Parse("a widget must be an object".into()))?;
            if let Some(kind_value) = widget.remove("kind") {
                let kind: WidgetKind = serde_json::from_value(kind_value).map_err(|e| {
                    StoreError::Parse(format!("unknown widget kind in a version-1 file: {e}"))
                })?;
                widget.insert(
                    "widget_id".into(),
                    serde_json::Value::String(kind.id().as_str().to_string()),
                );
            }
        }
    }
    each_child(object, migrate_kind_to_widget_id)
}

/// Version 2 wire shape → the shape [`LayoutFile`] deserializes: a known
/// `widget_id` becomes an ordinary `kind`-tagged `widget`; an unknown one is
/// moved to `unavailable`, config and style untouched, so it survives
/// instead of failing the whole file.
fn normalise_for_import(node: &mut serde_json::Value) -> Result<(), StoreError> {
    let object = node_object(node)?;
    if let Some(mut widget) = object.remove("widget") {
        if widget.is_null() {
            object.insert("widget".into(), widget);
        } else {
            let fields = widget
                .as_object_mut()
                .ok_or_else(|| StoreError::Parse("a widget must be an object".into()))?;
            let widget_id = fields
                .get("widget_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| StoreError::Parse("a widget is missing widget_id".into()))?
                .to_string();
            match WidgetKind::from_id(&widget_id) {
                Some(kind) => {
                    fields.remove("widget_id");
                    fields.insert(
                        "kind".into(),
                        serde_json::to_value(kind).map_err(|e| StoreError::Parse(e.to_string()))?,
                    );
                    object.insert("widget".into(), widget);
                }
                None => {
                    let config = fields.remove("config").unwrap_or(serde_json::Value::Null);
                    let style = fields.remove("style").unwrap_or(serde_json::Value::Null);
                    let mut unavailable = serde_json::Map::new();
                    unavailable.insert("widget_id".into(), serde_json::Value::String(widget_id));
                    unavailable.insert("config".into(), config);
                    unavailable.insert("style".into(), style);
                    object.insert("unavailable".into(), serde_json::Value::Object(unavailable));
                }
            }
        }
    }
    each_child(object, normalise_for_import)
}

/// The reverse of [`normalise_for_import`]: a `kind`-tagged `widget` becomes
/// `widget_id`-tagged, and an `unavailable` placeholder is promoted back to
/// `widget` — its `widget_id`, `config` and `style` are already in the wire
/// shape, since that is exactly what was preserved on the way in.
fn normalise_for_export(node: &mut serde_json::Value) -> Result<(), StoreError> {
    let object = node_object(node)?;
    if let Some(widget) = object.get_mut("widget") {
        if !widget.is_null() {
            let fields = widget
                .as_object_mut()
                .ok_or_else(|| StoreError::Parse("a widget must be an object".into()))?;
            if let Some(kind_value) = fields.remove("kind") {
                let kind: WidgetKind = serde_json::from_value(kind_value)
                    .map_err(|e| StoreError::Parse(e.to_string()))?;
                fields.insert(
                    "widget_id".into(),
                    serde_json::Value::String(kind.id().as_str().to_string()),
                );
            }
        }
    }
    if let Some(unavailable) = object.remove("unavailable") {
        if !unavailable.is_null() {
            object.insert("widget".into(), unavailable);
        }
    }
    each_child(object, normalise_for_export)
}

/// The layout library: the two built-ins plus the user's own.
#[derive(Debug, Clone)]
pub struct LayoutStore {
    directory: PathBuf,
    built_in: Vec<Layout>,
    custom: Vec<Layout>,
}

impl LayoutStore {
    /// Load custom layouts from `directory`, creating nothing.
    pub fn load(directory: impl Into<PathBuf>) -> LayoutStore {
        let directory = directory.into();
        let mut custom = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "json") {
                    continue;
                }
                match std::fs::read_to_string(&path)
                    .map_err(StoreError::from)
                    .and_then(|text| import_from_string(&text).map(|(layout, _)| layout))
                {
                    Ok(mut layout) => {
                        // The file name is the identity on disk.
                        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                            layout.id = LayoutId(stem.to_string());
                        }
                        custom.push(layout);
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "skipping unreadable layout")
                    }
                }
            }
        }
        custom.sort_by_key(|layout| layout.name.to_lowercase());
        LayoutStore {
            directory,
            built_in: built_in_layouts(),
            custom,
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn built_in(&self) -> &[Layout] {
        &self.built_in
    }

    pub fn custom(&self) -> &[Layout] {
        &self.custom
    }

    pub fn all(&self) -> impl Iterator<Item = &Layout> {
        self.built_in.iter().chain(self.custom.iter())
    }

    pub fn get(&self, id: &LayoutId) -> Option<&Layout> {
        self.all().find(|layout| &layout.id == id)
    }

    pub fn contains_name(&self, name: &str) -> bool {
        self.all()
            .any(|layout| layout.name.eq_ignore_ascii_case(name))
    }

    /// A name that is not taken, by appending a numeric suffix.
    pub fn unique_name(&self, wanted: &str) -> String {
        if !self.contains_name(wanted) {
            return wanted.to_string();
        }
        for suffix in 2..1000 {
            let candidate = format!("{wanted} {suffix}");
            if !self.contains_name(&candidate) {
                return candidate;
            }
        }
        format!("{wanted} {}", self.all().count() + 1)
    }

    fn unique_id(&self, wanted: LayoutId) -> LayoutId {
        if self.get(&wanted).is_none() {
            return wanted;
        }
        for suffix in 2..1000 {
            let candidate = LayoutId(format!("{}-{suffix}", wanted.0));
            if self.get(&candidate).is_none() {
                return candidate;
            }
        }
        LayoutId(format!("{}-{}", wanted.0, self.all().count() + 1))
    }

    fn path_for(&self, id: &LayoutId) -> PathBuf {
        self.directory.join(format!("{}.json", id.0))
    }

    /// Write a custom layout to disk, replacing any previous version.
    pub fn save(&mut self, layout: &Layout) -> Result<LayoutId, StoreError> {
        if layout.origin == Origin::BuiltIn {
            return Err(StoreError::BuiltIn("overwritten"));
        }
        let mut layout = layout.clone();
        if layout.name.trim().is_empty() {
            return Err(StoreError::Invalid("a layout needs a name".into()));
        }
        // A new layout gets a free id; an existing one keeps its own.
        if self.get(&layout.id).is_none() {
            layout.id = self.unique_id(LayoutId::from_name(&layout.name));
        }

        std::fs::create_dir_all(&self.directory)?;
        let text = export_to_string(&layout)?;
        let path = self.path_for(&layout.id);
        // Same atomic dance as the settings store: a crash must leave either
        // the old file or the new one.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text.as_bytes())?;
        std::fs::rename(&temporary, &path)?;

        match self
            .custom
            .iter_mut()
            .find(|existing| existing.id == layout.id)
        {
            Some(existing) => *existing = layout.clone(),
            None => self.custom.push(layout.clone()),
        }
        self.custom.sort_by_key(|layout| layout.name.to_lowercase());
        Ok(layout.id)
    }

    /// Save under a new name, leaving the original alone.
    pub fn save_as_new(&mut self, layout: &Layout, name: &str) -> Result<LayoutId, StoreError> {
        if self.contains_name(name) {
            return Err(StoreError::NameTaken(name.to_string()));
        }
        let mut copy = layout.clone();
        copy.origin = Origin::Custom;
        copy.name = name.to_string();
        copy.id = self.unique_id(LayoutId::from_name(name));
        self.save(&copy)
    }

    /// Copy any layout — including a built-in — into an editable custom one.
    pub fn duplicate(&mut self, id: &LayoutId) -> Result<LayoutId, StoreError> {
        let source = self
            .get(id)
            .ok_or_else(|| StoreError::NoSuchLayout(id.clone()))?
            .clone();
        let name = self.unique_name(&format!("{} copy", source.name));
        let mut copy = source;
        copy.origin = Origin::Custom;
        copy.name = name.clone();
        copy.id = self.unique_id(LayoutId::from_name(&name));
        copy.renumber();
        self.save(&copy)
    }

    pub fn rename(&mut self, id: &LayoutId, name: &str) -> Result<(), StoreError> {
        if self
            .get(id)
            .is_some_and(|layout| layout.origin == Origin::BuiltIn)
        {
            return Err(StoreError::BuiltIn("renamed"));
        }
        if name.trim().is_empty() {
            return Err(StoreError::Invalid("a layout needs a name".into()));
        }
        if self
            .all()
            .any(|layout| &layout.id != id && layout.name.eq_ignore_ascii_case(name))
        {
            return Err(StoreError::NameTaken(name.to_string()));
        }
        let layout = self
            .custom
            .iter_mut()
            .find(|layout| &layout.id == id)
            .ok_or_else(|| StoreError::NoSuchLayout(id.clone()))?;
        layout.name = name.to_string();
        let updated = layout.clone();
        self.save(&updated)?;
        Ok(())
    }

    pub fn delete(&mut self, id: &LayoutId) -> Result<(), StoreError> {
        if self
            .get(id)
            .is_some_and(|layout| layout.origin == Origin::BuiltIn)
        {
            return Err(StoreError::BuiltIn("deleted"));
        }
        let position = self
            .custom
            .iter()
            .position(|layout| &layout.id == id)
            .ok_or_else(|| StoreError::NoSuchLayout(id.clone()))?;
        let _ = std::fs::remove_file(self.path_for(id));
        self.custom.remove(position);
        Ok(())
    }

    /// Import from JSON text, giving the layout a free name if its own is
    /// taken.
    pub fn import(&mut self, text: &str) -> Result<(LayoutId, Vec<Issue>), StoreError> {
        let (mut layout, issues) = import_from_string(text)?;
        layout.name = self.unique_name(&layout.name);
        layout.id = self.unique_id(LayoutId::from_name(&layout.name));
        let id = self.save(&layout)?;
        Ok((id, issues))
    }

    pub fn export(&self, id: &LayoutId) -> Result<String, StoreError> {
        let layout = self
            .get(id)
            .ok_or_else(|| StoreError::NoSuchLayout(id.clone()))?;
        export_to_string(layout)
    }

    /// The saved version of a custom layout, for "reset to last saved".
    pub fn saved_version(&self, id: &LayoutId) -> Option<&Layout> {
        self.custom.iter().find(|layout| &layout.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::builtin::presenter_default;
    use crate::layout::tree::Direction;
    use crate::widgets::{Widget, WidgetKind};

    fn store() -> (tempfile::TempDir, LayoutStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = LayoutStore::load(directory.path());
        (directory, store)
    }

    fn custom_layout(name: &str) -> Layout {
        let mut layout = Layout::empty(name);
        let root = layout.root.id();
        layout.split_cell(root, Direction::Vertical).unwrap();
        let ids: Vec<_> = layout.cells().iter().map(|cell| cell.id).collect();
        layout
            .set_widget(ids[0], Widget::new(WidgetKind::CurrentSlide))
            .unwrap();
        layout
            .set_widget(ids[1], Widget::new(WidgetKind::SlideButtons))
            .unwrap();
        layout
    }

    #[test]
    fn a_fresh_store_offers_the_built_ins_and_nothing_else() {
        let (_directory, store) = store();
        assert_eq!(store.built_in().len(), 2);
        assert!(store.custom().is_empty());
        assert!(store.get(&LayoutId("presenter-default".into())).is_some());
    }

    #[test]
    fn saving_and_reloading_round_trips_through_disk() {
        let (directory, mut store) = store();
        let id = store.save(&custom_layout("Conference")).unwrap();

        let reloaded = LayoutStore::load(directory.path());
        let layout = reloaded.get(&id).expect("saved layout is back");
        assert_eq!(layout.name, "Conference");
        assert_eq!(layout.origin, Origin::Custom);
        assert_eq!(layout.widgets().len(), 2);
        assert!(layout.is_canonical());
    }

    #[test]
    fn built_in_layouts_cannot_be_overwritten_renamed_or_deleted() {
        let (_directory, mut store) = store();
        let id = LayoutId("presenter-default".into());
        assert!(matches!(
            store.save(&presenter_default()),
            Err(StoreError::BuiltIn("overwritten"))
        ));
        assert!(matches!(
            store.rename(&id, "Mine"),
            Err(StoreError::BuiltIn("renamed"))
        ));
        assert!(matches!(
            store.delete(&id),
            Err(StoreError::BuiltIn("deleted"))
        ));
    }

    #[test]
    fn duplicating_a_built_in_produces_an_editable_copy() {
        let (_directory, mut store) = store();
        let id = store
            .duplicate(&LayoutId("presenter-default".into()))
            .unwrap();
        let copy = store.get(&id).unwrap();
        assert_eq!(copy.origin, Origin::Custom);
        assert!(copy.is_editable());
        assert_eq!(copy.name, "Presenter copy");
        assert_eq!(copy.widgets().len(), presenter_default().widgets().len());

        // Duplicating again does not collide.
        let second = store
            .duplicate(&LayoutId("presenter-default".into()))
            .unwrap();
        assert_ne!(id, second);
        assert_eq!(store.get(&second).unwrap().name, "Presenter copy 2");
    }

    #[test]
    fn renaming_and_deleting_custom_layouts() {
        let (directory, mut store) = store();
        let id = store.save(&custom_layout("Draft")).unwrap();

        store.rename(&id, "Keynote").unwrap();
        assert_eq!(store.get(&id).unwrap().name, "Keynote");

        store.save(&custom_layout("Other")).unwrap();
        assert!(matches!(
            store.rename(&id, "Other"),
            Err(StoreError::NameTaken(_))
        ));

        store.delete(&id).unwrap();
        assert!(store.get(&id).is_none());
        assert!(!directory.path().join(format!("{}.json", id.0)).exists());
    }

    #[test]
    fn export_and_import_round_trip() {
        let (_directory, mut store) = store();
        let id = store.save(&custom_layout("Portable")).unwrap();
        let text = store.export(&id).unwrap();
        assert!(text.contains("\"format_version\": 2"));
        assert!(text.contains("Portable"));

        let (imported_id, issues) = store.import(&text).unwrap();
        assert!(!issues
            .iter()
            .any(|issue| issue.severity == crate::layout::validate::Severity::Blocking));
        let imported = store.get(&imported_id).unwrap();
        assert_eq!(
            imported.name, "Portable 2",
            "a taken name gets a numeric suffix"
        );
        assert_eq!(imported.origin, Origin::Custom);
        assert_eq!(imported.widgets().len(), 2);
    }

    #[test]
    fn exporting_a_built_in_and_importing_it_yields_a_custom_layout() {
        let (_directory, mut store) = store();
        let text = store.export(&LayoutId("reader-default".into())).unwrap();
        let (id, _) = store.import(&text).unwrap();
        assert_eq!(store.get(&id).unwrap().origin, Origin::Custom);
        assert_eq!(store.built_in().len(), 2, "the built-in is untouched");
    }

    #[test]
    fn a_newer_format_is_refused_with_a_clear_message() {
        let (_directory, mut store) = store();
        let text = export_to_string(&custom_layout("Future")).unwrap();
        let bumped = text.replace("\"format_version\": 2", "\"format_version\": 99");
        match store.import(&bumped) {
            Err(StoreError::FutureFormat { found, known }) => {
                assert_eq!((found, known), (99, FORMAT_VERSION));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A version-1, `kind`-tagged file with no `format_version` at all: the
    /// oldest shape this crate ever wrote. Migration must bring both the
    /// missing version *and* the `kind` → `widget_id` rename forward in one
    /// pass.
    fn legacy_kind_shape(name: &str, kind: &str) -> String {
        format!(
            r#"{{
              "name": "{name}",
              "design_ratio": "sixteen-nine",
              "root": {{
                "type": "leaf",
                "id": 0,
                "widget": {{
                  "kind": "{kind}",
                  "style": {{"variant": "standard", "scale": 1.0, "alignment": "center"}},
                  "config": "none"
                }},
                "padding": 0.0,
                "background": "none",
                "border": "none",
                "empty_behavior": "show-blank-panel"
              }}
            }}"#
        )
    }

    #[test]
    fn a_missing_format_version_is_migrated_forward() {
        let (_directory, mut store) = store();
        let legacy = legacy_kind_shape("Legacy", "slide-slider");

        let (id, _) = store.import(&legacy).unwrap();
        let layout = store.get(&id).unwrap();
        assert_eq!(
            layout.name, "Legacy",
            "an unversioned file still imports under its own name"
        );
        assert_eq!(layout.widgets()[0].kind(), WidgetKind::SlideSlider);
    }

    /// A version-1 file needs no mode migration: its viewer is in its tree.
    #[test]
    fn a_v1_file_derives_its_viewer_from_its_widgets() {
        let (_directory, mut store) = store();
        let legacy = legacy_kind_shape("Legacy", "document-page");

        let (id, _) = store.import(&legacy).unwrap();
        let layout = store.get(&id).unwrap();
        assert_eq!(
            crate::layout::PrimaryViewer::of(layout),
            crate::layout::PrimaryViewer::Document
        );
    }

    /// The obsolete purpose flag is neither needed nor written. Serde ignores
    /// it when reading an older exported file.
    #[test]
    fn layout_files_do_not_persist_a_mode() {
        let (directory, mut store) = store();
        let id = store.save(&custom_layout("Conference")).unwrap();

        let text =
            std::fs::read_to_string(directory.path().join(format!("{}.json", id.0))).unwrap();
        assert!(
            !text.contains("\"purpose\""),
            "mode leaked into layout: {text}"
        );

        let reloaded = LayoutStore::load(directory.path());
        let layout = reloaded.get(&id).unwrap();
        assert_eq!(
            crate::layout::PrimaryViewer::of(layout),
            crate::layout::PrimaryViewer::Slide
        );
    }

    #[test]
    fn a_version_one_file_with_an_unknown_kind_blocks_the_import() {
        // A version-1 file names widgets by their Rust-side `kind` tag, so an
        // unrecognised one is a genuine parse failure — this is the case
        // version 2's `widget_id` tolerance does not (and, being a hand-typo
        // or a genuinely different Rust version, cannot) cover.
        let (_directory, mut store) = store();
        let mut tampered = legacy_kind_shape("Alien", "hologram-projector");
        tampered = tampered.replace(
            "\"design_ratio\"",
            "\"format_version\": 1,\n              \"design_ratio\"",
        );
        assert!(matches!(store.import(&tampered), Err(StoreError::Parse(_))));
        assert!(store.custom().is_empty(), "nothing was imported");
    }

    #[test]
    fn a_version_one_fixture_loads_to_the_same_layout_as_its_version_two_equivalent() {
        let v1 = include_str!("fixtures/presenter-default.v1.json");
        let v2 = include_str!("fixtures/presenter-default.json");
        let (from_v1, _) = import_from_string(v1).unwrap();
        let (from_v2, _) = import_from_string(v2).unwrap();
        assert_eq!(from_v1.name, from_v2.name);
        assert_eq!(from_v1.design_ratio, from_v2.design_ratio);
        assert_eq!(from_v1.root, from_v2.root);
    }

    #[test]
    fn an_unknown_widget_id_becomes_a_placeholder_instead_of_blocking() {
        let (_directory, mut store) = store();
        let text = export_to_string(&custom_layout("Alien")).unwrap();
        let tampered = text.replace("pulpit.slide.current", "pulpit.slide.holographic");

        let (id, issues) = store.import(&tampered).unwrap();
        assert!(
            !issues
                .iter()
                .any(|issue| issue.severity == crate::layout::validate::Severity::Blocking),
            "an unknown widget id is not a blocking condition"
        );
        let layout = store.get(&id).unwrap();
        let cell = layout
            .root
            .cells()
            .into_iter()
            .find(|cell| cell.unavailable.is_some())
            .expect("the unrecognised widget became a placeholder");
        let unavailable = cell.unavailable.as_ref().unwrap();
        assert_eq!(unavailable.widget_id, "pulpit.slide.holographic");
        assert!(cell.widget.is_none());

        // Saving it again reproduces the placeholder, id and configuration
        // untouched.
        let reexported = store.export(&id).unwrap();
        let reimported = import_from_string(&reexported).unwrap().0;
        let reimported_cell = reimported
            .root
            .cells()
            .into_iter()
            .find(|cell| cell.unavailable.is_some())
            .expect("the placeholder survives a save/load round trip");
        assert_eq!(reimported_cell.unavailable, cell.unavailable);
    }

    #[test]
    fn an_invalid_structure_blocks_the_import() {
        let (_directory, mut store) = store();
        // Two speaker-notes widgets: legal JSON, illegal layout.
        let mut layout = custom_layout("Broken");
        let ids: Vec<_> = layout.cells().iter().map(|cell| cell.id).collect();
        layout.root.cell_mut(ids[0]).unwrap().widget = Some(Widget::new(WidgetKind::SpeakerNotes));
        layout.root.cell_mut(ids[1]).unwrap().widget = Some(Widget::new(WidgetKind::SpeakerNotes));
        let text = export_to_string(&layout).unwrap();

        match store.import(&text) {
            Err(StoreError::Invalid(reason)) => assert!(reason.contains("Speaker Notes")),
            other => panic!("expected a blocked import, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_warnings_do_not_block_an_import() {
        let (_directory, mut store) = store();
        // No navigation, no current slide: warnings, not errors.
        let text = export_to_string(&Layout::empty("Sparse")).unwrap();
        let (id, issues) = store.import(&text).unwrap();
        assert!(store.get(&id).is_some());
        assert!(
            !issues.is_empty(),
            "the warnings are reported to the caller"
        );
    }

    #[test]
    fn unreadable_files_in_the_directory_are_skipped_not_fatal() {
        let (directory, mut store) = store();
        store.save(&custom_layout("Good")).unwrap();
        std::fs::write(directory.path().join("broken.json"), "{ not json").unwrap();
        std::fs::write(directory.path().join("notes.txt"), "ignored").unwrap();

        let reloaded = LayoutStore::load(directory.path());
        assert_eq!(reloaded.custom().len(), 1);
        assert_eq!(reloaded.custom()[0].name, "Good");
    }

    #[test]
    fn imported_ids_are_renumbered_so_they_cannot_collide() {
        let (_directory, mut store) = store();
        let text = store.export(&LayoutId("presenter-default".into())).unwrap();
        let (id, _) = store.import(&text).unwrap();
        let layout = store.get(&id).unwrap();
        let ids = layout.root.ids();
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len());
    }
}

#[cfg(test)]
mod fixtures {
    use super::*;
    use crate::layout::tree::{Cell, Node, Split};
    use crate::widgets::{Widget, WidgetKind};

    /// The committed files, and what each one is for.
    ///
    /// Several, because one layout cannot hold every widget: instance limits
    /// and compound occupancy would make it invalid. Between them they cover
    /// every kind, which the test below proves.
    const FIXTURES: [(&str, &str); 3] = [
        (
            "presenter-default.json",
            include_str!("fixtures/presenter-default.json"),
        ),
        (
            "reader-default.json",
            include_str!("fixtures/reader-default.json"),
        ),
        (
            "every-other-widget.json",
            include_str!("fixtures/every-other-widget.json"),
        ),
    ];

    /// A layout holding the widgets the built-ins do not use, so the set of
    /// fixtures covers the whole catalog.
    pub fn every_other_widget() -> Layout {
        let kinds = [
            WidgetKind::PreviousCurrentNext,
            WidgetKind::PreviousSlide,
            WidgetKind::SlideButtons,
            WidgetKind::SlideSlider,
            WidgetKind::Timer,
            WidgetKind::Clock,
            WidgetKind::SlideCounter,
            WidgetKind::PauseResume,
            WidgetKind::EndPresentation,
            WidgetKind::PresentationTitle,
            WidgetKind::AudienceScreenStatus,
            WidgetKind::CurrentSection,
            WidgetKind::ConnectionStatus,
            WidgetKind::Annotations,
            WidgetKind::MediaTransport,
            WidgetKind::MainMenu,
            WidgetKind::AudienceControls,
            WidgetKind::Search,
            WidgetKind::BlankSpace,
        ];
        let mut children = Vec::new();
        let mut sizes = Vec::new();
        for (index, kind) in kinds.into_iter().enumerate() {
            children.push(Node::Leaf(Cell::with_widget(
                crate::layout::NodeId(index as u32 + 1),
                Widget::new(kind),
            )));
            sizes.push(1.0 / kinds.len() as f32);
        }
        Layout::from_parts(
            LayoutId("every-other-widget".into()),
            "Every other widget".into(),
            crate::layout::Origin::Custom,
            crate::layout::AspectRatio::SixteenNine,
            Node::Split(Split {
                id: crate::layout::NodeId(0),
                name: Some("Everything else".into()),
                direction: crate::layout::Direction::Vertical,
                children,
                sizes,
                gap: 8.0,
                min_child: 0.05,
            }),
        )
    }

    /// Rewrite the fixtures. Run with
    /// `PULPIT_WRITE_FIXTURES=1 cargo test -p pulpit fixtures`
    /// after a deliberate format change, then read the diff.
    #[test]
    fn fixtures_are_current() {
        if std::env::var_os("PULPIT_WRITE_FIXTURES").is_none() {
            return;
        }
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/layout/fixtures");
        std::fs::create_dir_all(&directory).unwrap();
        let layouts = [
            (
                "presenter-default.json",
                crate::layout::builtin::presenter_default(),
            ),
            ("every-other-widget.json", every_other_widget()),
            (
                "reader-default.json",
                crate::layout::builtin::reader_default(crate::layout::AspectRatio::SixteenNine),
            ),
        ];
        for (name, layout) in layouts {
            std::fs::write(directory.join(name), export_to_string(&layout).unwrap()).unwrap();
        }
    }

    #[test]
    fn every_fixture_round_trips_byte_for_byte() {
        for (name, text) in FIXTURES {
            let (layout, _) =
                import_from_string(text).unwrap_or_else(|e| panic!("{name} does not import: {e}"));
            let again = export_to_string(&layout).unwrap();
            assert_eq!(
                again.trim(),
                text.trim(),
                "{name} changed shape on the way through"
            );
        }
    }

    #[test]
    fn the_fixtures_between_them_cover_every_widget() {
        let mut seen: Vec<WidgetKind> = Vec::new();
        for (name, text) in FIXTURES {
            let (layout, _) =
                import_from_string(text).unwrap_or_else(|e| panic!("{name} does not import: {e}"));
            for widget in layout.widgets() {
                if !seen.contains(&widget.kind()) {
                    seen.push(widget.kind());
                }
            }
        }
        let missing: Vec<WidgetKind> = WidgetKind::ALL
            .into_iter()
            .filter(|kind| !seen.contains(kind))
            .collect();
        assert!(
            missing.is_empty(),
            "no committed fixture contains {missing:?}"
        );
    }

    #[test]
    fn a_configuration_that_does_not_belong_to_its_kind_is_refused() {
        // Not sanitised away: the file claims something that cannot be true,
        // and quietly choosing one half would change what the presenter sees.
        let text = r#"{
          "format_version": 1,
          "name": "Impossible",
          "design_ratio": "sixteen-nine",
          "root": {
            "type": "leaf",
            "id": 0,
            "widget": {
              "kind": "presentation-title",
              "style": {"variant": "standard", "scale": 1.0, "alignment": "center"},
              "config": {"notes": {"font_size": 18.0, "line_spacing": 1.5, "source": "current-slide"}}
            },
            "padding": 0.0,
            "background": "none",
            "border": "none",
            "empty_behavior": "show-blank-panel"
          }
        }"#;
        let error = import_from_string(text).expect_err("a mismatch blocks import");
        assert!(
            format!("{error}").contains("configuration"),
            "the message should say what is wrong, got: {error}"
        );
    }
}
