//! Supervisor integration tests with real worker processes.
//!
//! These run the fixture backend so they need neither PDFium nor a graphical
//! session, which is what makes them CI-safe.

use std::time::{Duration, Instant};

use pulpit_core::notes::Region;
use pulpit_core::RenderGeneration;
use pulpit_render::protocol::{Priority, RenderJob, RequestId};
use pulpit_render::supervisor::{
    RenderEvent, RendererSupervisor, SupervisorConfig, Wakeup, WorkerCommand,
};

const WORKER: &str = env!("CARGO_BIN_EXE_pulpit-render-worker");

fn config(workers: usize) -> SupervisorConfig {
    SupervisorConfig {
        workers,
        command: WorkerCommand::Explicit {
            program: WORKER.into(),
            args: Vec::new(),
        },
        deadline: Duration::from_secs(5),
        max_restarts: 3,
        restart_window: Duration::from_secs(60),
        // Far longer than any test runs, so the pool never shrinks under a
        // test that is not about retirement.
        retire_after: Duration::from_secs(120),
    }
}

/// Serialises worker spawns.
///
/// Failure injection is configured through the environment and inherited by
/// the child, but the environment is process-global while `cargo test` runs
/// these in parallel threads. A test that set `PULPIT_WORKER_HANG_ON_PAGE=5`
/// around its own spawn therefore also armed the hang in *any* worker another
/// test happened to start in that window — and
/// `inline_bytes_in_flight_are_capped` renders pages 0..=5, so its sixth job
/// is page 5. It lost that race often enough to be the suite's flakiest test:
/// five frames, then a worker sleeping for ever, recovered only by the
/// deadline killing it. It looked exactly like a supervisor stall.
///
/// Holding this across set / spawn / unset closes the window, because every
/// spawn takes it. A worker spawned *later* by `dispatch` to relieve
/// contention is not covered, but by then the variables are long unset.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Start a supervisor with `variables` set only for the workers it spawns.
fn start_with(config: SupervisorConfig, variables: &[(&str, &str)]) -> RendererSupervisor {
    // A panicking test poisons the lock; the environment is still consistent
    // because the unset below runs before this function returns.
    let guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    // Children inherit this: the fixture backend keeps the tests hermetic.
    std::env::set_var("PULPIT_FORCE_FIXTURE_BACKEND", "1");
    for (name, value) in variables {
        std::env::set_var(name, value);
    }
    let supervisor = RendererSupervisor::start(config).expect("start supervisor");
    for (name, _) in variables {
        std::env::remove_var(name);
    }
    drop(guard);
    supervisor
}

fn start(workers: usize) -> RendererSupervisor {
    start_with(config(workers), &[])
}

fn job(id: u64, generation: u64, page: usize, priority: Priority) -> RenderJob {
    RenderJob {
        id: RequestId(id),
        generation: RenderGeneration(generation),
        document: 1,
        page,
        region: Region::FULL,
        width: 320,
        height: 180,
        priority,
        with_annotations: false,
        // Replaced by the supervisor with the worker's own region.
        region_name: "placeholder".into(),
    }
}

/// Pump until `predicate` is satisfied or the timeout expires.
fn collect_until(
    supervisor: &mut RendererSupervisor,
    timeout: Duration,
    mut predicate: impl FnMut(&[RenderEvent]) -> bool,
) -> Vec<RenderEvent> {
    let deadline = Instant::now() + timeout;
    let mut all = Vec::new();
    while Instant::now() < deadline {
        all.extend(supervisor.pump_blocking(Duration::from_millis(50)));
        if predicate(&all) {
            break;
        }
    }
    all
}

fn frames(events: &[RenderEvent]) -> Vec<&RenderJob> {
    events
        .iter()
        .filter_map(|event| match event {
            RenderEvent::Frame { job, .. } => Some(job),
            _ => None,
        })
        .collect()
}

fn completed_job_ids(events: &[RenderEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            RenderEvent::Frame { job, .. } | RenderEvent::Failed { job, .. } => Some(job.id.0),
            RenderEvent::Cancelled { id } => Some(id.0),
            _ => None,
        })
        .collect()
}

