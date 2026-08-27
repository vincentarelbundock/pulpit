//! Crash recovery: what the last run was doing, and how it is restored.
//!
//! Pulpit writes a small snapshot of the live session beside the settings
//! file while it runs, and deletes that file on a clean quit. The presence of
//! a snapshot at startup therefore means exactly one thing: the previous run
//! ended without going through [`crate::app::App::quit`] — a crash, a killed
//! process, a power cut mid-talk.
//!
//! Two rules shape everything here.
//!
//! 1. **Planning is inert.** The snapshot is turned into a [`RestorePlan`];
//!    constructing one cannot move the audience, blank it, or reassign a
//!    display. Only [`RestorePlan::apply_to`] does that during startup.
//! 2. **The plan is honest.** A slide index means nothing if the deck was
//!    rebuilt under us, so the plan checks a fingerprint of the file and
//!    restores only the parts that still apply.
//!
//! The snapshot is JSON rather than the TOML the settings use: it is machine
//! state written unattended, never hand-edited, and JSON has no rule about
//! values preceding tables to trip over as fields are added.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use pulpit_core::{Blank, Command as Nav, NotesMapping, PresentationState, SlideIndex, Timer};
use pulpit_display::DisplayRoles;
use serde::{Deserialize, Serialize};

/// Current session-snapshot schema version. Bump it when a field changes
/// meaning; an unrecognised version is discarded rather than guessed at.
pub const SCHEMA_VERSION: u32 = 1;

// ------------------------------------------------------------------- model

/// Enough of a file to notice that it is not the file we were presenting.
///
/// Modification time and size, not a content hash: a snapshot is written
/// while a talk is in progress and must cost nothing, and a rebuilt deck
/// always changes at least one of the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFingerprint {
    pub path: PathBuf,
    /// Seconds since the Unix epoch, absent when the platform will not say.
    pub modified_unix: Option<u64>,
    pub size: Option<u64>,
}

impl DocumentFingerprint {
    /// Whether this describes the same bytes as `other`.
    ///
    /// A fingerprint whose metadata could not be read matches only on the
    /// path: refusing to match at all would mean never restoring a slide on a
    /// filesystem that hides timestamps, and matching regardless would mean
    /// restoring a slide index into a document that has since been rebuilt.
    /// Matching on the path alone is the honest middle: the file is the same
    /// file, and we say the deck is unchanged only when we have evidence.
    pub fn matches(&self, other: &DocumentFingerprint) -> bool {
        self.path == other.path
            && self.modified_unix == other.modified_unix
            && self.size == other.size
    }
}

/// The clock, flattened to values that survive a process restart.
///
/// [`Timer`] holds an [`Instant`], which is meaningless in another process,
/// so the running timer is stored as "this much elapsed, and it was ticking".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TimerSnapshot {
    pub elapsed_secs: u64,
    pub target_secs: Option<u64>,
    pub running: bool,
}

impl TimerSnapshot {
    pub fn of(timer: &Timer, now: Instant) -> TimerSnapshot {
        TimerSnapshot {
            elapsed_secs: timer.elapsed(now).as_secs(),
            target_secs: timer.target.map(|target| target.as_secs()),
            running: timer.is_running(),
        }
    }

    /// Rebuild a timer showing this elapsed time as of `now`.
    ///
    /// The elapsed time is reconstructed by starting the clock in the past;
    /// a platform that refuses the subtraction simply restores a timer that
    /// starts from where it was rather than panicking mid-recovery.
    pub fn to_timer(self, now: Instant) -> Timer {
        let mut timer = Timer::new(self.target_secs.map(Duration::from_secs));
        let elapsed = Duration::from_secs(self.elapsed_secs);
        timer.start(now.checked_sub(elapsed).unwrap_or(now));
        if !self.running {
            timer.pause(now);
        }
        timer
    }
}

/// Everything worth recovering from an interrupted talk.
///
/// Pure data: it touches no filesystem, holds no `Instant`, and knows nothing
/// about windows. That is what makes the restore decision testable without a
/// desktop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSnapshot {
    pub schema: u32,
    /// Seconds since the Unix epoch, so the offer can say how old it is.
    pub saved_at: u64,
    pub document: Option<DocumentFingerprint>,
    /// The slide the audience was on.
    pub committed: SlideIndex,
    /// The slide the presenter was looking at, which may be a different one.
    pub preview: SlideIndex,
    pub timer: TimerSnapshot,
    pub blank: Blank,
    /// The presenter layout in use, by identifier.
    pub layout: Option<String>,
    pub mapping: NotesMapping,
    pub roles: DisplayRoles,
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            saved_at: 0,
            document: None,
            committed: 0,
            preview: 0,
            timer: TimerSnapshot::default(),
            blank: Blank::Off,
            layout: None,
            mapping: NotesMapping::default(),
            roles: DisplayRoles::default(),
        }
    }
}

