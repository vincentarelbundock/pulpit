//! Failure-safe document reload.
//!
//! A file-watch event is a *hint*, never proof that a complete new PDF
//! exists. The manager debounces, waits for the file to stop changing,
//! attempts to open a candidate, and promotes it only once the candidate's
//! first audience frame has rendered. Until then the working document and the
//! last valid audience frame stay exactly where they are.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pulpit_core::{DocumentId, DocumentInfo};

/// What the filesystem says about the watched source right now.
///
/// An enum since `SPEC-images.md` §44.3, because the file case is blind to
/// the one that matters for a directory: a directory's mtime moves when a
/// member is added or removed, but **not** when a member's contents are
/// overwritten, so an export re-writing `slide03.png` in place would never be
/// noticed. The manager only ever compares stamps for equality, so the enum
/// drops into the existing logic unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStamp {
    File {
        len: u64,
        modified: Option<std::time::SystemTime>,
    },
    Directory {
        entries: usize,
        /// The digest of §42.3, over the ordered `(name, len, mtime)`
        /// triples — the same one the worker computes and the application
        /// compares against.
        digest: u64,
    },
}

/// Abstracted so tests can simulate a partial write without racing a real
/// filesystem.
pub trait FileProbe {
    fn probe(&self, path: &Path) -> Option<SourceStamp>;
}

/// The real filesystem.
pub struct RealFileProbe;