#[test]
fn renders_pages_through_worker_processes() {
    let mut supervisor = start(2);
    supervisor.open(1, "fixture:pages=30");

    for (index, page) in [0usize, 1, 2].iter().enumerate() {
        supervisor.submit(job(index as u64 + 1, 1, *page, Priority::Audience));
    }
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).len() == 3
    });

    let rendered = frames(&events);
    assert_eq!(rendered.len(), 3, "all three pages came back: {events:?}");
    for event in &events {
        if let RenderEvent::Frame { job, frame, .. } = event {
            assert_eq!(frame.width, job.width);
            assert_eq!(frame.pixels.len(), 320 * 180 * 4);
            assert!(
                frame.pixels.iter().any(|b| *b != 0),
                "the frame has content"
            );
        }
    }
    assert!(events
        .iter()
        .any(|e| matches!(e, RenderEvent::Opened(o) if o.page_count == 30)));
}

#[test]
fn a_crashing_worker_is_restarted_and_rendering_resumes() {
    // One worker, so the crash is unambiguous.
    let mut supervisor = start_with(config(1), &[("PULPIT_WORKER_CRASH_ON_PAGE", "7")]);
    supervisor.open(1, "fixture:pages=30");

    supervisor.submit(job(1, 1, 7, Priority::Audience));
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerRestarted { .. }))
    });

    assert!(
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerCrashed { .. })),
        "the crash is reported: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::Failed { job, .. } if job.id == RequestId(1))),
        "the active request fails rather than hanging: {events:?}"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, RenderEvent::WorkerRestarted { .. })));
    assert_eq!(supervisor.worker_count(), 1, "the pool is intact");

    // The replacement worker reopened the document and renders again.
    supervisor.submit(job(2, 1, 3, Priority::Audience));
    let more = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        !frames(events).is_empty()
    });
    assert_eq!(frames(&more).len(), 1, "rendering resumed: {more:?}");

    // §76.7: the replacement worker replays `Open` for every document it
    // missed, and its `Opened` answer used to be forwarded again as if it
    // were a second document arriving — which the application read as a
    // fresh candidate to compare against the one it already had, closing the
    // very document that was still live. Across the whole crash and restart,
    // exactly one `Opened` for document 1 must reach the application.
    let all: Vec<RenderEvent> = events.into_iter().chain(more).collect();
    let opened_count = all
        .iter()
        .filter(|e| matches!(e, RenderEvent::Opened(o) if o.document == 1))
        .count();
    assert_eq!(
        opened_count, 1,
        "exactly one Opened for document 1 across the crash: {all:?}"
    );
}

/// §77.8: `ask()` questions used to be fire-and-forget. A crash or a stall
/// lost Navigation/Capabilities/Links with no event, and the application had
/// no way to notice it was still waiting — it never re-asked.
///
/// The worker's own loop is single-threaded (see `worker.rs::run`): once it
/// picks up a render it does not return to its inbox until that render
/// finishes, so a render that hangs forever strands any control message
/// queued behind it exactly as a crash would, without racing a real crash's
/// timing.
#[test]
fn an_ask_lost_to_a_hung_worker_is_replayed_after_the_restart() {
    let mut supervisor = start_with(
        SupervisorConfig {
            deadline: Duration::from_millis(300),
            ..config(1)
        },
        &[("PULPIT_WORKER_HANG_ON_PAGE", "5")],
    );
    supervisor.open(1, "fixture:pages=30");

    // Occupy the worker's one thread with a render that never returns.
    supervisor.submit(job(1, 1, 5, Priority::Audience));
    // Give the worker time to actually pick up the render and start hanging
    // before the question arrives, so it queues behind rather than racing it.
    std::thread::sleep(Duration::from_millis(200));
    supervisor.request_navigation(1);

    // The deadline kills the hung worker and restarts it; the replacement
    // must still answer the question the dead one never reached.
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::Navigation { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerTimedOut { .. })),
        "the hang is what kills the worker: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::Navigation { document: 1, .. })),
        "the lost question was replayed on the replacement, not lost: {events:?}"
    );
}