impl SessionSnapshot {
    /// Capture the live session. `now` is the caller's clock so the capture
    /// stays deterministic in tests.
    pub fn capture(
        state: &PresentationState,
        layout: Option<String>,
        roles: &DisplayRoles,
        fingerprint: Option<DocumentFingerprint>,
        now: Instant,
    ) -> SessionSnapshot {
        SessionSnapshot {
            schema: SCHEMA_VERSION,
            saved_at: unix_now(),
            document: fingerprint,
            committed: state.committed(),
            preview: state.preview(),
            timer: TimerSnapshot::of(state.timer(), now),
            blank: state.blank(),
            layout,
            mapping: state.mapping().clone(),
            roles: roles.clone(),
        }
    }

    /// Whether two snapshots describe the same session, ignoring when they
    /// were taken. A talk that is sitting still on one slide must not rewrite
    /// the file every interval just because the wall clock moved.
    ///
    /// The timer's elapsed seconds get thirty seconds of slack: a running
    /// clock would otherwise make every snapshot "different" and force a
    /// disk write per interval for the whole talk. Recovery after a crash
    /// may therefore restore a clock up to half a minute behind — a price a
    /// crash can charge; a healthy talk paying an fsync every two seconds
    /// is not.
    pub fn matches_content(&self, other: &SessionSnapshot) -> bool {
        self.schema == other.schema
            && self.document == other.document
            && self.committed == other.committed
            && self.preview == other.preview
            && self.blank == other.blank
            && self.layout == other.layout
            && self.mapping == other.mapping
            && self.roles == other.roles
            && self.timer.running == other.timer.running
            && self.timer.target_secs == other.timer.target_secs
            && self.timer.elapsed_secs.abs_diff(other.timer.elapsed_secs) < 30
    }

    /// Whether there is anything here worth restoring. A snapshot of an
    /// empty session with a stopped clock is not meaningful recovery.
    pub fn is_worth_offering(&self) -> bool {
        self.document.is_some()
            || self.committed != 0
            || self.timer.elapsed_secs > 0
            || self.blank.is_blanked()
    }

    /// Decide what may be restored, given what the document looks like *now*.
    ///
    /// `current` is the fingerprint taken at startup, or `None` when the file
    /// has gone. Nothing here changes anything: the result is a description
    /// of the recovery, and applying it is a separate step.
    pub fn plan(&self, current: Option<&DocumentFingerprint>) -> RestorePlan {
        let document = match (&self.document, current) {
            (None, _) => DocumentStatus::NoDocument,
            (Some(_), None) => DocumentStatus::Missing,
            (Some(stored), Some(current)) if stored.matches(current) => DocumentStatus::Unchanged,
            (Some(_), Some(_)) => DocumentStatus::Changed,
        };
        RestorePlan {
            document_status: document,
            snapshot: self.clone(),
        }
    }
}

/// What became of the document the snapshot was taken against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentStatus {
    /// Same path, same bytes as far as the fingerprint can tell.
    Unchanged,
    /// The file is there but has been rebuilt: slide indices are meaningless.
    Changed,
    /// The file is gone.
    Missing,
    /// The interrupted session had no document open.
    NoDocument,
}

impl DocumentStatus {
    /// Whether a slide index taken against this document still means anything.
    fn slides_are_meaningful(self) -> bool {
        matches!(self, DocumentStatus::Unchanged)
    }
}

/// A recovery plan, not an action.
///
/// Holding one has no effect on the presentation. Everything it can change
/// happens inside [`RestorePlan::apply_to`] and [`RestorePlan::document`],
/// which the application calls during startup.
#[derive(Debug, Clone, PartialEq)]
pub struct RestorePlan {
    pub document_status: DocumentStatus,
    snapshot: SessionSnapshot,
}

impl RestorePlan {
    /// The document to reopen, or `None` when the deck cannot be trusted.
    pub fn document(&self) -> Option<&Path> {
        if !self.document_status.slides_are_meaningful() {
            return None;
        }
        self.snapshot.document.as_ref().map(|d| d.path.as_path())
    }