impl FileProbe for RealFileProbe {
    fn probe(&self, path: &Path) -> Option<SourceStamp> {
        let metadata = std::fs::metadata(path).ok()?;
        if metadata.is_dir() {
            // A directory still settling — a camera import landing fifty
            // files — is an unstable source on the same debounce and backoff
            // as a half-written PDF (§44.4), and that falls straight out of
            // the digest still moving between two probes.
            let (entries, digest) = pulpit_render::images::directory_stamp(path)?;
            return Some(SourceStamp::Directory { entries, digest });
        }
        Some(SourceStamp::File {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReloadPolicy {
    /// Wait this long after the last change before doing anything.
    pub debounce: Duration,
    /// The file must present the same stamp twice, this far apart, before it
    /// is considered stable enough to open.
    pub stability_interval: Duration,
    /// First retry delay; doubles up to `max_backoff`.
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// Report a persistent failure to the user after this many attempts.
    pub attempts_before_reporting: u32,
}

impl Default for ReloadPolicy {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(300),
            stability_interval: Duration::from_millis(150),
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
            attempts_before_reporting: 3,
        }
    }
}

/// What the manager wants the application to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Open this path as a *candidate*. The active document keeps rendering.
    OpenCandidate {
        path: PathBuf,
        document: DocumentId,
        attempt: u32,
    },
    /// The candidate opened and validated: render its current audience page
    /// before anything is promoted.
    RenderFirstFrame {
        document: DocumentId,
        info: DocumentInfo,
    },
    /// The first frame is ready: this is now the active document.
    Promote { info: DocumentInfo },
    /// Give up on this candidate and let go of its resources.
    DiscardCandidate { document: DocumentId },
    /// Tell the user the rebuild is not opening. Persistent, not transient.
    ReportFailure { reason: String, attempts: u32 },
    /// A previously reported failure has cleared.
    ClearFailure,
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Idle,
    /// Changes are arriving; wait for quiet.
    Debouncing {
        quiet_at: Instant,
        last_stamp: Option<SourceStamp>,
    },
    /// Quiet, but the stamp must repeat before we trust it.
    Stabilising {
        check_at: Instant,
        stamp: Option<SourceStamp>,
    },
    /// An open is in flight.
    Opening {
        document: DocumentId,
        attempt: u32,
    },
    /// Opened and validated; waiting for its first audience frame.
    AwaitingFirstFrame {
        document: DocumentId,
    },
    /// Failed; retry after a delay.
    Backoff {
        until: Instant,
        attempt: u32,
    },
}

#[derive(Debug)]
pub struct DocumentManager {
    path: PathBuf,
    policy: ReloadPolicy,
    state: State,
    active: Option<DocumentInfo>,
    next_document: DocumentId,
    reported_failure: bool,
    reloads: u64,
}

impl DocumentManager {
    pub fn new(path: impl Into<PathBuf>, policy: ReloadPolicy) -> Self {
        Self {
            path: path.into(),
            policy,
            state: State::Idle,
            active: None,
            next_document: DocumentId::NONE,
            reported_failure: false,
            reloads: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Continue the previous manager's document numbering.
    ///
    /// Document ids address open documents inside the render workers, and a
    /// worker keeps the ones it has been given. Opening a second file makes a
    /// second manager, so without this its first candidate would reuse the id
    /// the previous file still holds and every render would answer from the
    /// old document.
    pub fn continue_ids_after(&mut self, previous: &Self) {
        self.next_document = previous.next_document;
    }

    pub fn active(&self) -> Option<&DocumentInfo> {
        self.active.as_ref()
    }

    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn reload_count(&self) -> u64 {
        self.reloads
    }

    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub fn is_reloading(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    fn allocate(&mut self) -> DocumentId {
        self.next_document = self.next_document.next();
        self.next_document
    }

    /// Open the document for the first time. This is the same machinery as a
    /// reload, so the first open and every rebuild follow one code path.
    pub fn open_initial(&mut self, now: Instant) -> Vec<Action> {
        self.state = State::Stabilising {
            check_at: now,
            stamp: None,
        };
        self.tick(now, &RealFileProbe)
    }

    /// A file-watch hint arrived. Hints are cheap and may be duplicated.
    pub fn on_file_event(&mut self, now: Instant) -> Vec<Action> {
        let quiet_at = now + self.policy.debounce;
        match &self.state {
            // A change during an open or a wait means the build is still
            // running: restart the debounce and let the in-flight candidate
            // finish or be discarded when it lands.
            State::Debouncing { last_stamp, .. } => {
                self.state = State::Debouncing {
                    quiet_at,
                    last_stamp: *last_stamp,
                };
            }
            _ => {
                self.state = State::Debouncing {
                    quiet_at,
                    last_stamp: None,
                }
            }
        }
        Vec::new()
    }

    /// Drive timers. Call regularly (a subscription tick is enough).
    pub fn tick(&mut self, now: Instant, probe: &dyn FileProbe) -> Vec<Action> {
        match self.state.clone() {
            State::Idle | State::Opening { .. } | State::AwaitingFirstFrame { .. } => Vec::new(),
            State::Debouncing { quiet_at, .. } if now < quiet_at => Vec::new(),
            State::Debouncing { .. } => {
                let stamp = probe.probe(&self.path);
                self.state = State::Stabilising {
                    check_at: now + self.policy.stability_interval,
                    stamp,
                };
                Vec::new()
            }
            State::Stabilising { check_at, .. } if now < check_at => Vec::new(),
            State::Stabilising { stamp, .. } => {
                let current = probe.probe(&self.path);
                match current {
                    // The file vanished or is unreadable mid-build: retry.
                    None => {
                        self.state = State::Backoff {
                            until: now + self.policy.initial_backoff,
                            attempt: 1,
                        };
                        Vec::new()
                    }
                    Some(current) if Some(current) != stamp => {
                        // Still changing: a partial write. Look again later
                        // rather than opening a half-written file.
                        self.state = State::Stabilising {
                            check_at: now + self.policy.stability_interval,
                            stamp: Some(current),
                        };
                        Vec::new()
                    }
                    Some(_) => {
                        let document = self.allocate();
                        self.state = State::Opening {
                            document,
                            attempt: 1,
                        };
                        vec![Action::OpenCandidate {
                            path: self.path.clone(),
                            document,
                            attempt: 1,
                        }]
                    }
                }
            }
            State::Backoff { until, .. } if now < until => Vec::new(),
            State::Backoff { attempt, .. } => {
                let document = self.allocate();
                self.state = State::Opening {
                    document,
                    attempt: attempt + 1,
                };
                vec![Action::OpenCandidate {
                    path: self.path.clone(),
                    document,
                    attempt: attempt + 1,
                }]
            }
        }
    }

    /// The renderer opened the candidate and reported its page count.
    pub fn on_candidate_opened(&mut self, info: DocumentInfo) -> Vec<Action> {
        let State::Opening { document, .. } = self.state else {
            // A late reply for a candidate we already gave up on.
            return vec![Action::DiscardCandidate { document: info.id }];
        };
        if info.id != document {
            return vec![Action::DiscardCandidate { document: info.id }];
        }
        if info.pdf_pages == 0 {
            return self.on_candidate_failed(
                info.id,
                "the document reports no pages".into(),
                Instant::now(),
            );
        }
        self.state = State::AwaitingFirstFrame { document };
        vec![Action::RenderFirstFrame { document, info }]
    }

    /// The candidate's first audience frame rendered: promote it. This is the
    /// only path that replaces the working document.
    pub fn on_first_frame(&mut self, info: DocumentInfo) -> Vec<Action> {
        let State::AwaitingFirstFrame { document } = self.state else {
            return vec![Action::DiscardCandidate { document: info.id }];
        };
        if info.id != document {
            return vec![Action::DiscardCandidate { document: info.id }];
        }
        let previous = self.active.replace(info.clone());
        self.state = State::Idle;
        self.reloads += 1;

        let mut actions = vec![Action::Promote { info }];
        if let Some(previous) = previous {
            // The old document is only released once nothing needs it.
            actions.push(Action::DiscardCandidate {
                document: previous.id,
            });
        }
        if self.reported_failure {
            self.reported_failure = false;
            actions.push(Action::ClearFailure);
        }
        actions
    }

    /// The candidate failed to open, or its first frame failed to render.
    pub fn on_candidate_failed(
        &mut self,
        document: DocumentId,
        reason: String,
        now: Instant,
    ) -> Vec<Action> {
        let attempt = match self.state {
            State::Opening { attempt, .. } | State::Backoff { attempt, .. } => attempt,
            State::AwaitingFirstFrame { .. } => 1,
            _ => return vec![Action::DiscardCandidate { document }],
        };
        let backoff = self
            .policy
            .initial_backoff
            .saturating_mul(2u32.saturating_pow(attempt.min(10)))
            .min(self.policy.max_backoff);
        self.state = State::Backoff {
            until: now + backoff,
            attempt,
        };

        let mut actions = vec![Action::DiscardCandidate { document }];
        if attempt >= self.policy.attempts_before_reporting && !self.reported_failure {
            self.reported_failure = true;
            actions.push(Action::ReportFailure {
                reason,
                attempts: attempt,
            });
        }
        actions
    }

    /// Adopt a document that was opened outside the reload path (the very
    /// first load, or an explicit "open file" action).
    #[allow(dead_code)] // reached by its tests, not by the application — SPEC-simplify.md §69
    pub fn adopt(&mut self, info: DocumentInfo) {
        self.path = info.path.clone();
        self.active = Some(info);
        self.state = State::Idle;
    }

    #[allow(dead_code)] // unreached, including by its own tests — SPEC-simplify.md §69
    pub fn next_document_id(&mut self) -> DocumentId {
        self.allocate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeProbe {
        stamps: RefCell<Vec<Option<SourceStamp>>>,
    }

    impl FakeProbe {
        fn with(stamps: Vec<Option<SourceStamp>>) -> Self {
            Self {
                stamps: RefCell::new(stamps),
            }
        }
    }

    impl FileProbe for FakeProbe {
        fn probe(&self, _path: &Path) -> Option<SourceStamp> {
            let mut stamps = self.stamps.borrow_mut();
            if stamps.len() > 1 {
                stamps.remove(0)
            } else {
                stamps.first().copied().flatten()
            }
        }
    }

    fn stamp(len: u64) -> Option<SourceStamp> {
        Some(SourceStamp::File {
            len,
            modified: None,
        })
    }

    fn folder(entries: usize, digest: u64) -> Option<SourceStamp> {
        Some(SourceStamp::Directory { entries, digest })
    }

    /// Report a file event, let the debounce window pass, and take the id of
    /// the candidate that opens.
    ///
    /// The first tick is inside the debounce window and must produce nothing;
    /// the second is past it. Every reload test starts here, and the interval
    /// arithmetic said five times over hid what each was actually about.
    fn open_candidate(
        manager: &mut DocumentManager,
        probe: &FakeProbe,
        start: Instant,
    ) -> DocumentId {
        manager.on_file_event(start);
        manager.tick(start + Duration::from_millis(400), probe);
        let open = manager.tick(start + Duration::from_millis(600), probe);
        let Action::OpenCandidate { document, .. } = open[0].clone() else {
            panic!("the debounce window has passed, so a candidate opens")
        };
        document
    }

    fn manager() -> DocumentManager {
        DocumentManager::new("/decks/talk.pdf", ReloadPolicy::default())
    }

    fn info(id: DocumentId, pages: usize) -> DocumentInfo {
        DocumentInfo::new(id, "/decks/talk.pdf", pages)
    }

    #[test]
    fn a_burst_of_events_produces_exactly_one_open() {
        let start = Instant::now();
        let mut manager = manager();
        let probe = FakeProbe::with(vec![stamp(1000)]);

        // typst watch rewrites the file a dozen times in 50ms.
        for i in 0..12 {
            manager.on_file_event(start + Duration::from_millis(i * 4));
            assert!(manager
                .tick(start + Duration::from_millis(i * 4), &probe)
                .is_empty());
        }
        // Nothing yet: still inside the debounce window.
        assert!(manager
            .tick(start + Duration::from_millis(100), &probe)
            .is_empty());

        let quiet = start + Duration::from_millis(500);
        assert!(
            manager.tick(quiet, &probe).is_empty(),
            "first probe only records the stamp"
        );
        let actions = manager.tick(quiet + Duration::from_millis(200), &probe);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            actions[0],
            Action::OpenCandidate { attempt: 1, .. }
        ));
    }

    #[test]
    fn a_partial_write_is_never_opened() {
        let start = Instant::now();
        let mut manager = manager();
        // The file grows between probes: a build in progress.
        let probe = FakeProbe::with(vec![stamp(100), stamp(400), stamp(900), stamp(900)]);

        manager.on_file_event(start);
        let mut now = start + Duration::from_millis(400);
        let mut opens = 0;
        for _ in 0..4 {
            for action in manager.tick(now, &probe) {
                if matches!(action, Action::OpenCandidate { .. }) {
                    opens += 1;
                }
            }
            now += Duration::from_millis(200);
        }
        assert_eq!(opens, 1, "opened once, only after the size settled");
    }

    /// §44.4. A folder receiving fifty files from a camera import is the
    /// normal case, not an error: it is an unstable source on the same
    /// debounce and backoff as a half-written PDF, and the digest still
    /// moving is what says so.
    #[test]
    fn a_directory_still_receiving_files_is_never_opened() {
        let start = Instant::now();
        let mut manager = DocumentManager::new("/pictures/import", ReloadPolicy::default());
        let probe = FakeProbe::with(vec![
            folder(3, 0xaaa),
            folder(20, 0xbbb),
            folder(50, 0xccc),
            folder(50, 0xccc),
        ]);

        manager.on_file_event(start);
        let mut now = start + Duration::from_millis(400);
        let mut opens = 0;
        for _ in 0..4 {
            opens += manager
                .tick(now, &probe)
                .iter()
                .filter(|action| matches!(action, Action::OpenCandidate { .. }))
                .count();
            now += Duration::from_millis(200);
        }
        assert_eq!(opens, 1, "opened once, only after the digest settled");
    }

    /// The §44.2 regression, at the manager's own level: the *count* did not
    /// change and neither did the directory's mtime, so only a stamp that
    /// carries the digest can tell these two apart.
    #[test]
    fn a_member_overwritten_in_place_is_a_different_stamp() {
        assert_ne!(folder(12, 0xaaa), folder(12, 0xbbb));
        assert_eq!(folder(12, 0xaaa), folder(12, 0xaaa));
    }

    #[test]
    fn a_failed_build_retries_with_backoff_and_reports_persistence() {
        let start = Instant::now();
        let mut manager = manager();
        let probe = FakeProbe::with(vec![stamp(500)]);
        let document = open_candidate(&mut manager, &probe, start);

        // First failure: retried quietly.
        let actions = manager.on_candidate_failed(document, "broken".into(), start);
        assert!(actions
            .iter()
            .all(|a| matches!(a, Action::DiscardCandidate { .. })));

        // Keep failing until it is worth telling the user.
        let mut now = start;
        let mut reported = None;
        for _ in 0..6 {
            now += Duration::from_secs(10);
            let actions = manager.tick(now, &probe);
            let Some(Action::OpenCandidate { document, .. }) = actions.first().cloned() else {
                continue;
            };
            for action in manager.on_candidate_failed(document, "still broken".into(), now) {
                if let Action::ReportFailure { attempts, .. } = action {
                    reported = Some(attempts);
                }
            }
        }
        let attempts = reported.expect("a persistent failure must be reported");
        assert!(attempts >= ReloadPolicy::default().attempts_before_reporting);
    }

    #[test]
    fn the_working_document_survives_a_failed_rebuild() {
        let start = Instant::now();
        let mut manager = manager();
        manager.adopt(info(DocumentId(1), 20));

        let probe = FakeProbe::with(vec![stamp(500)]);
        let document = open_candidate(&mut manager, &probe, start);
        manager.on_candidate_failed(document, "typst error".into(), start);

        assert_eq!(
            manager.active().unwrap().id,
            DocumentId(1),
            "still the working document"
        );
        assert_eq!(manager.active().unwrap().pdf_pages, 20);
    }

    #[test]
    fn a_candidate_is_promoted_only_after_its_first_frame() {
        let start = Instant::now();
        let mut manager = manager();
        manager.adopt(info(DocumentId(1), 20));

        let probe = FakeProbe::with(vec![stamp(500)]);
        let document = open_candidate(&mut manager, &probe, start);

        let candidate = info(document, 25);
        let actions = manager.on_candidate_opened(candidate.clone());
        assert!(matches!(actions[0], Action::RenderFirstFrame { .. }));
        assert_eq!(
            manager.active().unwrap().id,
            DocumentId(1),
            "opening is not promoting: the projector still shows the old frame"
        );

        let actions = manager.on_first_frame(candidate.clone());
        assert!(actions.contains(&Action::Promote {
            info: candidate.clone()
        }));
        assert!(actions.contains(&Action::DiscardCandidate {
            document: DocumentId(1)
        }));
        assert_eq!(manager.active().unwrap().pdf_pages, 25);
        assert_eq!(manager.reload_count(), 1);
    }

    #[test]
    fn an_empty_candidate_is_rejected() {
        let start = Instant::now();
        let mut manager = manager();
        manager.adopt(info(DocumentId(1), 20));
        let probe = FakeProbe::with(vec![stamp(500)]);
        let document = open_candidate(&mut manager, &probe, start);

        let actions = manager.on_candidate_opened(info(document, 0));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::DiscardCandidate { .. })));
        assert_eq!(manager.active().unwrap().pdf_pages, 20);
    }