/// §77.8: give-up used to be instant and permanent — once the restart budget
/// inside `restart_window` was spent, the slot never rendered again for the
/// rest of the session, even for a document with nothing wrong with it.
/// `max_restarts: 0` makes the very first crash exceed the budget, so
/// give-up is reached from one crash rather than racing how many times the
/// injected crash re-arms itself across a restart (it does not: the failure
/// injection is only inherited by the worker `start_with` spawns).
#[test]
fn a_given_up_worker_is_retried_once_its_backoff_elapses() {
    let mut supervisor = start_with(
        SupervisorConfig {
            max_restarts: 0,
            restart_window: Duration::from_millis(200),
            ..config(1)
        },
        &[("PULPIT_WORKER_CRASH_ON_PAGE", "7")],
    );
    supervisor.open(1, "fixture:pages=30");
    supervisor.submit(job(1, 1, 7, Priority::Audience));

    let gave_up = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerGaveUp { .. }))
    });
    assert!(
        !gave_up
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerRestarted { .. })),
        "give-up is immediate, not preceded by an extra restart: {gave_up:?}"
    );

    // Past the backoff window the slot is retried on its own — no further
    // submission is needed to provoke it, unlike the contention that grows
    // the pool.
    let retried = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerRestarted { .. }))
    });
    assert!(
        retried
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerRestarted { .. })),
        "the given-up worker is retried once its backoff elapses: {retried:?}"
    );
    assert_eq!(supervisor.worker_count(), 1, "the pool recovered");

    // And it renders again: a good deck is not stuck behind one bad page for
    // the rest of the session.
    supervisor.submit(job(2, 1, 3, Priority::Audience));
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        !frames(events).is_empty()
    });
    assert_eq!(frames(&events).len(), 1, "rendering resumed: {events:?}");
}

#[test]
fn an_unresponsive_worker_is_terminated_at_the_deadline() {
    let mut supervisor = start_with(
        SupervisorConfig {
            deadline: Duration::from_millis(300),
            ..config(1)
        },
        &[("PULPIT_WORKER_HANG_ON_PAGE", "5")],
    );
    supervisor.open(1, "fixture:pages=30");

    supervisor.submit(job(1, 1, 5, Priority::Audience));
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerTimedOut { .. }))
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerTimedOut { .. })),
        "termination is the last-resort cancellation mechanism: {events:?}"
    );
}

#[test]
fn obsolete_generations_never_produce_frames() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    supervisor.submit(job(1, 1, 10, Priority::Adjacent));
    supervisor.submit(job(2, 1, 11, Priority::Adjacent));
    // A reload happens: everything before generation 2 is obsolete.
    supervisor.cancel_older_than(RenderGeneration(2));
    supervisor.submit(job(3, 2, 12, Priority::Audience));

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).iter().any(|job| job.id == RequestId(3))
    });

    for job in frames(&events) {
        assert!(
            job.generation >= RenderGeneration(2),
            "a stale frame reached the application: {job:?}"
        );
    }
    assert!(frames(&events).iter().any(|job| job.id == RequestId(3)));
}

#[test]
fn a_duplicate_request_for_the_same_page_still_answers_both_ids() {
    // The queue used to deduplicate identical jobs, answering only the newer
    // id — which left the older requester's frame key pending for ever. Two
    // identical inline requests now both come back. (The region-job variant,
    // where the duplicates genuinely sit in the queue together, is
    // `two_identical_jobs_from_different_requesters_are_both_answered`.)
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    supervisor.submit(job(1, 1, 0, Priority::Audience));
    supervisor.submit(job(2, 1, 4, Priority::Adjacent));
    supervisor.submit(job(3, 1, 4, Priority::Audience));

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).len() >= 3
    });
    let ids: Vec<u64> = frames(&events).iter().map(|job| job.id.0).collect();
    for id in [1, 2, 3] {
        assert!(ids.contains(&id), "job {id} was answered: {ids:?}");
    }
}

#[test]
fn shutdown_leaves_no_workers_behind() {
    let mut supervisor = start(2);
    supervisor.open(1, "fixture:pages=5");
    supervisor.submit(job(1, 1, 0, Priority::Audience));
    let _ = collect_until(&mut supervisor, Duration::from_secs(5), |events| {
        !frames(events).is_empty()
    });
    supervisor.shutdown();
    assert_eq!(supervisor.worker_count(), 0);
}

/// A job too large to travel inline, so it must own the shared region.
fn region_job(id: u64, page: usize) -> RenderJob {
    let mut job = job(id, 1, page, Priority::Audience);
    // 1600 × 1400 × 4 = 8.96 MB, just over `INLINE_FRAME_BYTES`.
    job.width = 1600;
    job.height = 1400;
    job
}

