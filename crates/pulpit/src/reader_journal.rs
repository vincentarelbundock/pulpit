//! The document journal: every edit, durably, as it is made (§11.1).
//!
//! The presentation snapshot and this file answer the same question — *what
//! was the last run doing, and does it still apply to this file?* — and the
//! specification settles them into one restore prompt. They differ in one way,
//! and the difference is normative rather than an implementation choice:
//!
//! * a **presentation** payload is a periodic snapshot, because losing a slide
//!   index costs a keystroke;
//! * a **document** payload appends each command *at commit*, because losing
//!   an edit is data loss.
//!
//! So this is an append-only log with a flush per entry, not a snapshot on a
//! timer. It is written beside the settings, deleted on a clean quit, and
//! offered back only after the source file's fingerprint still matches and
//! only on an explicit answer — the same rules the presentation snapshot
//! already keeps.
//!
//! What is *not* in it (§11.4): no PDFium pointers, no object numbers as
//! identity, no passwords, and no transient gestures. Every entry names
//! annotations by their `/NM` identity and geometry in canonical page points,
//! which is exactly what makes replay against a freshly opened file possible.

use std::io::Write;
use std::path::{Path, PathBuf};

use pulpit_render::document::{DocumentRevision, DocumentTransaction, DocumentUndo};
use serde::{Deserialize, Serialize};

use crate::session::DocumentFingerprint;

/// Bumped when the journal's line format changes. A journal written by a
/// different version is discarded rather than half-understood: a partly
/// replayed edit history is worse than none.
pub const JOURNAL_SCHEMA: u32 = 1;

/// The most entries one journal keeps.
///
/// A session's worth of edits many times over. Past it the journal stops
/// growing and says so, rather than filling a disk while a runaway loop
/// commits (§11.5). The bound is on the file, not on the document: the
/// document is unaffected and Save As still writes everything.
pub const MAX_ENTRIES: usize = pulpit_render::document::limits::MAX_JOURNAL_ENTRIES;

/// What one revision-incrementing operation was.
///
/// Undos and redos are recorded like anything else, in revision order, so
/// replay reproduces the exact history — and an edit the user undid stays
/// undone after recovery (§11.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JournalEntry {
    /// One atomic user action.
    Applied {
        revision: DocumentRevision,
        transaction: DocumentTransaction,
    },
    /// An undo or a redo, which are the same request carrying different
    /// operations (§9.5).
    Reversed {
        revision: DocumentRevision,
        operation: Box<DocumentUndo>,
    },
}

impl JournalEntry {
    /// The revision this entry produced.
    pub fn revision(&self) -> DocumentRevision {
        match self {
            JournalEntry::Applied { revision, .. } | JournalEntry::Reversed { revision, .. } => {
                *revision
            }
        }
    }
}

/// The first line of a journal: which document it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalHeader {
    pub schema: u32,
    pub source: PathBuf,
    pub fingerprint: DocumentFingerprint,
}

/// A journal read back from disk.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredJournal {
    pub header: JournalHeader,
    pub entries: Vec<JournalEntry>,
    /// True when the file ended mid-line — a crash during a write.
    ///
    /// The complete entries before it are still good, and are offered; the
    /// half-written one is not, because half an edit is not an edit.
    pub truncated: bool,
}

impl RecoveredJournal {
    /// Is this worth offering back?
    ///
    /// Only when it belongs to the file being opened *and* has something in
    /// it. An offer to restore nothing is a prompt for its own sake.
    pub fn applies_to(&self, source: &Path, fingerprint: &DocumentFingerprint) -> bool {
        !self.entries.is_empty()
            && self.header.schema == JOURNAL_SCHEMA
            && self.header.source == source
            && self.header.fingerprint.matches(fingerprint)
    }

    /// The entries in revision order, which is the order they must be
    /// replayed in.
    ///
    /// Sorted rather than assumed: the file is appended in order, but a
    /// journal that has been edited or concatenated by something else must
    /// not replay an undo before the edit it undoes.
    pub fn in_order(&self) -> Vec<JournalEntry> {
        let mut entries = self.entries.clone();
        entries.sort_by_key(|entry| entry.revision());
        entries
    }

    /// What to tell the user before they answer.
    pub fn summary(&self) -> String {
        let count = self.entries.len();
        let name = self
            .header
            .source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.header.source.display().to_string());
        let edits = if count == 1 { "edit" } else { "edits" };
        if self.truncated {
            format!("{count} unsaved {edits} to {name}, and one that was interrupted")
        } else {
            format!("{count} unsaved {edits} to {name}")
        }
    }
}

/// The open journal for one document.
pub struct Journal {
    path: PathBuf,
    file: Option<std::fs::File>,
    written: usize,
    /// Set once the bound is reached, so the warning is given once.
    full: bool,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("path", &self.path)
            .field("written", &self.written)
            .finish()
    }
}

