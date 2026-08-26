//! File-watch hints.
//!
//! Deliberately thin: the watcher only says "something touched the file".
//! Every decision about whether that means a new document exists belongs to
//! [`crate::doc::manager::DocumentManager`], which is pure and testable.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
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
    wakeup: Option<Arc<FileWakeup>>,
}

/// One-slot event-loop doorbell for filesystem hints.
pub struct FileWakeup {
    inbox: Mutex<Receiver<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wakeup {
    Ring,
    Idle,
    Closed,
}

impl FileWakeup {
    pub fn wait(&self, timeout: Duration) -> Wakeup {
        let Ok(inbox) = self.inbox.try_lock() else {
            return Wakeup::Closed;
        };
        match inbox.recv_timeout(timeout) {
            Ok(()) => Wakeup::Ring,
            Err(RecvTimeoutError::Timeout) => Wakeup::Idle,
            Err(RecvTimeoutError::Disconnected) => Wakeup::Closed,
        }
    }
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
///
/// `watched` is `None` for an image document, whose source is the *directory*
/// (`SPEC-images.md` §40.1): there the interesting change is any supported
/// image in it, and the predicate widens to the one extension set in
/// `pulpit_render::images` rather than restating it (§41.5, §50.2).
fn is_the_watched_file(changed: &Path, watched: Option<&std::ffi::OsStr>) -> bool {
    match watched {
        Some(name) => changed.file_name() == Some(name),
        None => pulpit_render::images::is_supported_image(changed),
    }
}

impl DocumentWatcher {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, WatchError> {
        let path = path.as_ref().to_path_buf();
        // An image document *is* its directory, so that is what is watched,
        // and every supported image in it counts. A PDF watches the directory
        // containing it and filters by name.
        let is_directory = path.is_dir();
        let directory = if is_directory {
            path.clone()
        } else {
            path.parent().unwrap_or(Path::new(".")).to_path_buf()
        };
        let watched = if is_directory {
            None
        } else {
            path.file_name().map(|name| name.to_os_string())
        };
        let (sender, events) = channel();
        let (signal, wakeup) = sync_channel(1);

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
                let _ = signal.try_send(());
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
            wakeup: Some(Arc::new(FileWakeup {
                inbox: Mutex::new(wakeup),
            })),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn take_wakeup(&mut self) -> Option<Arc<FileWakeup>> {
        self.wakeup.take()
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

    /// Forget whatever the setup writes produced, so that what follows is
    /// the only thing the watcher can be reacting to.
    ///
    /// inotify draws a clean line: nothing that happened before
    /// `inotify_add_watch` is ever reported. FSEvents does not — it
    /// coalesces per directory, so a file written moments before the stream
    /// opened still arrives in the stream's first batch. A negative test
    /// without this sees the setup write, matches it against the watched
    /// name, and calls it the change it was told to ignore.
    fn settle(watcher: &DocumentWatcher) {
        std::thread::sleep(Duration::from_millis(250));
        watcher.drain();
    }

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

    /// §41.5 and §51.4: the predicate reads the one extension set rather than
    /// restating it, so a format added there cannot be a format the watcher
    /// silently ignores.
    #[test]
    fn a_directory_source_notices_exactly_the_supported_extensions() {
        for extension in pulpit_render::images::IMAGE_EXTENSIONS {
            let name = format!("/pictures/talk/slide.{extension}");
            assert!(
                is_the_watched_file(Path::new(&name), None),
                "{name} is a page of the document"
            );
            let shouted = format!("/pictures/talk/slide.{}", extension.to_uppercase());
            assert!(is_the_watched_file(Path::new(&shouted), None), "{shouted}");
        }
        for other in ["/pictures/talk/notes.txt", "/pictures/talk/deck.pdf"] {
            assert!(!is_the_watched_file(Path::new(other), None), "{other}");
        }
    }

    #[test]
    fn a_new_image_in_the_watched_directory_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"first").unwrap();
        let watcher = DocumentWatcher::new(dir.path()).unwrap();

        std::fs::write(dir.path().join("b.png"), b"second").unwrap();
        assert!(
            watcher.wait(Duration::from_secs(5)),
            "a page appearing in the folder is a change to the document"
        );
    }

    #[test]
    fn an_unrelated_file_in_the_watched_directory_is_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"first").unwrap();
        let watcher = DocumentWatcher::new(dir.path()).unwrap();
        settle(&watcher);

        std::fs::write(dir.path().join("notes.txt"), b"unrelated").unwrap();
        assert!(!watcher.wait(Duration::from_millis(500)));
    }

    #[test]
    fn other_files_in_the_directory_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pdf");
        std::fs::write(&path, b"original").unwrap();
        let watcher = DocumentWatcher::new(&path).unwrap();
        settle(&watcher);

        std::fs::write(dir.path().join("notes.txt"), b"unrelated").unwrap();
        assert!(!watcher.wait(Duration::from_millis(500)));
    }
}