#[test]
fn inline_jobs_pipeline_within_one_worker() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    for page in 0..4usize {
        supervisor.submit(job(page as u64 + 1, 1, page, Priority::Ancillary));
    }
    // All four were dispatched immediately: an inline frame does not occupy
    // the region, so the worker holds a small backlog instead of receiving
    // one job per pump.
    assert_eq!(
        supervisor.in_flight(),
        4,
        "the pipeline is filled at submit"
    );
    assert_eq!(supervisor.queued(), 0);

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).len() == 4
    });
    let rendered = frames(&events);
    assert_eq!(rendered.len(), 4, "every job came back: {events:?}");
    for event in &events {
        if let RenderEvent::Frame { frame, .. } = event {
            assert_eq!(frame.pixels.len(), 320 * 180 * 4);
            assert!(frame.pixels.iter().any(|b| *b != 0));
        }
    }
}

#[test]
fn region_jobs_stay_exclusive_per_worker() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    supervisor.submit(region_job(1, 0));
    supervisor.submit(region_job(2, 1));
    // The second must wait: the first owns the region until its frame is
    // copied out on a pump.
    assert_eq!(supervisor.in_flight(), 1, "one region job in flight");
    assert_eq!(supervisor.queued(), 1, "the other waits for the region");

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).len() == 2
    });
    assert_eq!(
        frames(&events).len(),
        2,
        "both rendered in turn: {events:?}"
    );
}

#[test]
fn a_crash_fails_every_in_flight_job() {
    let mut supervisor = start_with(config(1), &[("PULPIT_WORKER_CRASH_ON_PAGE", "7")]);
    supervisor.open(1, "fixture:pages=30");

    // Four inline jobs pipeline onto the one worker; the first crashes it.
    for (index, page) in [7usize, 8, 9, 10].iter().enumerate() {
        supervisor.submit(job(index as u64 + 1, 1, *page, Priority::Ancillary));
    }
    assert_eq!(supervisor.in_flight(), 4);

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::WorkerRestarted { .. }))
    });
    // Every dispatched job fails; a silently dropped one would leave its
    // requester waiting forever.
    let failed: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            RenderEvent::Failed { job, .. } => Some(job.id.0),
            _ => None,
        })
        .collect();
    for id in 1..=4u64 {
        assert!(
            failed.contains(&id),
            "job {id} was failed, not lost: {events:?}"
        );
    }
}

#[test]
fn inline_bytes_in_flight_are_capped() {
    let mut supervisor = start(1);

    // 1024 × 2048 × 4 is exactly `INLINE_FRAME_BYTES`: inline, but four of
    // them fill `MAX_INLINE_IN_FLIGHT_BYTES`, so the depth cap alone must
    // not decide how many the worker holds.
    for page in 0..6usize {
        let mut big = job(page as u64 + 1, 1, page, Priority::Ancillary);
        big.width = 1024;
        big.height = 2048;
        supervisor.submit(big);
    }
    assert_eq!(
        supervisor.in_flight(),
        4,
        "the byte budget, not the depth, is the binding cap"
    );
    assert_eq!(supervisor.queued(), 2);

    // The document is intentionally unopened, so each large job returns a
    // bounded failure instead of serialising an 8 MiB fixture frame in an
    // unoptimised test build. The replenishment path is the same: every reply
    // releases its byte budget and dispatches one of the two queued jobs.
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        completed_job_ids(events).len() == 6
    });
    let mut completed = completed_job_ids(&events);
    completed.sort_unstable();
    assert_eq!(
        completed,
        vec![1, 2, 3, 4, 5, 6],
        "every byte-budgeted job receives exactly one terminal answer"
    );
}

/// The doorbell rings when a frame is ready, so the application need not poll
/// to find out. This is the whole latency claim: without it a finished frame
/// waits for the next application tick before any window can draw it.
#[test]
fn a_finished_frame_rings_the_doorbell() {
    let mut supervisor = start(1);
    let wakeup = supervisor.take_wakeup().expect("the doorbell, once");
    supervisor.open(1, "fixture:pages=4");

    // Opening is itself something a worker answers, and that answer rings:
    // every worker message is one, not only a frame. Settle those first, so
    // what the frame does is what this test is measuring.
    while wakeup.wait(Duration::from_millis(200)) == Wakeup::Ring {
        supervisor.pump();
    }
    assert_eq!(
        wakeup.wait(Duration::from_millis(100)),
        Wakeup::Idle,
        "an idle renderer does not wake the event loop"
    );

    supervisor.submit(job(1, 1, 0, Priority::Audience));
    assert_eq!(
        wakeup.wait(Duration::from_secs(10)),
        Wakeup::Ring,
        "the frame announced itself"
    );
    // The ring is only a hint to look; the frame comes off the ordinary queue.
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        !frames(events).is_empty()
    });
    assert_eq!(frames(&events).len(), 1, "and it was really there");
}

