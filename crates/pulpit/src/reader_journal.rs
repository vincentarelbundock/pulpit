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

/// How a committed form field is written down, so it survives a crash (§11.5).
///
/// A value typed into a field is a revision-incrementing edit like any other,
/// and every one of those is recorded. This one did not used to be: the
/// journal's only entry point was the reply to a *transaction*, and a form
/// commit does not come back as one — it arrives as a `FormEventResult` from
/// PDFium's own editor. The result was a recovery that offered "N unsaved
/// edits", put back the ink strokes and silently dropped every field the
/// reader had filled in, which in a form is the whole of the work.
///
/// It is written as the [`DocumentCommand::SetField`] that reproduces it, for
/// the same reason the date and time pickers commit that way: replay then goes
/// through PDFium's editor, the field's format script runs over the value and
/// the appearance is regenerated, so there is no second way of writing a value
/// that would have to be kept working (§8.6).
///
/// `None` for a commit the engine could not name. There is no command that
/// could put one back — a value needs a field to go in — which is the same
/// reason no undo entry is recorded for one either.
///
/// A free function so the rule lives in one place and can be tested on a
/// committed field alone, without an application around it.
pub fn entry_for_committed_field(
    committed: &pulpit_render::document::protocol::CommittedField,
) -> Option<JournalEntry> {
    use pulpit_render::document::{DocumentCommand, DocumentTransaction};

    if committed.name.is_empty() {
        return None;
    }
    Some(JournalEntry::Applied {
        revision: committed.revision,
        transaction: DocumentTransaction::one(DocumentCommand::SetField {
            name: committed.name.clone(),
            value: committed.value.clone(),
            // What a multi-select list box chose, which the value alone names
            // at most one of.
            selected: committed.selected.clone(),
        }),
    })
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
    /// The file it was read from. The copy that read it owns it now: it
    /// belonged to a process that is gone, and the answer to the offer is
    /// what decides when it may be deleted.
    pub path: PathBuf,
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

/// The journal this process writes.
///
/// One file per copy, named after the process. Several copies of pulpit may
/// be editing different documents at once; a shared name would interleave
/// two edit histories into one file, and replay would reproduce a session
/// that never happened.
pub fn journal_name(pid: u32) -> String {
    format!("document-journal-{pid}.jsonl")
}

fn journal_pid(name: &str) -> Option<u32> {
    name.strip_prefix("document-journal-")?
        .strip_suffix(".jsonl")?
        .parse()
        .ok()
}

/// Journals left behind by copies that are no longer running.
///
/// `is_live` answers whether the process that wrote one still exists. A live
/// copy's journal is being appended to right now: reading it would offer
/// somebody else's unsaved edits, and deleting it after would take them away.
pub fn abandoned_journals(directory: &Path, is_live: impl Fn(u32) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = match std::fs::read_dir(directory) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let pid = journal_pid(name.to_str()?)?;
                (pid != std::process::id() && !is_live(pid)).then(|| (pid, entry.path()))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    found.sort_by_key(|(pid, _)| *pid);
    let mut paths: Vec<PathBuf> = found.into_iter().map(|(_, path)| path).collect();
    // The name used before journals were per-process. Unsaved edits are data
    // loss (§11.1), so one left by the version that wrote it is still offered.
    let legacy = directory.join("document-journal.jsonl");
    if legacy.is_file() {
        paths.insert(0, legacy);
    }
    paths
}

/// The open journal for one document.
pub struct Journal {
    path: PathBuf,
    file: Option<std::fs::File>,
    written: usize,
    /// Set once the bound is reached, so the warning is given once.
    full: bool,
    /// What `start` wrote into the header, kept so `finish` can re-create the
    /// file with the same header rather than leaving the journal silently
    /// stopped (§76.3). Unchanged for the life of the journal: pulpit only
    /// ever writes copies, never the source, so the fingerprint recorded at
    /// open is still the fingerprint of the file on disk after a save.
    source: PathBuf,
    fingerprint: DocumentFingerprint,
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
        let source = source.to_path_buf();
        let file = Self::create(&path, &source, &fingerprint)?;
        Ok(Journal {
            path,
            file: Some(file),
            written: 0,
            full: false,
            source,
            fingerprint,
        })
    }

    /// Write a fresh journal file with its header at `path`, for `start` and
    /// for `finish` re-creating one after a save.
    fn create(
        path: &Path,
        source: &Path,
        fingerprint: &DocumentFingerprint,
    ) -> std::io::Result<std::fs::File> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(path)?;
        let header = JournalHeader {
            schema: JOURNAL_SCHEMA,
            source: source.to_path_buf(),
            fingerprint: fingerprint.clone(),
        };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;
        file.sync_all()?;
        Ok(file)
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
        // §76.3: a journal with no file is not a journal that quietly
        // accepts everything — `finish` used to leave it in exactly that
        // state after the first Save As, so every edit made for the rest of
        // the session went unrecorded and unwarned-about. An `Err` here is
        // what tells the caller a save is needed before this edit is
        // protected, the same way a full journal does via `is_full`.
        let Some(file) = self.file.as_mut() else {
            return Err(std::io::Error::other(
                "the journal was closed by a save and has not been re-opened",
            ));
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

    /// The document has been saved, so nothing recorded so far is unsaved any
    /// more. The file is replaced with a fresh, empty one rather than simply
    /// removed: pulpit keeps editing the same in-memory document after a Save
    /// As, and every edit made from here on is exactly as unsaved as one made
    /// before it — the journal's job for the rest of the session, not only up
    /// to the first save.
    ///
    /// §76.3: this used to drop the file and leave it dropped, so `append`
    /// silently no-op'd for the rest of the session and nothing after a save
    /// was protected against a crash. The source and fingerprint are
    /// unchanged, because pulpit only ever writes copies of the source file,
    /// never the source itself.
    ///
    /// A failure to re-create the file is not surfaced here — there is
    /// nothing to hand it to on the save path — but leaves `self.file` unset,
    /// so the next `append` reports it as an `Err` rather than a silent
    /// success.
    pub fn finish(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.written = 0;
        self.full = false;
        self.file = Self::create(&self.path, &self.source, &self.fingerprint).ok();
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
            path: path.to_path_buf(),
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

    /// A running copy is still appending to its journal. Collecting it would
    /// offer somebody else's unsaved edits and then delete them.
    #[test]
    fn only_a_departed_copys_journal_is_abandoned() {
        let directory = tempfile::tempdir().unwrap();
        let live = 4242;
        let gone = 4243;
        for pid in [live, gone, std::process::id()] {
            std::fs::write(directory.path().join(journal_name(pid)), "").unwrap();
        }
        // Another kind of recovery file entirely, and not this scan's business.
        std::fs::write(directory.path().join("session-4243.json"), "").unwrap();

        let abandoned = abandoned_journals(directory.path(), |pid| pid == live);
        assert_eq!(
            abandoned,
            vec![directory.path().join(journal_name(gone))],
            "a live copy's journal and this copy's own are left alone"
        );
    }

    /// The name journals had before they were per-process. Unsaved edits from
    /// before an upgrade are still unsaved edits.
    #[test]
    fn a_journal_from_before_the_rename_is_still_offered() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("document-journal.jsonl"), "").unwrap();
        let abandoned = abandoned_journals(directory.path(), |_| false);
        assert_eq!(
            abandoned,
            vec![directory.path().join("document-journal.jsonl")]
        );
    }

    /// Two copies editing at once keep two journals, and neither one's entries
    /// reach the other's file. One shared name would replay a history that
    /// never happened.
    #[test]
    fn two_copies_write_two_journals() {
        let directory = tempfile::tempdir().unwrap();
        assert_ne!(journal_name(1), journal_name(2));
        let first = directory.path().join(journal_name(1));
        let second = directory.path().join(journal_name(2));
        let source = Path::new("/tmp/paper.pdf");
        let mut a = Journal::start(&first, source, fingerprint()).unwrap();
        let mut b = Journal::start(&second, source, fingerprint()).unwrap();
        a.append(&entry(1, 10.0)).unwrap();
        b.append(&entry(1, 20.0)).unwrap();
        b.append(&entry(2, 30.0)).unwrap();

        assert_eq!(Journal::recover(&first).unwrap().entries.len(), 1);
        assert_eq!(Journal::recover(&second).unwrap().entries.len(), 2);
    }

    /// A recovered journal knows which file it came from: the copy that wrote
    /// it is gone, so whoever answers the offer is what removes it.
    #[test]
    fn a_recovered_journal_names_the_file_it_came_from() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(journal_name(4243));
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();

        let recovered = Journal::recover(&path).expect("a journal to recover");
        assert_eq!(recovered.path, path);
        Journal::discard(&recovered.path);
        assert!(!path.exists(), "answering the offer removes the file");
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
    fn saving_clears_what_was_recorded_but_keeps_journalling() {
        // §76.3: `finish` used to drop the file and leave it dropped, so
        // every edit made for the rest of the session went unrecorded and
        // `is_full` never warned. It now re-creates an empty journal with the
        // same header, so the edits made after a Save As are exactly as
        // protected as the ones made before it.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.append(&entry(1, 10.0)).unwrap();
        assert!(path.exists());

        journal.finish();
        assert!(
            path.exists(),
            "a journal that stops after the first save protects nothing after it"
        );
        assert!(journal.is_empty());

        // The post-save edit is recorded, and is all `recover` offers back:
        // the pre-save edit is in the file the user just saved.
        journal.append(&entry(2, 60.0)).unwrap();
        let recovered = Journal::recover(&path).unwrap();
        assert_eq!(recovered.entries, vec![entry(2, 60.0)]);
    }

    #[test]
    fn a_journal_with_no_file_refuses_to_append_rather_than_silently_dropping_it() {
        // §76.3: `append` on a closed journal used to report `Ok` and record
        // nothing, so nothing ever warned that edits had stopped being
        // protected.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let mut journal =
            Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
        journal.file = None;
        assert!(journal.append(&entry(1, 10.0)).is_err());
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

    fn committed(
        name: &str,
        value: &str,
        selected: Vec<u32>,
    ) -> pulpit_render::document::protocol::CommittedField {
        pulpit_render::document::protocol::CommittedField {
            name: name.to_string(),
            value: value.to_string(),
            previous: String::new(),
            revision: DocumentRevision(7),
            selected,
            previous_selected: Vec::new(),
        }
    }

    #[test]
    fn a_filled_field_is_written_down_like_any_other_edit() {
        // The bug this holds shut: a value typed into a form field bumped the
        // revision, went into the undo history and was never journalled, so a
        // crash lost every field the reader had filled in while the recovery
        // prompt counted the ink strokes and called them "the unsaved edits".
        use pulpit_render::document::{DocumentCommand, DocumentTransaction};

        let entry = entry_for_committed_field(&committed("name", "Ada", Vec::new()))
            .expect("a named commit is recordable");
        assert_eq!(
            entry,
            JournalEntry::Applied {
                revision: DocumentRevision(7),
                transaction: DocumentTransaction::one(DocumentCommand::SetField {
                    name: "name".into(),
                    value: "Ada".into(),
                    selected: Vec::new(),
                }),
            },
            "a filled field must replay as the SetField that reproduces it"
        );
        assert_eq!(entry.revision(), DocumentRevision(7));
    }

    #[test]
    fn a_multi_select_carries_every_row_it_chose_and_not_just_one() {
        // A list box's value names at most one of three chosen rows. Recording
        // the string alone would replay a recovery that reinstated one tick
        // and dropped two, silently.
        let entry = entry_for_committed_field(&committed("langues", "Français", vec![0, 2, 3]))
            .expect("a named commit is recordable");
        let JournalEntry::Applied { transaction, .. } = entry else {
            panic!("a fill is an applied transaction");
        };
        let [pulpit_render::document::DocumentCommand::SetField { selected, .. }] =
            transaction.0.as_slice()
        else {
            panic!("one command, and it sets a field");
        };
        assert_eq!(selected, &[0, 2, 3]);
    }

    #[test]
    fn a_commit_the_engine_could_not_name_records_nothing() {
        // There is no command that puts back a value with no field to put it
        // in, and half an entry replayed is worse than none. The revision
        // still moved; that is the session's business, not the journal's.
        assert!(entry_for_committed_field(&committed("", "orphan", Vec::new())).is_none());
    }

    #[test]
    fn a_filled_field_survives_the_file_it_is_written_to() {
        // End to end, because the point of journalling a fill is that it comes
        // back after a crash — and the line has to parse as well as serialise.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.jsonl");
        let filled = entry_for_committed_field(&committed("name", "Ada", vec![1])).unwrap();
        {
            let mut journal =
                Journal::start(&path, Path::new("/tmp/paper.pdf"), fingerprint()).unwrap();
            journal.append(&entry(1, 10.0)).unwrap();
            journal.append(&filled).unwrap();
        }
        let recovered = Journal::recover(&path).unwrap();
        assert_eq!(recovered.entries.len(), 2, "the fill did not survive");
        assert!(
            recovered.entries.contains(&filled),
            "the fill came back as something else: {:?}",
            recovered.entries
        );
    }

    /// The pulpit executable this test run built, beside the test binary
    /// itself — the same trick `crates/pulpit/tests/document_worker.rs` uses,
    /// since a document worker is a role of *this* binary (§5.1) and
    /// `std::env::current_exe()` from a unit test is the test binary.
    fn worker_executable() -> Option<PathBuf> {
        let test_binary = std::env::current_exe().ok()?;
        let candidate = test_binary.parent()?.parent()?.join(if cfg!(windows) {
            "pulpit.exe"
        } else {
            "pulpit"
        });
        candidate.is_file().then_some(candidate)
    }

    fn worker_command(
        source: &Path,
    ) -> Option<pulpit_render::document::session::DocumentWorkerCommand> {
        let program = worker_executable()?;
        if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
            let lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib");
            std::env::set_var("PULPIT_PDFIUM_PATH", lib);
        }
        Some(
            pulpit_render::document::session::DocumentWorkerCommand::Explicit {
                program,
                args: vec![format!("--document-worker={}", source.display())],
            },
        )
    }

    fn ink_stroke() -> DocumentTransaction {
        DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
            InkDraft {
                page: PageIndex(0),
                points: vec![InkPoint::new(72.0, 72.0), InkPoint::new(180.0, 140.0)],
                style: MarkStyle::default(),
            },
        ))])
    }

    /// §76.2, end to end against a real document worker: draw, undo, crash,
    /// recover — the mark MUST stay absent.
    ///
    /// This is the seam §83.1 asks for: it drives the journal (this module)
    /// and the document worker (`pulpit-render`) together, the same two
    /// halves that disagreed about which operation a `Reversed` entry means.
    /// `PendingEdit.reversal` in `app.rs` — the fix this test is a regression
    /// guard for — is exactly the operation captured here as `sent_as_undo`:
    /// the one actually sent to the worker's `Ask::Undo`, not
    /// `Applied.undo` from the *answer* to that undo, which is the redo of
    /// it. Journalling the latter (the bug) is reproduced below and shown to
    /// bring the mark back; journalling the former (the fix) does not.
    #[test]
    fn drawing_then_undoing_then_recovering_leaves_the_mark_absent() {
        use pulpit_render::document::protocol::{DocumentRequest, DocumentResponse};
        use pulpit_render::document::session::DocumentSession;
        use pulpit_render::document::{DocumentRevision, DocumentUndo};

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.pdf");
        if pulpit_render::pdf::synth::write_pdf(&source, 1, None).is_err() {
            eprintln!("skipping: cannot write a synthetic PDF");
            return;
        }
        let Some(command) = worker_command(&source) else {
            eprintln!("skipping: the pulpit executable was not built beside this test");
            return;
        };

        let mut session = match DocumentSession::start(&command, &source) {
            Ok(session) => session,
            Err(error) => {
                eprintln!("skipping the recovery test: {error}");
                return;
            }
        };

        let annotations = |session: &mut DocumentSession| {
            let DocumentResponse::Annotations(list) = session
                .request(DocumentRequest::ListAnnotations { page: PageIndex(0) })
                .expect("the worker answers")
            else {
                panic!("expected an annotation list")
            };
            list.len()
        };
        assert_eq!(annotations(&mut session), 0);

        // Draw. `sent_as_undo` is what `PendingEdit.reversal` holds in the
        // real application: the operation that will be sent to reverse this,
        // captured at the moment the mark was applied — not derived from
        // whatever answer comes back later.
        let response = session
            .request(DocumentRequest::Apply {
                expected_revision: DocumentRevision::INITIAL,
                transaction: ink_stroke(),
            })
            .expect("the stroke commits");
        let DocumentResponse::Applied(applied) = response else {
            panic!("expected an applied transaction, got {response:?}")
        };
        let sent_as_undo: DocumentUndo = applied.undo.clone();
        assert_eq!(annotations(&mut session), 1);

        // Undo. The answer's own `.undo` is the *inverse of the undo* — the
        // redo — which is the value §76.2 found being journalled by mistake.
        let response = session
            .request(DocumentRequest::Undo {
                expected_revision: applied.document_revision,
                operation: sent_as_undo.clone(),
            })
            .expect("the undo commits");
        let DocumentResponse::Applied(undone) = response else {
            panic!("expected an applied undo, got {response:?}")
        };
        let redo_of_the_undo: DocumentUndo = undone.undo.clone();
        assert_eq!(annotations(&mut session), 0, "the undo removed the mark");
        session.close();

        // Recover with the fix: journal the operation that was sent.
        let path = directory.path().join("journal.jsonl");
        let mut journal = Journal::start(
            &path,
            &source,
            DocumentFingerprint {
                path: source.clone(),
                size: None,
                modified_unix: None,
            },
        )
        .unwrap();
        journal
            .append(&JournalEntry::Applied {
                revision: applied.document_revision,
                transaction: ink_stroke(),
            })
            .unwrap();
        journal
            .append(&JournalEntry::Reversed {
                revision: undone.document_revision,
                operation: Box::new(sent_as_undo.clone()),
            })
            .unwrap();
        let recovered = Journal::recover(&path).expect("the journal recovers");

        let mut replay = DocumentSession::start(&command, &source).unwrap();
        let mut revision = DocumentRevision::INITIAL;
        for entry in recovered.in_order() {
            match entry {
                JournalEntry::Applied { transaction, .. } => {
                    let DocumentResponse::Applied(applied) = replay
                        .request(DocumentRequest::Apply {
                            expected_revision: revision,
                            transaction,
                        })
                        .unwrap()
                    else {
                        panic!("expected an applied transaction")
                    };
                    revision = applied.document_revision;
                }
                JournalEntry::Reversed { operation, .. } => {
                    let DocumentResponse::Applied(applied) = replay
                        .request(DocumentRequest::Undo {
                            expected_revision: revision,
                            operation: *operation,
                        })
                        .unwrap()
                    else {
                        panic!("expected an applied undo")
                    };
                    revision = applied.document_revision;
                }
            }
        }
        assert_eq!(
            annotations(&mut replay),
            0,
            "§76.2: journalling the operation that was sent leaves the undo undone"
        );
        replay.close();

        // And the bug this replaces: journalling the *answer's* `.undo`
        // (the redo) instead brings the mark back, which is the defect
        // §76.2 records and this test exists to keep fixed.
        let mut buggy_replay = DocumentSession::start(&command, &source).unwrap();
        let mut revision = DocumentRevision::INITIAL;
        let DocumentResponse::Applied(applied) = buggy_replay
            .request(DocumentRequest::Apply {
                expected_revision: revision,
                transaction: ink_stroke(),
            })
            .unwrap()
        else {
            panic!("expected an applied transaction")
        };
        revision = applied.document_revision;
        let DocumentResponse::Applied(applied) = buggy_replay
            .request(DocumentRequest::Undo {
                expected_revision: revision,
                // The bug: the *answer's* undo, i.e. `redo_of_the_undo`.
                operation: redo_of_the_undo,
            })
            .unwrap()
        else {
            panic!("expected an applied undo")
        };
        let _ = applied;
        assert_eq!(
            annotations(&mut buggy_replay),
            1,
            "the bug this test guards against: replaying the redo of the \
             undo brings the mark back"
        );
        buggy_replay.close();
    }

    #[test]
    fn a_journal_written_before_selections_were_recorded_still_replays() {
        // `selected` was added to the command after the format existed, so a
        // line written without it must still parse — as a fill that chose
        // nothing, which is what every field except a multi-select is.
        let line = r#"{"applied":{"revision":3,"transaction":[{"set-field":{"name":"name","value":"Ada"}}]}}"#;
        let parsed: JournalEntry = serde_json::from_str(line).expect("an older line still parses");
        assert_eq!(
            parsed,
            entry_for_committed_field(&pulpit_render::document::protocol::CommittedField {
                revision: DocumentRevision(3),
                ..committed("name", "Ada", Vec::new())
            })
            .unwrap()
        );
    }
}