impl Journal {
    /// Start a journal for `source`, replacing any journal already there.
    ///
    /// Replacing rather than appending: the old one belonged to a session that
    /// has been offered and answered, and mixing two sessions' edits into one
    /// file would replay a history that never happened.
    pub fn start(
        path: impl Into<PathBuf>,
        source: &Path,
        fingerprint: DocumentFingerprint,
    ) -> std::io::Result<Journal> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&path)?;
        let header = JournalHeader {
            schema: JOURNAL_SCHEMA,
            source: source.to_path_buf(),
            fingerprint,
        };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;
        file.sync_all()?;
        Ok(Journal {
            path,
            file: Some(file),
            written: 0,
            full: false,
        })
    }

    /// Where this journal is, for a diagnostic that has to name it.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many entries have been recorded. Read by the tests that prove a
    /// save ends the journal and that it stops at its bound; a diagnostic
    /// reads it too.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.written
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.written == 0
    }

    /// Record one revision-incrementing operation, durably.
    ///
    /// Flushed and synced before returning, because the whole point of the
    /// document payload is that an edit survives the process that made it.
    /// That cost is paid once per user action — not once per pointer sample —
    /// which is what makes it affordable.
    pub fn append(&mut self, entry: &JournalEntry) -> std::io::Result<()> {
        if self.written >= MAX_ENTRIES {
            self.full = true;
            return Ok(());
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        writeln!(file, "{}", serde_json::to_string(entry)?)?;
        file.sync_all()?;
        self.written += 1;
        Ok(())
    }

    /// Has the journal stopped recording?
    pub fn is_full(&self) -> bool {
        self.full
    }

    /// The document has been saved, so nothing in the journal is unsaved any
    /// more. The file goes.
    ///
    /// Not "the edits are gone": they are in the file the user just wrote,
    /// which is the point. A journal kept past a save would offer to replay
    /// edits onto a document that already has them.
    pub fn finish(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.path);
        self.written = 0;
    }

    /// Read a journal back, if there is one worth reading.
    pub fn recover(path: &Path) -> Option<RecoveredJournal> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut lines = text.lines();
        let header: JournalHeader = serde_json::from_str(lines.next()?).ok()?;
        if header.schema != JOURNAL_SCHEMA {
            return None;
        }

        let mut entries = Vec::new();
        let mut truncated = false;
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalEntry>(line) {
                Ok(entry) => entries.push(entry),
                // A line that will not parse is the one the crash interrupted.
                // Everything before it is complete, and stopping here rather
                // than skipping on is what keeps the history contiguous.
                Err(_) => {
                    truncated = true;
                    break;
                }
            }
            if entries.len() >= MAX_ENTRIES {
                break;
            }
        }
        Some(RecoveredJournal {
            header,
            entries,
            truncated,
        })
    }

    /// Remove a journal without reading it — the clean-quit path.
    pub fn discard(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        // A journal is *not* removed on drop. Dropping is what happens on a
        // crash as well as on a close, and the whole purpose of the file is to
        // outlive the first. `finish` is the deliberate ending.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotate::{
        AnnotationCommand, AnnotationDraft, InkDraft, InkPoint, MarkStyle,
    };
    use pulpit_core::page::PageIndex;

    fn fingerprint() -> DocumentFingerprint {
        DocumentFingerprint {
            path: "/tmp/paper.pdf".into(),
            size: Some(4_096),
            modified_unix: Some(1_700_000_000),
        }
    }

    fn stroke(x: f32) -> DocumentTransaction {
        DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
            InkDraft {
                page: PageIndex(0),
                points: vec![InkPoint::new(x, 10.0), InkPoint::new(x + 40.0, 60.0)],
                style: MarkStyle::default(),
            },
        ))])
    }

    fn entry(revision: u64, x: f32) -> JournalEntry {
        JournalEntry::Applied {
            revision: DocumentRevision(revision),
            transaction: stroke(x),
        }
    }

    #[test]
    fn an_edit_is_on_disk_before_the_call_returns() {
        // The whole reason the document payload is a journal and not a
        // snapshot: losing an edit is data loss (§11.1).
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();

        // Read it from the filesystem, not from the handle.
        let recovered = Journal::recover(&path).expect("the entry is on disk");
        assert_eq!(recovered.entries.len(), 1);
        assert!(!recovered.truncated);
    }

    #[test]
    fn a_journal_belongs_to_one_document_and_says_so() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let source = Path::new("/tmp/paper.pdf");
        let mut journal = Journal::start(&path, source, fingerprint()).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();

        let recovered = Journal::recover(&path).unwrap();
        assert!(recovered.applies_to(source, &fingerprint()));
        // A different file, or the same file changed underneath: not ours.
        assert!(!recovered.applies_to(Path::new("/tmp/other.pdf"), &fingerprint()));
        let mut changed = fingerprint();
        changed.size = Some(4_097);
        assert!(!recovered.applies_to(source, &changed));
    }

    #[test]
    fn an_empty_journal_is_not_offered() {
        // An offer to restore nothing is a prompt for its own sake.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let _journal = Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        let recovered = Journal::recover(&path).unwrap();
        assert!(recovered.entries.is_empty());
        assert!(!recovered.applies_to(Path::new("/tmp/paper.pdf"), &fingerprint()));
    }

    #[test]
    fn a_crash_mid_write_keeps_everything_before_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        {
            let mut journal =
                Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
            journal.append(&entry(1, 10.0)).unwrap();
            journal.append(&entry(2, 60.0)).unwrap();
        }
        // Half a line, as a process killed mid-write leaves.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"applied\":{\"revision\":3,\"transa");
        std::fs::write(&path, text).unwrap();

        let recovered = Journal::recover(&path).expect("the good entries survive");
        assert_eq!(recovered.entries.len(), 2);
        assert!(recovered.truncated, "the interrupted write is reported");
        assert!(recovered.summary().contains("interrupted"));
    }

    #[test]
    fn entries_replay_in_revision_order_however_the_file_is_ordered() {
        // An undo replayed before the edit it undoes is not the history that
        // happened.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.append(&entry(2, 60.0)).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();

        let recovered = Journal::recover(&path).unwrap();
        let order: Vec<u64> = recovered
            .in_order()
            .iter()
            .map(|entry| entry.revision().0)
            .collect();
        assert_eq!(order, vec![1, 2]);
    }

    #[test]
    fn undos_are_recorded_like_anything_else_so_replay_leaves_them_undone() {
        // §11.1: an edit the user undid stays undone after recovery.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();
        journal
            .append(&JournalEntry::Reversed {
                revision: DocumentRevision(2),
                operation: Box::new(DocumentUndo {
                    operations: Vec::new(),
                    restores: DocumentRevision(1),
                    label: "Add Ink".into(),
                }),
            })
            .unwrap();

        let recovered = Journal::recover(&path).unwrap();
        assert_eq!(recovered.entries.len(), 2);
        assert!(matches!(
            recovered.in_order()[1],
            JournalEntry::Reversed { .. }
        ));
    }

    #[test]
    fn saving_ends_the_journal_because_nothing_is_unsaved_any_more() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();
        assert!(path.exists());

        journal.finish();
        assert!(!path.exists(), "a journal kept past a save replays twice");
        assert!(journal.is_empty());
    }

    #[test]
    fn a_journal_from_another_version_is_discarded_rather_than_half_understood() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let header = serde_json::json!({
            "schema": JOURNAL_SCHEMA + 1,
            "source": "/tmp/paper.pdf",
            "fingerprint": fingerprint(),
        });
        std::fs::write(&path, format!("{header}\n")).unwrap();
        assert!(Journal::recover(&path).is_none());
    }

    #[test]
    fn a_journal_that_is_not_one_is_not_a_crash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        std::fs::write(&path, "this is not a journal\n").unwrap();
        assert!(Journal::recover(&path).is_none());
        assert!(Journal::recover(&directory.path().join("absent")).is_none());
    }

    #[test]
    fn the_journal_stops_growing_at_its_bound_rather_than_filling_a_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        // Reaching the real bound would write a hundred thousand fsynced
        // lines; the behaviour at the bound is what matters, so it is checked
        // by driving the counter to it.
        journal.written = MAX_ENTRIES;
        journal.append(&entry(1, 10.0)).unwrap();
        assert!(journal.is_full());
        assert_eq!(journal.len(), MAX_ENTRIES, "it grew past its own bound");
    }

    #[test]
    fn the_summary_counts_what_would_be_restored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();
        let one = Journal::recover(&path).unwrap();
        assert_eq!(one.summary(), "1 unsaved edit to paper.pdf");

        journal.append(&entry(2, 60.0)).unwrap();
        let two = Journal::recover(&path).unwrap();
        assert_eq!(two.summary(), "2 unsaved edits to paper.pdf");
    }

    #[test]
    fn a_new_session_replaces_the_previous_journal_rather_than_appending() {
        // Two sessions' edits in one file would replay a history that never
        // happened.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        {
            let mut journal =
                Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
            journal.append(&entry(1, 10.0)).unwrap();
        }
        let mut second = Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        second.append(&entry(1, 200.0)).unwrap();

        let recovered = Journal::recover(&path).unwrap();
        assert_eq!(recovered.entries.len(), 1, "the old session leaked through");
    }
}