/// The handle is taken once. A second listener would divide the rings between
/// two waiters, and the one that did not get it would sleep through a frame.
#[test]
fn the_doorbell_has_exactly_one_listener() {
    let mut supervisor = start(1);
    assert!(supervisor.take_wakeup().is_some());
    assert!(supervisor.take_wakeup().is_none());
}

/// A burst collapses: the doorbell says "look again", so several frames
/// finishing together are one pass of the event loop rather than one each.
#[test]
fn a_burst_of_frames_is_one_wakeup() {
    let mut supervisor = start(2);
    let wakeup = supervisor.take_wakeup().expect("the doorbell");
    supervisor.open(1, "fixture:pages=8");

    for page in 0..6usize {
        supervisor.submit(job(page as u64 + 1, 1, page, Priority::Ancillary));
    }
    let events = collect_until(&mut supervisor, Duration::from_secs(20), |events| {
        frames(events).len() == 6
    });
    assert_eq!(frames(&events).len(), 6, "all six rendered: {events:?}");

    // A reader thread posts its message and *then* rings, so collecting the
    // six frames does not mean the six rings have all been attempted yet. A
    // ring that lands between two waits is a *late* ring, not a queued one,
    // and counting it as queueing is how this test used to fail on a loaded
    // machine: it slept a fixed 500 ms and called that "every straggler has
    // landed", which is a guess about scheduling rather than a fact about the
    // doorbell.
    //
    // So wait for silence instead of guessing at it. The doorbell is one
    // deep, so quiet for a whole window means every ring has been attempted
    // and collapsed; the deadline is only there so a broken doorbell fails
    // rather than hangs.
    let quiet = Duration::from_millis(250);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut rings = 0;
    while wakeup.wait(quiet) == Wakeup::Ring {
        rings += 1;
        assert!(
            Instant::now() < deadline,
            "the doorbell never went quiet: {rings} rings and counting"
        );
    }

    // What coalescing means, stated so that it cannot be confused with
    // scheduling: six frames did not produce six wakeups. A queueing doorbell
    // holds one ring per frame and hands back all six here with no pause
    // between them, which is the failure this is looking for. Coalescing
    // gives one, or two when the last frame's ring lands just after the first
    // wait took the slot.
    assert!(
        rings < 6,
        "the doorbell queued a ring per frame rather than coalescing: {rings}"
    );
}

/// A shutdown closes the doorbell rather than leaving a listener parked on it
/// for ever: the listener thread must be able to notice and stop.
#[test]
fn dropping_the_supervisor_closes_the_doorbell() {
    let mut supervisor = start(1);
    let wakeup = supervisor.take_wakeup().expect("the doorbell");
    drop(supervisor);
    // A dying worker says so, and that rings on the way past. What matters is
    // that the rings run out and the doorbell then reports itself closed,
    // rather than leaving the listener parked until its timeout for ever.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut outcome = wakeup.wait(Duration::from_millis(200));
    while outcome == Wakeup::Ring && Instant::now() < deadline {
        outcome = wakeup.wait(Duration::from_millis(200));
    }
    assert_eq!(outcome, Wakeup::Closed);
    // And stays closed, so a loop that stops on this stops for good.
    assert_eq!(wakeup.wait(Duration::from_millis(50)), Wakeup::Closed);
}

/// A backend with no text layer says so, rather than answering "no matches".
///
/// The fixture backend cannot search, and that has to reach the application
/// as a different fact from an empty result: a presenter told "no matches"
/// stops looking, and a presenter told the deck cannot be searched does not.
#[test]
fn a_backend_that_cannot_search_says_so_over_the_protocol() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture.pdf");
    supervisor.request_find_text(
        1,
        pulpit_core::search::SearchGeneration(7),
        pulpit_core::search::Query::new("anything", false, false),
        0..4,
    );
    let events = collect_until(&mut supervisor, Duration::from_secs(5), |events| {
        events
            .iter()
            .any(|event| matches!(event, RenderEvent::Found { .. }))
    });
    let found = events
        .iter()
        .find_map(|event| match event {
            RenderEvent::Found {
                generation,
                chunk,
                searchable,
                ..
            } => Some((*generation, chunk.clone(), *searchable)),
            _ => None,
        })
        .expect("the worker answered the search");
    assert_eq!(found.0, pulpit_core::search::SearchGeneration(7));
    assert!(found.1.hits.is_empty());
    assert!(!found.2, "the fixture backend has no text layer to search");
    supervisor.shutdown();
}

