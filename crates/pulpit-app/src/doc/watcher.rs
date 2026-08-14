//! File-watch hints.
//!
//! Deliberately thin: the watcher only says "something touched the file".
//! Every decision about whether that means a new document exists belongs to
//! [`crate::doc::manager::DocumentManager`], which is pure and testable.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("cannot watch {path}: {reason}")]
    Watch { path: String, reason: String },
}

/// Watches the *directory* containing the document, not the file itself:
/// generators like `typst watch` and LaTeX replace the file by renaming a new
/// one over it, which breaks an inode-level watch immediately.
pub struct DocumentWatcher {
    path: PathBuf,
    _watcher: notify::RecommendedWatcher,
    events: Receiver<()>,
}

impl std::fmt::Debug for DocumentWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentWatcher")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Does this event name the document?
///
/// Compared by *file name*, never by whole path. The watch is non-recursive
/// on exactly one directory, so anything reported is already an entry in it
/// and the name alone identifies the file.
///
/// Comparing whole paths looks more precise and is in fact broken, because
/// the path a backend reports is not the path the caller wrote:
///
/// - macOS FSEvents reports the *canonical* path, so a deck under
///   `/var/folders/…` — or `/tmp`, or any symlinked directory — arrives as
///   `/private/var/folders/…` and matches nothing. Auto-reload then silently
///   never fires, which is indistinguishable from a generator that did not
///   rebuild.
/// - A relative path given on the command line is reported back resolved
///   differently by different backends.
///
/// This is why the argument is an `OsStr` and not a `Path`.
fn is_the_watched_file(changed: &Path, watched: Option<&std::ffi::OsStr>) -> bool {
    match watched {
        Some(name) => changed.file_name() == Some(name),
        // A path with no final component cannot be a document.
        None => false,
    }
}

impl DocumentWatcher {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, WatchError> {
        let path = path.as_ref().to_path_buf();
        let directory = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let watched = path.file_name().map(|name| name.to_os_string());
        let (sender, events) = channel();

        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let Ok(event) = event else { return };
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return;
            }
            if event
                .paths
                .iter()
                .any(|changed| is_the_watched_file(changed, watched.as_deref()))
            {
                let _ = sender.send(());
            }
        })
        .map_err(|e| WatchError::Watch {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

        watcher
            .watch(&directory, RecursiveMode::NonRecursive)
            .map_err(|e| WatchError::Watch {
                path: directory.display().to_string(),
                reason: e.to_string(),
            })?;

        Ok(Self {
            path,
            _watcher: watcher,
            events,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Block for up to `timeout` for a hint. Returns true if the file was
    /// touched; repeated hints collapse into one.
    pub fn wait(&self, timeout: Duration) -> bool {
        match self.events.recv_timeout(timeout) {
            Ok(()) => {
                // Drain the burst: the manager debounces anyway.
                while self.events.try_recv().is_ok() {}
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => false,
        }
    }

    pub fn drain(&self) -> bool {
        let mut any = false;
        while self.events.try_recv().is_ok() {
            any = true;
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The macOS failure, as a test that runs everywhere: FSEvents reports
    /// the canonical path, so the deck the user opened as `/var/…` is
    /// reported as `/private/var/…`. Comparing whole paths dropped the event
    /// and auto-reload silently never fired.
    #[test]
    fn a_differently_spelled_path_for_the_same_file_still_matches() {
        let name = std::ffi::OsString::from("deck.pdf");
        for spelling in [
            "/private/var/folders/T/deck.pdf",
            "/var/folders/T/deck.pdf",
            "deck.pdf",
            "./deck.pdf",
        ] {
            assert!(
                is_the_watched_file(Path::new(spelling), Some(&name)),
                "{spelling} is the watched file however it is spelled"
            );
        }
    }

    #[test]
    fn another_file_in_the_same_directory_is_not_the_document() {
        let name = std::ffi::OsString::from("deck.pdf");
        for other in ["/var/folders/T/notes.pdf", "deck.pdf.tmp", "deck.pd"] {
            assert!(
                !is_the_watched_file(Path::new(other), Some(&name)),
                "{other} is not the watched file"
            );
        }
    }

    #[test]
    fn a_rename_over_the_file_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pdf");
        std::fs::write(&path, b"original").unwrap();

        let watcher = DocumentWatcher::new(&path).unwrap();

        // Exactly what a generator does: write a temporary, then rename.
        let temporary = dir.path().join("deck.pdf.tmp");
        std::fs::write(&temporary, b"rebuilt and longer").unwrap();
        std::fs::rename(&temporary, &path).unwrap();

        assert!(
            watcher.wait(Duration::from_secs(5)),
            "the rebuild was not noticed"
        );
    }

    #[test]
    fn other_files_in_the_directory_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pdf");
        std::fs::write(&path, b"original").unwrap();
        let watcher = DocumentWatcher::new(&path).unwrap();

        std::fs::write(dir.path().join("notes.txt"), b"unrelated").unwrap();
        assert!(!watcher.wait(Duration::from_millis(500)));
    }
}