    /// Whether this plan is about `path` — the document a launch named.
    ///
    /// A path given on the command line is what someone double-clicked, and
    /// it decides whether an interrupted session is a recovery or an
    /// interruption: restoring is right when the plan is about that same
    /// file, and wrong when it is about a different one.
    ///
    /// The stored path came from a previous process and the given one from a
    /// file manager, so they can name one file two ways — relative against a
    /// different working directory, or through a symlink. Equality is tried
    /// first because it needs no filesystem, and canonical form only when it
    /// disagrees; a path that cannot be canonicalised (it has been deleted,
    /// or is not readable) answers "not the same file", which reopens the
    /// named document rather than restoring over it.
    pub fn is_about(&self, path: &Path) -> bool {
        let Some(stored) = self.snapshot.document.as_ref().map(|d| d.path.as_path()) else {
            return false;
        };
        if stored == path {
            return true;
        }
        match (stored.canonicalize(), path.canonicalize()) {
            (Ok(stored), Ok(named)) => stored == named,
            _ => false,
        }
    }

    /// The slide pair to return to, or `None` when the deck changed.
    pub fn slides(&self) -> Option<(SlideIndex, SlideIndex)> {
        self.document_status
            .slides_are_meaningful()
            .then_some((self.snapshot.committed, self.snapshot.preview))
    }

    /// Blanking is only restored alongside a slide: a black audience screen
    /// with no deck behind it is a puzzle, not a recovery.
    pub fn blank(&self) -> Blank {
        if self.document_status.slides_are_meaningful() {
            self.snapshot.blank
        } else {
            Blank::Off
        }
    }

    pub fn mapping(&self) -> Option<&NotesMapping> {
        self.document_status
            .slides_are_meaningful()
            .then_some(&self.snapshot.mapping)
    }

    /// The layout identifier survives any document change: it describes the
    /// presenter's own screen, not the deck.
    pub fn layout(&self) -> Option<&str> {
        self.snapshot.layout.as_deref()
    }

    pub fn roles(&self) -> &DisplayRoles {
        &self.snapshot.roles
    }

    /// Apply everything that lives in presentation state.
    ///
    /// This is the only function in this module that can move the audience,
    /// and the application reaches it from a single confirmed message. The
    /// document itself is reopened by the caller — see [`RestorePlan::document`]
    /// — because loading a PDF is not a pure operation.
    pub fn apply_to(&self, state: &mut PresentationState, roles: &mut DisplayRoles, now: Instant) {
        if let Some(mapping) = self.mapping() {
            state.apply(Nav::SetNotesMapping(mapping.clone()), now);
        }
        if let Some((committed, preview)) = self.slides() {
            state.apply(Nav::GoTo(committed), now);
            state.apply(Nav::PreviewGoTo(preview), now);
        }
        state.apply(Nav::SetBlank(self.blank()), now);
        *state.timer_mut() = self.snapshot.timer.to_timer(now);
        *roles = self.snapshot.roles.clone();
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------- throttle

/// How often the session snapshot may be rewritten.
///
/// The update loop ticks every 50 ms; writing there would be twenty small
/// fsynced writes a second for the whole of a talk, which is a lot of disk
/// traffic to protect against an event that happens approximately never. Two
/// seconds is the other end of the trade: the most a crash can cost is the
/// last two seconds of navigation — at worst one slide — while a presenter
/// hammering the arrow keys still produces at most one write every two
/// seconds instead of one per keypress.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(2);

/// A "not more often than this" gate.
#[derive(Debug, Clone)]
pub struct SaveThrottle {
    interval: Duration,
    last: Option<Instant>,
}

impl Default for SaveThrottle {
    fn default() -> Self {
        Self::new(SAVE_INTERVAL)
    }
}

impl SaveThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// Whether a write is allowed now, recording it if so. The first call
    /// always allows one: a crash seconds after launch should still leave the
    /// document and layout recoverable.
    pub fn due(&mut self, now: Instant) -> bool {
        let due = match self.last {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= self.interval,
        };
        if due {
            self.last = Some(now);
        }
        due
    }
}

// ------------------------------------------------------------------- store

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot parse the session snapshot: {0}")]
    Parse(String),
    #[error("cannot serialise the session snapshot: {0}")]
    Serialise(String),
    #[error("session schema {found} is not {known}")]
    UnknownSchema { found: u32, known: u32 },
}