/// `SPEC-images.md` §45, end to end through real worker processes: one worker
/// holds a folder of pictures and a deck at the same time, each answered by
/// its own backend, and the folder's open carries the source digest §42.3
/// compares against.
#[test]
fn a_folder_of_images_and_a_deck_are_both_held_by_one_worker() {
    let dir = tempfile::tempdir().expect("temporary directory");
    for (name, colour) in [("img2.png", 40u8), ("img10.png", 90)] {
        image::RgbaImage::from_pixel(64, 32, image::Rgba([colour, colour, colour, 255]))
            .save(dir.path().join(name))
            .expect("write a fixture image");
    }
    // Neither a page nor a reload trigger: decided by extension alone (§41.1).
    std::fs::write(dir.path().join("notes.txt"), b"not a page").unwrap();

    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=4");
    supervisor.open(2, &dir.path().to_string_lossy());

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .filter(|e| matches!(e, RenderEvent::Opened(_)))
            .count()
            == 2
    });
    let opened: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RenderEvent::Opened(opened) => Some(opened.clone()),
            _ => None,
        })
        .collect();

    let deck = opened.iter().find(|o| o.document == 1).expect("the deck");
    assert_eq!(deck.page_count, 4);
    assert!(deck.source_digest.is_none(), "a file has no listing digest");

    let folder = opened.iter().find(|o| o.document == 2).expect("the folder");
    assert_eq!(folder.page_count, 2, "the text file is not a page");
    assert_eq!(folder.first_page_size.width, 64.0);
    assert!(folder.metadata_text.is_empty(), "§46.5");
    assert!(!folder.page_sizes_sampled, "§46.3");
    // The application lists the same directory and must reach the same
    // number, or the open is stale (§42.3).
    let ours = pulpit_render::images::list_directory(dir.path()).unwrap();
    assert_eq!(folder.source_digest, Some(ours.digest()));

    // Page 1 is img10.png, because the order is natural rather than readdir's.
    let mut picture = job(9, 1, 1, Priority::Audience);
    picture.document = 2;
    supervisor.submit(picture);
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        !frames(events).is_empty()
    });
    let pixels = events
        .iter()
        .find_map(|event| match event {
            RenderEvent::Frame { job, frame, .. } if job.id == RequestId(9) => {
                Some(frame.pixels.clone())
            }
            _ => None,
        })
        .expect("the picture");
    assert_eq!(&pixels[..4], &[90, 90, 90, 255]);

    supervisor.shutdown();
}

/// The presenter's render path for a DjVu, end to end through real worker
/// processes (`SPEC-reader-formats.md` §55, §56.1).
///
/// The document worker is covered separately, in `pulpit`'s
/// `document_worker.rs`; this is the other half — the router inside a spawned
/// renderer picking the DjVu route and frames coming back over the pipe.
///
/// Skips with a message when djvulibre is absent (§63.2). Set
/// `PULPIT_REQUIRE_DJVU=1` to make the skip a failure.
#[test]
fn renders_a_djvu_through_worker_processes() {
    let book =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/djvu_fixture/book.djvu");
    let mut supervisor = start(2);
    supervisor.open(1, &book.display().to_string());

    for (index, page) in [0usize, 1, 2].iter().enumerate() {
        supervisor.submit(job(index as u64 + 1, 1, *page, Priority::Audience));
    }
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).len() == 3
    });

    let opened = events
        .iter()
        .any(|e| matches!(e, RenderEvent::Opened(o) if o.page_count == 3));
    if !opened {
        if std::env::var_os("PULPIT_REQUIRE_DJVU").is_some() {
            panic!("PULPIT_REQUIRE_DJVU is set but the book did not open: {events:?}");
        }
        eprintln!(
            "skipping: no djvulibre in the spawned workers, so the DjVu render path was not \
             exercised. Set PULPIT_REQUIRE_DJVU=1 to make this a failure."
        );
        return;
    }

    let rendered = frames(&events);
    assert_eq!(rendered.len(), 3, "all three pages came back: {events:?}");
    for event in &events {
        if let RenderEvent::Frame { job, frame, .. } = event {
            assert_eq!(frame.width, job.width);
            assert_eq!(frame.pixels.len(), 320 * 180 * 4);
            assert!(
                frame.pixels.iter().any(|b| *b != 0),
                "the frame has content"
            );
        }
    }
}

