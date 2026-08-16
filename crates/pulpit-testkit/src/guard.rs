//! The invariant every other test is allowed to assume.
//!
//! This program's one irreversible action is writing a file. It promises never
//! to write the file it was opened on: exports go to a new path, and a failed
//! export leaves nothing behind. Those promises are cheap to state and easy to
//! break accidentally — a temporary file created next to the source, a
//! `persist` onto the wrong path, a partial write left after an error.
//!
//! So rather than a handful of tests that remember to check, the check is an
//! object: hold one across an operation and it verifies on the way out.

use std::path::{Path, PathBuf};

/// Watches a file, and fails if it changes while the guard is alive.
pub struct Unchanged {
    path: PathBuf,
    before: Vec<u8>,
    what: String,
}

impl Unchanged {
    /// Start watching `path`, which must exist.
    pub fn new(path: &Path, what: &str) -> Self {
        let before = std::fs::read(path)
            .unwrap_or_else(|error| panic!("cannot read {} to guard it: {error}", path.display()));
        Self {
            path: path.to_path_buf(),
            before,
            what: what.to_owned(),
        }
    }

    /// Check now, without waiting for the guard to be dropped.
    pub fn check(&self) {
        let after = std::fs::read(&self.path).unwrap_or_else(|error| {
            panic!(
                "{}: the source {} disappeared: {error}",
                self.what,
                self.path.display()
            )
        });
        assert!(
            after == self.before,
            "{}: the source document {} was modified. \
             The source is the one file this program must never write: it is \
             what the user still has if anything else goes wrong. \
             ({} bytes before, {} after)",
            self.what,
            self.path.display(),
            self.before.len(),
            after.len()
        );
    }
}

impl Drop for Unchanged {
    fn drop(&mut self) {
        // A failing assertion inside a drop during another panic would abort
        // and hide the original cause, so defer to whatever failed first.
        if std::thread::panicking() {
            return;
        }
        self.check();
    }
}

/// Assert that a failed operation left nothing at `destination`.
///
/// A half-written PDF is worse than no PDF: it looks like a result.
pub fn nothing_written(destination: &Path, what: &str) {
    assert!(
        !destination.exists(),
        "{what}: the operation failed but left {} behind. \
         A failed export must leave no file for the user to mistake for one.",
        destination.display()
    );
}

/// Every file in `directory`, so a test can assert that an operation left no
/// stray temporary files next to the document it was working on.
pub fn entries(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}