/// The snapshot this process writes.
///
/// One file per copy, named after the process, because several copies of
/// pulpit may be running: a shared name would have two of them overwriting
/// each other's crash record, and a clean quit in one deleting the other's.
pub fn snapshot_name(pid: u32) -> String {
    format!("session-{pid}.json")
}

/// The pid a snapshot file is named after, or `None` for anything else in
/// the directory.
fn snapshot_pid(name: &str) -> Option<u32> {
    name.strip_prefix("session-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

/// Snapshots left behind by copies that are no longer running.
///
/// `is_live` answers whether the process that wrote a snapshot still exists;
/// a snapshot whose writer is alive is that copy's business, and taking it
/// would offer a running session back as a crashed one. This process's own
/// snapshot is never abandoned, whatever the answer.
pub fn abandoned_snapshots(directory: &Path, is_live: impl Fn(u32) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = match std::fs::read_dir(directory) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let pid = snapshot_pid(name.to_str()?)?;
                (pid != std::process::id() && !is_live(pid)).then(|| (pid, entry.path()))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    // Oldest process first, so a machine that crashed twice replays in a
    // stable order rather than whatever the directory happens to yield.
    found.sort_by_key(|(pid, _)| *pid);
    let mut paths: Vec<PathBuf> = found.into_iter().map(|(_, path)| path).collect();
    // The name used before snapshots were per-process. Whoever wrote it can
    // only be a version that is no longer running, and dropping it silently
    // would lose an interrupted session across an upgrade.
    let legacy = directory.join("session.json");
    if legacy.is_file() {
        paths.insert(0, legacy);
    }
    paths
}

/// The snapshot file, written atomically beside the settings.
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new(
            crate::settings::store::config_directory().join(snapshot_name(std::process::id())),
        )
    }
}

impl SessionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The snapshot left by an unclean exit, if there is a usable one.
    ///
    /// Anything unreadable is treated as "no snapshot": a corrupt recovery
    /// file must never stand between a presenter and a working application.
    pub fn load(&self) -> Option<SessionSnapshot> {
        match self.try_load() {
            Ok(snapshot) => Some(snapshot),
            Err(e) => {
                if self.path.exists() {
                    tracing::warn!(path = %self.path.display(), error = %e,
                        "ignoring an unusable session snapshot");
                }
                None
            }
        }
    }

    fn try_load(&self) -> Result<SessionSnapshot, SessionError> {
        let text = std::fs::read_to_string(&self.path)?;
        let snapshot: SessionSnapshot =
            serde_json::from_str(&text).map_err(|e| SessionError::Parse(e.to_string()))?;
        // Unlike settings there is no migration path: a snapshot describes a
        // single interrupted run and is worth nothing once it is stale.
        if snapshot.schema != SCHEMA_VERSION {
            return Err(SessionError::UnknownSchema {
                found: snapshot.schema,
                known: SCHEMA_VERSION,
            });
        }
        Ok(snapshot)
    }

    /// Write the snapshot atomically, through the one primitive every writer
    /// in pulpit uses — a crash during the write of a crash-recovery file
    /// must not be what destroys the recovery.
    pub fn save(&self, snapshot: &SessionSnapshot) -> Result<(), SessionError> {
        // `capture` stamps the schema; serialising the borrow directly saves
        // cloning the mapping and roles on every periodic save.
        debug_assert_eq!(snapshot.schema, SCHEMA_VERSION);
        let text = serde_json::to_string_pretty(snapshot)
            .map_err(|e| SessionError::Serialise(e.to_string()))?;

        let directory = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)?;

        crate::platform::paths::write_atomically(&self.path, text.as_bytes())?;
        Ok(())
    }

    /// Forget the interrupted session. Called on a clean quit and before a
    /// restore is applied, so a failed restore is never replayed forever.
    pub fn clear(&self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %self.path.display(), error = %e,
                    "cannot remove the session snapshot");
            }
        }
    }
}

/// Fingerprint a document on disk. `None` when it cannot be read at all,
/// which tells the restore plan which document state can still be trusted.
pub fn fingerprint(path: &Path) -> Option<DocumentFingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(DocumentFingerprint {
        path: path.to_path_buf(),
        modified_unix: metadata.modified().ok().and_then(|time| {
            time.duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .map(|since| since.as_secs())
        }),
        size: Some(metadata.len()),
    })
}