/// `SPEC-reader-formats.md` §54, end to end through a real worker process: a
/// comic archive opens, its entries are its pages in natural order over the
/// full path, and it reports no source digest because it is one file.
#[test]
fn a_comic_archive_opens_and_renders_through_a_worker() {
    use std::io::Write;

    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("comic.cbz");
    {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, colour) in [
            ("ch-1/page-10.png", 90u8),
            ("ch-1/page-02.png", 20),
            ("ComicInfo.xml", 0),
        ] {
            writer.start_file(name, options).unwrap();
            if name.ends_with(".png") {
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::RgbaImage::from_pixel(64, 32, image::Rgba([colour, colour, colour, 255]))
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                writer.write_all(bytes.get_ref()).unwrap();
            } else {
                writer.write_all(b"<ComicInfo/>").unwrap();
            }
        }
        writer.finish().unwrap();
    }

    let mut supervisor = start(1);
    supervisor.open(3, &path.to_string_lossy());
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::Opened(_) | RenderEvent::OpenFailed { .. }))
    });
    let opened = events
        .iter()
        .find_map(|e| match e {
            RenderEvent::Opened(opened) => Some(opened.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the archive did not open: {events:?}"));

    assert_eq!(opened.page_count, 2, "the XML is not a page");
    assert_eq!(
        opened.source_digest, None,
        "§54.2: an archive is one file, so there is nothing to agree about"
    );

    // Page 1 is page-10.png, because the order is the sorted full path.
    let mut picture = job(11, 1, 1, Priority::Audience);
    picture.document = 3;
    supervisor.submit(picture);
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        !frames(events).is_empty()
    });
    let pixels = events
        .iter()
        .find_map(|event| match event {
            RenderEvent::Frame { job, frame, .. } if job.id == RequestId(11) => {
                Some(frame.pixels.clone())
            }
            _ => None,
        })
        .expect("the page");
    assert_eq!(&pixels[..4], &[90, 90, 90, 255]);

    supervisor.shutdown();
}

/// §54.7 and §61.2, over the protocol: a format pulpit does not read fails
/// the open with a message naming it, not with a corruption report.
#[test]
fn a_rar_comic_fails_its_open_by_name() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("comic.cbr");
    std::fs::write(&path, b"Rar!\x1a\x07\x00").unwrap();

    let mut supervisor = start(1);
    supervisor.open(4, &path.to_string_lossy());
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::Opened(_) | RenderEvent::OpenFailed { .. }))
    });
    let reason = events
        .iter()
        .find_map(|e| match e {
            RenderEvent::OpenFailed { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a refusal: {events:?}"));
    assert!(reason.contains("RAR"), "{reason}");
    assert!(
        !reason.to_lowercase().contains("corrupt") && !reason.to_lowercase().contains("damaged"),
        "{reason}"
    );

    supervisor.shutdown();
}

/// A render job of `width` × `height`, for the tests where the size is the
/// point: whether it travels inline or through the shared region, and
/// whether the region can be made to hold it at all.
fn sized_job(id: u64, page: usize, width: u32, height: u32) -> RenderJob {
    RenderJob {
        width,
        height,
        ..job(id, 1, page, Priority::Presenter)
    }
}

/// The invariant every one of these three tests holds the supervisor to:
/// a submitted job is *answered* — Frame, Failed or Cancelled — never
/// swallowed. The application frees an outstanding-work slot only when an
/// event names the job, so a job that dies silently leaves its frame key
/// pending for ever: every later plan sees the key as already asked for and
/// never submits it again, and the page it was refining keeps its coarse
/// stand-in for as long as it is looked at.
#[test]
fn a_job_the_shared_region_cannot_hold_is_failed_not_swallowed() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    // 20000 × 20000 × 4 bytes is past MAX_REGION_BYTES, so sizing the region
    // fails deterministically, before any filesystem is involved.
    supervisor.submit(sized_job(1, 0, 20_000, 20_000));

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        completed_job_ids(events).contains(&1)
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::Failed { job, .. } if job.id == RequestId(1))),
        "the job the region cannot hold is answered with Failed: {events:?}"
    );

    // The failure poisoned nothing: an ordinary job still renders.
    supervisor.submit(job(2, 1, 1, Priority::Audience));
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        !frames(events).is_empty()
    });
    assert!(
        frames(&events).iter().any(|job| job.id == RequestId(2)),
        "rendering goes on: {events:?}"
    );

    supervisor.shutdown();
}