    #[test]
    fn a_late_reply_for_an_abandoned_candidate_is_discarded() {
        let mut manager = manager();
        manager.adopt(info(DocumentId(1), 20));
        let actions = manager.on_candidate_opened(info(DocumentId(99), 5));
        assert_eq!(
            actions,
            vec![Action::DiscardCandidate {
                document: DocumentId(99)
            }]
        );
        assert_eq!(manager.active().unwrap().id, DocumentId(1));

        let actions = manager.on_first_frame(info(DocumentId(99), 5));
        assert_eq!(
            actions,
            vec![Action::DiscardCandidate {
                document: DocumentId(99)
            }]
        );
    }

    #[test]
    fn recovery_after_a_persistent_failure_clears_the_warning() {
        let start = Instant::now();
        let mut manager = manager();
        manager.adopt(info(DocumentId(1), 20));
        let probe = FakeProbe::with(vec![stamp(500)]);

        // Fail enough times to be reported.
        manager.on_file_event(start);
        manager.tick(start + Duration::from_millis(400), &probe);
        let mut now = start + Duration::from_millis(600);
        let mut reported = false;
        for _ in 0..8 {
            for action in manager.tick(now, &probe) {
                if let Action::OpenCandidate { document, .. } = action {
                    for action in manager.on_candidate_failed(document, "boom".into(), now) {
                        reported |= matches!(action, Action::ReportFailure { .. });
                    }
                }
            }
            now += Duration::from_secs(10);
        }
        assert!(reported);

        // Then a good build lands.
        now += Duration::from_secs(30);
        let actions = manager.tick(now, &probe);
        let Some(Action::OpenCandidate { document, .. }) = actions.first().cloned() else {
            panic!("expected another attempt")
        };
        let candidate = info(document, 22);
        manager.on_candidate_opened(candidate.clone());
        let actions = manager.on_first_frame(candidate);
        assert!(
            actions.contains(&Action::ClearFailure),
            "the user is told it recovered"
        );
    }