/// Identify a document by what is in it rather than by where it is.
///
/// The lowercase hex BLAKE3 of the file's bytes, or `None` when it cannot be
/// read — an unreadable file simply has no remembered preferences, which is
/// the same as a file that has never been opened before.
///
/// By contents so that moving, renaming or copying a document keeps whatever
/// the user chose for it, and so that the *same* document fetched twice from
/// two places is one document as far as those choices go. [`fingerprint`]
/// answers a different question — "is this the same file, unmodified, that we
/// were reading an hour ago" — and deliberately notices an edit in place,
/// which is what makes it right for crash recovery and wrong here.
///
/// This reads the whole file, on the thread that asked. It runs once per open,
/// where the surrounding work is starting worker processes and parsing a PDF,
/// and BLAKE3 moves at gigabytes a second: a deck costs under a millisecond
/// and a large scanned book costs tens.
pub fn content_hash(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::{DocumentId, DocumentInfo};
    use pulpit_display::{IdentityRecord, MonitorIdentity, RoleTarget};

    /// A snapshot belongs to the copy that wrote it. Only what a departed copy
    /// left behind is a crash record anyone else may take.
    #[test]
    fn only_a_departed_copys_snapshot_is_abandoned() {
        let directory = tempfile::tempdir().unwrap();
        let live = 4242;
        let gone = 4243;
        for pid in [live, gone, std::process::id()] {
            std::fs::write(directory.path().join(snapshot_name(pid)), "{}").unwrap();
        }
        // Not a snapshot at all, and never collected as one.
        std::fs::write(directory.path().join("settings.toml"), "").unwrap();

        let abandoned = abandoned_snapshots(directory.path(), |pid| pid == live);
        assert_eq!(
            abandoned,
            vec![directory.path().join(snapshot_name(gone))],
            "a live copy's snapshot and this copy's own are left alone"
        );
    }

    /// Two crashes, two snapshots: the order they are offered in is the order
    /// they were written in, not whatever the directory yields.
    #[test]
    fn abandoned_snapshots_are_offered_in_a_stable_order() {
        let directory = tempfile::tempdir().unwrap();
        for pid in [900u32, 100, 500] {
            std::fs::write(directory.path().join(snapshot_name(pid)), "{}").unwrap();
        }
        let abandoned = abandoned_snapshots(directory.path(), |_| false);
        let names: Vec<String> = abandoned
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![snapshot_name(100), snapshot_name(500), snapshot_name(900)]
        );
    }

    /// The name snapshots had before they were per-process. An upgrade must
    /// not lose an interrupted session that was written by the old one.
    #[test]
    fn a_snapshot_from_before_the_rename_is_still_offered() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("session.json"), "{}").unwrap();
        std::fs::write(directory.path().join(snapshot_name(4243)), "{}").unwrap();

        let abandoned = abandoned_snapshots(directory.path(), |_| false);
        assert_eq!(
            abandoned.first(),
            Some(&directory.path().join("session.json")),
            "the older record is offered before the newer one"
        );
        assert_eq!(abandoned.len(), 2);
    }

    /// A directory that is not there is not an error: it is a machine that has
    /// never run pulpit.
    #[test]
    fn a_missing_directory_holds_no_abandoned_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        assert!(abandoned_snapshots(&directory.path().join("nothing"), |_| false).is_empty());
    }

    fn interrupted_state(pages: usize) -> PresentationState {
        let mut state = PresentationState::new(
            DocumentInfo::new(DocumentId(1), "/decks/talk.pdf", pages),
            NotesMapping::SlidesOnly,
        );
        let now = Instant::now();
        state.apply(Nav::GoTo(11), now);
        state.apply(Nav::PreviewGoTo(13), now);
        state.apply(Nav::SetBlank(Blank::Black), now);
        state.apply(Nav::StartTimer, now);
        state
    }

    fn roles_with_a_chosen_audience() -> DisplayRoles {
        DisplayRoles {
            audience: RoleTarget::Monitor(Box::new(IdentityRecord::new(
                MonitorIdentity::Session { handle: 7 },
            ))),
            audience_fullscreen: false,
            ..DisplayRoles::default()
        }
    }

    fn snapshot_of_an_interrupted_talk() -> SessionSnapshot {
        let mut snapshot = SessionSnapshot::capture(
            &interrupted_state(40),
            Some("wide".into()),
            &roles_with_a_chosen_audience(),
            Some(DocumentFingerprint {
                path: "/decks/talk.pdf".into(),
                modified_unix: Some(1_700_000_000),
                size: Some(4096),
            }),
            Instant::now(),
        );
        snapshot.timer.elapsed_secs = 754;
        snapshot
    }

    fn unchanged_fingerprint() -> DocumentFingerprint {
        DocumentFingerprint {
            path: "/decks/talk.pdf".into(),
            modified_unix: Some(1_700_000_000),
            size: Some(4096),
        }
    }

    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("session.json"));
        (dir, store)
    }

    /// The property the remembered-layout list depends on: identity follows
    /// the bytes, not the name, so a renamed file is the same document and an
    /// edited one is not.
    #[test]
    fn a_documents_hash_follows_its_contents_and_not_its_name() {
        let directory = tempfile::tempdir().unwrap();
        let one = directory.path().join("talk.pdf");
        let copy = directory.path().join("talk-renamed.pdf");
        let other = directory.path().join("other.pdf");
        std::fs::write(&one, b"%PDF-1.7 the same bytes").unwrap();
        std::fs::write(&copy, b"%PDF-1.7 the same bytes").unwrap();
        std::fs::write(&other, b"%PDF-1.7 different bytes").unwrap();

        let hash = content_hash(&one).expect("a readable file hashes");
        assert_eq!(content_hash(&copy).as_deref(), Some(hash.as_str()));
        assert_ne!(content_hash(&other), Some(hash.clone()));
        assert_eq!(hash.len(), 64, "lowercase hex BLAKE3");

        // An unreadable file has no identity, which is the same as never
        // having been opened.
        assert_eq!(content_hash(&directory.path().join("absent.pdf")), None);
    }

    #[test]
    fn a_snapshot_round_trips_through_disk() {
        let (_dir, store) = store();
        let snapshot = snapshot_of_an_interrupted_talk();
        store.save(&snapshot).unwrap();
        assert_eq!(store.load(), Some(snapshot));
    }

    #[test]
    fn a_snapshot_from_another_schema_is_refused_rather_than_misread() {
        let (_dir, store) = store();
        let mut snapshot = snapshot_of_an_interrupted_talk();
        store.save(&snapshot).unwrap();
        snapshot.schema = SCHEMA_VERSION + 1;
        let text = serde_json::to_string(&snapshot).unwrap();
        std::fs::write(&store.path, text).unwrap();
        assert_eq!(store.load(), None, "no offer beats a misread offer");
    }

    #[test]
    fn a_corrupt_snapshot_is_ignored_rather_than_fatal() {
        let (_dir, store) = store();
        std::fs::write(&store.path, "{not json").unwrap();
        assert_eq!(store.load(), None);
    }

    #[test]
    fn no_snapshot_means_the_last_run_exited_cleanly() {
        let (_dir, store) = store();
        assert_eq!(store.load(), None);
    }

    #[test]
    fn clearing_the_snapshot_leaves_nothing_to_restore() {
        let (_dir, store) = store();
        store.save(&snapshot_of_an_interrupted_talk()).unwrap();
        store.clear();
        assert!(!store.path.exists());
        assert_eq!(store.load(), None);
        // Clearing again is what a second clean quit does; it must be quiet.
        store.clear();
    }

    #[test]
    fn a_changed_document_is_detected_by_its_fingerprint() {
        let snapshot = snapshot_of_an_interrupted_talk();
        let rebuilt = DocumentFingerprint {
            size: Some(8192),
            ..unchanged_fingerprint()
        };
        assert_eq!(
            snapshot.plan(Some(&rebuilt)).document_status,
            DocumentStatus::Changed
        );

        let touched = DocumentFingerprint {
            modified_unix: Some(1_700_000_999),
            ..unchanged_fingerprint()
        };
        assert_eq!(
            snapshot.plan(Some(&touched)).document_status,
            DocumentStatus::Changed
        );
        assert_eq!(
            snapshot
                .plan(Some(&unchanged_fingerprint()))
                .document_status,
            DocumentStatus::Unchanged
        );
        assert_eq!(snapshot.plan(None).document_status, DocumentStatus::Missing);
    }

    #[test]
    fn a_changed_document_is_never_offered_a_slide_index() {
        let snapshot = snapshot_of_an_interrupted_talk();
        for current in [
            Some(DocumentFingerprint {
                size: Some(1),
                ..unchanged_fingerprint()
            }),
            None,
        ] {
            let plan = snapshot.plan(current.as_ref());
            assert_eq!(plan.slides(), None);
            assert_eq!(plan.document(), None);
            assert_eq!(plan.mapping(), None);
            assert_eq!(plan.blank(), Blank::Off, "and never a blanked audience");
            assert_eq!(
                plan.layout(),
                Some("wide"),
                "the presenter's own screen still applies"
            );
            assert_eq!(plan.snapshot.timer.elapsed_secs, 754);
        }
    }

    #[test]
    fn an_unconfirmed_restore_changes_nothing_the_audience_can_see() {
        // The whole point of the feature: a snapshot may be loaded, planned
        // and described without the audience learning anything about it.
        let (_dir, store) = store();
        store.save(&snapshot_of_an_interrupted_talk()).unwrap();

        let snapshot = store.load().expect("the crashed run left one");
        let plan = snapshot.plan(Some(&unchanged_fingerprint()));
        let _ = plan.document();
        let _ = plan.slides();

        let mut state = PresentationState::default();
        let mut roles = DisplayRoles::default();
        let fresh_state = state.clone();
        let fresh_roles = roles.clone();

        assert_eq!(state, fresh_state, "the audience is on nothing");
        assert_eq!(state.blank(), Blank::Off, "and is not blanked");
        assert_eq!(roles, fresh_roles, "and no display was reassigned");

        // Only the explicit, confirmed step touches any of it.
        plan.apply_to(&mut state, &mut roles, Instant::now());
        assert_ne!(roles, fresh_roles);
        assert!(matches!(roles.audience, RoleTarget::Monitor(_)));
    }

    #[test]
    fn a_confirmed_restore_puts_the_talk_back_where_it_was() {
        let snapshot = snapshot_of_an_interrupted_talk();
        let plan = snapshot.plan(Some(&unchanged_fingerprint()));
        assert_eq!(plan.document(), Some(Path::new("/decks/talk.pdf")));

        let now = Instant::now();
        let mut state = PresentationState::new(
            DocumentInfo::new(DocumentId(2), "/decks/talk.pdf", 40),
            NotesMapping::SlidesOnly,
        );
        let mut roles = DisplayRoles::default();
        plan.apply_to(&mut state, &mut roles, now);

        assert_eq!(state.committed(), 11);
        assert_eq!(state.preview(), 13);
        assert_eq!(state.blank(), Blank::Black);
        assert!(state.timer().is_running());
        assert_eq!(
            state.timer().elapsed(now).as_secs(),
            754,
            "the clock resumes where the crash left it"
        );
    }

    #[test]
    fn a_paused_clock_is_restored_paused() {
        let snapshot = SessionSnapshot {
            timer: TimerSnapshot {
                elapsed_secs: 90,
                target_secs: Some(1200),
                running: false,
            },
            ..SessionSnapshot::default()
        };
        let now = Instant::now();
        let timer = snapshot.timer.to_timer(now);
        assert!(!timer.is_running());
        assert_eq!(timer.elapsed(now).as_secs(), 90);
        assert_eq!(
            timer.elapsed(now + Duration::from_secs(60)).as_secs(),
            90,
            "a paused clock does not run on after recovery"
        );
        assert_eq!(timer.target, Some(Duration::from_secs(1200)));
    }

    #[test]
    fn the_throttle_writes_at_most_once_per_interval() {
        let start = Instant::now();
        let mut throttle = SaveThrottle::new(Duration::from_secs(2));
        assert!(throttle.due(start), "the first write is always allowed");

        // A tick every 50 ms for just under the interval: no second write.
        let mut writes = 0;
        for step in 1..40 {
            if throttle.due(start + Duration::from_millis(50 * step)) {
                writes += 1;
            }
        }
        assert_eq!(writes, 0, "navigation storms do not become disk storms");

        assert!(throttle.due(start + Duration::from_secs(2)));
        assert!(!throttle.due(start + Duration::from_secs(3)));
        assert!(throttle.due(start + Duration::from_secs(4)));
    }

    #[test]
    fn a_session_sitting_still_is_not_rewritten() {
        let snapshot = snapshot_of_an_interrupted_talk();
        let later = SessionSnapshot {
            saved_at: snapshot.saved_at + 600,
            ..snapshot.clone()
        };
        assert!(snapshot.matches_content(&later), "only the clock moved");

        let advanced = SessionSnapshot {
            committed: snapshot.committed + 1,
            ..later
        };
        assert!(!snapshot.matches_content(&advanced), "a slide moved");
    }

    #[test]
    fn an_empty_session_is_not_worth_offering_back() {
        assert!(!SessionSnapshot::default().is_worth_offering());
        assert!(snapshot_of_an_interrupted_talk().is_worth_offering());
        assert!(SessionSnapshot {
            timer: TimerSnapshot {
                elapsed_secs: 30,
                ..TimerSnapshot::default()
            },
            ..SessionSnapshot::default()
        }
        .is_worth_offering());
    }

    #[test]
    fn a_partial_write_is_never_visible() {
        let (_dir, store) = store();
        let snapshot = snapshot_of_an_interrupted_talk();
        store.save(&snapshot).unwrap();
        // A crash mid-save leaves a temporary file that was never renamed.
        std::fs::write(store.path.with_extension("json.tmp"), "{\"schema\"").unwrap();
        assert_eq!(store.load(), Some(snapshot));
    }

    /// An interrupted session that was looking at `document`, or at nothing.
    fn snapshot_about(document: Option<DocumentFingerprint>) -> SessionSnapshot {
        SessionSnapshot::capture(
            &interrupted_state(40),
            Some("wide".into()),
            &roles_with_a_chosen_audience(),
            document,
            Instant::now(),
        )
    }

    /// The bug this closes: double-clicking a PDF opened the *previous*
    /// document, because a surviving snapshot was applied before the path the
    /// launch was given was ever looked at.
    #[test]
    fn a_plan_is_only_about_the_document_it_holds() {
        let snapshot = snapshot_about(Some(unchanged_fingerprint()));
        let plan = snapshot.plan(Some(&unchanged_fingerprint()));

        assert!(
            plan.is_about(Path::new("/decks/talk.pdf")),
            "the plan is about its own document, so a launch naming that file \
             is a recovery and the page and clock are worth restoring"
        );
        assert!(
            !plan.is_about(Path::new("/decks/other.pdf")),
            "a launch naming a different file must not be answered with the \
             document the last session happened to be looking at"
        );
    }

    /// A plan with nothing open is about no document, so it can never
    /// displace a named one.
    #[test]
    fn a_plan_with_no_document_is_about_nothing() {
        let plan = snapshot_about(None).plan(None);
        assert!(!plan.is_about(Path::new("/decks/talk.pdf")));
    }

    /// Two names for one file are one file. The snapshot's path came from
    /// another process and the named one from a file manager, so this is the
    /// ordinary case rather than a clever one.
    #[test]
    fn one_file_named_two_ways_is_still_a_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.pdf");
        std::fs::write(&path, b"deck").unwrap();
        let plan = snapshot_about(fingerprint(&path)).plan(fingerprint(&path).as_ref());

        let indirect = dir.path().join(".").join("talk.pdf");
        assert!(
            plan.is_about(&indirect),
            "{indirect:?} is the same file as {path:?} and must be recognised \
             as one, or a crash recovery turns into a plain reopen"
        );
    }

    /// A document that changed since the snapshot is still *that* document.
    /// `document()` refuses to restore a slide index into it, which is a
    /// separate question from which file the plan is about.
    #[test]
    fn a_rebuilt_document_is_still_the_one_the_plan_is_about() {
        let plan = snapshot_about(Some(unchanged_fingerprint())).plan(Some(&DocumentFingerprint {
            path: "/decks/talk.pdf".into(),
            modified_unix: Some(1_700_000_999),
            size: Some(5000),
        }));
        assert_eq!(plan.document_status, DocumentStatus::Changed);
        assert_eq!(plan.document(), None, "a moved-on deck restores no slide");
        assert!(
            plan.is_about(Path::new("/decks/talk.pdf")),
            "which file it is about does not depend on whether the file moved on"
        );
    }

    #[test]
    fn a_fingerprint_of_a_real_file_notices_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.pdf");
        std::fs::write(&path, b"one").unwrap();
        let before = fingerprint(&path).expect("the file is there");

        std::fs::write(&path, b"a longer deck").unwrap();
        let after = fingerprint(&path).expect("still there");
        assert!(!before.matches(&after));
        assert!(before.matches(&before.clone()));

        std::fs::remove_file(&path).unwrap();
        assert_eq!(fingerprint(&path), None);
    }
}