#[test]
fn a_submit_below_the_generation_floor_is_cancelled_not_swallowed() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    supervisor.cancel_older_than(RenderGeneration(5));
    supervisor.submit(job(1, 1, 0, Priority::Audience));

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        completed_job_ids(events).contains(&1)
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RenderEvent::Cancelled { id } if *id == RequestId(1))),
        "the below-floor submit is answered with Cancelled: {events:?}"
    );

    supervisor.shutdown();
}

/// The slide plan and the reader plan can ask for the same page at the same
/// size, at the same generation, from the same document — two requesters,
/// two ids, one picture. Deduplicating the queue answered only the newer id
/// and left the older requester waiting for ever; both must come back.
#[test]
fn two_identical_jobs_from_different_requesters_are_both_answered() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    // 2048 × 1152 × 4 bytes is over the inline threshold, so these travel
    // through the shared region — and a region job occupies the region until
    // its frame is copied out, which keeps the two duplicates queued together
    // behind the first job rather than racing it to the worker.
    supervisor.submit(sized_job(1, 0, 2_048, 1_152));
    supervisor.submit(sized_job(2, 1, 2_048, 1_152));
    supervisor.submit(sized_job(3, 1, 2_048, 1_152));

    let events = collect_until(&mut supervisor, Duration::from_secs(20), |events| {
        frames(events).len() == 3
    });
    let rendered: Vec<u64> = frames(&events).iter().map(|job| job.id.0).collect();
    assert_eq!(
        rendered.len(),
        3,
        "both requesters of the identical picture are answered: {events:?}"
    );
    for id in [1, 2, 3] {
        assert!(
            rendered.contains(&id),
            "job {id} was answered: {rendered:?}"
        );
    }

    supervisor.shutdown();
}

/// The pool is elastic: any burst may take the whole configured pool — a
/// worker's share of the document is worth paying for a second or two of
/// warming — and a quiet spell gives it back, down to the one worker an idle
/// application has always kept. Growth without retirement charged a whole
/// session for its busiest moment.
#[test]
fn a_burst_takes_the_pool_and_idleness_gives_it_back() {
    let mut config = config(3);
    config.retire_after = Duration::from_millis(150);
    let mut supervisor = start_with(config, &[]);
    supervisor.open(1, "fixture:pages=200");

    for page in 0..48usize {
        supervisor.submit(job(page as u64 + 1, 1, page, Priority::Ancillary));
    }
    assert!(
        supervisor.diagnostics().workers_alive > 1,
        "contention grew the pool"
    );

    // Drain every frame, then sit quiet past the retirement age.
    let deadline = Instant::now() + Duration::from_secs(20);
    while supervisor.in_flight() > 0 || supervisor.queued() > 0 {
        assert!(Instant::now() < deadline, "jobs never drained");
        supervisor.pump_blocking(Duration::from_millis(50));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while supervisor.diagnostics().workers_alive > 1 {
        assert!(
            Instant::now() < deadline,
            "idle workers were never retired: {:?}",
            supervisor.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(50));
        supervisor.pump();
    }
    assert!(supervisor.diagnostics().workers_retired >= 1);

    // And the pool is still a pool: new contention grows it again.
    for page in 0..48usize {
        supervisor.submit(job(1000 + page as u64, 1, page, Priority::Ancillary));
    }
    assert!(
        supervisor.diagnostics().workers_alive > 1,
        "a retired pool must grow back"
    );
    supervisor.shutdown();
}

/// And a page somebody is waiting for still does, which is what the rest of
/// the configured pool is for.
#[test]
fn a_page_a_window_waits_for_still_grows_the_pool() {
    let mut supervisor = start(4);
    supervisor.open(1, "fixture:pages=200");
    let before = supervisor.diagnostics().workers_alive;

    for page in 0..64usize {
        supervisor.submit(job(page as u64 + 1, 1, page, Priority::Audience));
    }
    assert!(
        supervisor.diagnostics().workers_alive > before,
        "contention on the projector's own frame is what the pool exists for"
    );
}