    #[test]
    fn rapid_rebuilds_during_a_reload_do_not_pile_up() {
        let start = Instant::now();
        let mut manager = manager();
        manager.adopt(info(DocumentId(1), 20));
        let probe = FakeProbe::with(vec![stamp(500)]);

        let document = open_candidate(&mut manager, &probe, start);

        // Another rebuild lands while the candidate is still opening.
        let mut opens = 0;
        for i in 0..10 {
            manager.on_file_event(start + Duration::from_millis(700 + i));
            opens += manager
                .tick(start + Duration::from_millis(700 + i), &probe)
                .iter()
                .filter(|a| matches!(a, Action::OpenCandidate { .. }))
                .count();
        }
        assert_eq!(
            opens, 0,
            "no second open while one is in flight or debouncing"
        );

        // The newer build wins: the in-flight candidate is abandoned rather
        // than promoted over a file that has since changed again.
        let stale = info(document, 21);
        assert_eq!(
            manager.on_candidate_opened(stale.clone()),
            vec![Action::DiscardCandidate { document }]
        );
        assert_eq!(
            manager.active().unwrap().pdf_pages,
            20,
            "still the working document"
        );

        // And the newer build is opened and promoted normally.
        let mut now = start + Duration::from_secs(1);
        let mut opened = None;
        for _ in 0..5 {
            now += Duration::from_millis(300);
            if let Some(Action::OpenCandidate { document, .. }) =
                manager.tick(now, &probe).first().cloned()
            {
                opened = Some(document);
                break;
            }
        }
        let document = opened.expect("expected the newer build to be opened");
        let candidate = info(document, 21);
        manager.on_candidate_opened(candidate.clone());
        assert!(manager
            .on_first_frame(candidate)
            .iter()
            .any(|a| matches!(a, Action::Promote { .. })));
    }
}
