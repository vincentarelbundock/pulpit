//! The measured shape of a known document, kept between sessions.
//!
//! Describing a document loads every page for its real geometry — the crop
//! box and rotation a mark's coordinates are measured against — and on a
//! six-hundred-page book that is a third of a second, after a comparable
//! wait for the worker process to come up at all. Both sit squarely on the
//! open path: the reader cannot lay out its column, restore its place or
//! render the page being returned to until the answer lands.
//!
//! The shape is a pure function of the file's bytes, and the application
//! already computes a content hash of those bytes at every open. So the
//! shape is remembered against that hash, and reopening a known document —
//! which is what a reading position makes routine — lays out from the
//! record while the worker measures afresh. The two answers are compared
//! when the real one arrives: equal, and nothing happened; different — the
//! file changed between the hash and the measure, which a watcher-triggered
//! rebuild can arrange — and the fresh answer replaces the remembered one
//! wholesale.
//!
//! Trust is anchored in the hash, not the file name: a renamed, moved or
//! copied document keeps its shape, and an edited one cannot inherit it.

use std::path::PathBuf;

use pulpit_core::navigation::Outline;
use pulpit_core::page::PageGeometry;
use pulpit_render::document::OpenDocumentInfo;
use serde::{Deserialize, Serialize};

/// Bumped when the layout of the record changes; an unreadable or
/// wrong-version file is a miss, never an error.
const VERSION: u32 = 1;

/// How many documents' shapes to keep. Enough for anyone's rotation of
/// current reading; bounded so an archive crawl cannot grow the directory
/// for ever.
const KEPT: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentShape {
    pub info: OpenDocumentInfo,
    pub geometry: Vec<PageGeometry>,
    pub outline: Outline,
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    shape: DocumentShape,
}

fn directory() -> PathBuf {
    // This is a cache, not configuration: `remember` says so above,
    // and losing it costs a slower open, never data. It belongs under
    // `Directories::cache`, which a system cleaner may clear, rather than
    // under configuration, which one must not touch.
    crate::platform::Directories::detect().cache.join("shapes")
}

fn path_for(directory: &std::path::Path, hash: &str) -> Option<PathBuf> {
    // The hash is lowercase hex from this application's own hasher, but it
    // is about to become a file name: refuse anything else outright rather
    // than trust the caller for ever.
    hash.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| directory.join(format!("{hash}.json")))
}

/// The remembered shape of the document with these bytes, if any.
pub fn recall(hash: &str) -> Option<DocumentShape> {
    recall_in(&directory(), hash)
}

fn recall_in(directory: &std::path::Path, hash: &str) -> Option<DocumentShape> {
    let bytes = std::fs::read(path_for(directory, hash)?).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    (record.version == VERSION).then_some(record.shape)
}

/// Remember a freshly measured shape, and forget the stalest beyond the
/// bound. Failures are silent: the cache is a shortcut, and a disk that
/// will not take it costs a slower open, not an error anyone can act on.
pub fn remember(hash: &str, shape: DocumentShape) {
    remember_in(&directory(), hash, shape);
}

fn remember_in(directory: &std::path::Path, hash: &str, shape: DocumentShape) {
    let Some(path) = path_for(directory, hash) else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    let record = Record {
        version: VERSION,
        shape,
    };
    let Ok(bytes) = serde_json::to_vec(&record) else {
        return;
    };
    // Whole or absent: a torn record read back as a miss would be fine, but
    // read back as valid JSON of half a document it would not be. Written
    // through `pulpit_render::atomic::replace` rather than a predictable
    // `.tmp` sibling with no `fsync`, for the same reason every other writer
    // in pulpit goes through it.
    let _ = pulpit_render::atomic::replace(
        &path,
        "shape",
        pulpit_render::atomic::Visibility::Private,
        &bytes,
    );

    // Prune by age, oldest first, only past the bound.
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut shapes: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    if shapes.len() <= KEPT {
        return;
    }
    shapes.sort_by_key(|(modified, _)| *modified);
    for (_, stale) in shapes.iter().take(shapes.len() - KEPT) {
        let _ = std::fs::remove_file(stale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(pages: usize) -> DocumentShape {
        let page = PageGeometry::upright(612.0, 792.0);
        DocumentShape {
            info: OpenDocumentInfo {
                page_count: pages,
                level: Default::default(),
                warnings: Vec::new(),
                first_page: page,
                has_form: false,
            },
            geometry: vec![page; pages],
            outline: Outline::default(),
        }
    }

    #[test]
    fn a_shape_round_trips_by_hash_and_an_unknown_hash_misses() {
        let directory = tempfile::tempdir().unwrap();
        let hash = "abc123def4567890abc123def4567890";
        remember_in(directory.path(), hash, shape(3));
        assert_eq!(recall_in(directory.path(), hash), Some(shape(3)));
        assert_eq!(
            recall_in(directory.path(), "00000000000000000000000000000000"),
            None
        );
    }

    #[test]
    fn a_stale_shape_is_pruned_and_a_bad_hash_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        for n in 0..KEPT + 3 {
            remember_in(directory.path(), &format!("{n:032x}"), shape(1));
        }
        let kept = std::fs::read_dir(directory.path()).unwrap().count();
        assert!(kept <= KEPT + 1, "{kept} shapes survived the prune");
        assert_eq!(recall_in(directory.path(), "../escape"), None);
    }
}
