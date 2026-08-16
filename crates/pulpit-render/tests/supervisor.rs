//! Supervisor integration tests with real worker processes.
//!
//! These run the fixture backend so they need neither PDFium nor a graphical
//! session, which is what makes them CI-safe.

use std::time::{Duration, Instant};

use pulpit_core::notes::Region;
use pulpit_core::RenderGeneration;
use pulpit_render::protocol::{Priority, Quality, RenderJob, RequestId};
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
        quality: Quality::Refined,
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
fn a_superseded_request_for_the_same_page_replaces_the_queued_one() {
    let mut supervisor = start(1);
    supervisor.open(1, "fixture:pages=30");

    // Fill the single worker, then queue two requests for the same page.
    supervisor.submit(job(1, 1, 0, Priority::Audience));
    supervisor.submit(job(2, 1, 4, Priority::Adjacent));
    supervisor.submit(job(3, 1, 4, Priority::Audience));
    assert!(
        supervisor.queued() <= 1,
        "the older duplicate was dropped, not queued"
    );

    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).len() >= 2
    });
    let ids: Vec<u64> = frames(&events).iter().map(|job| job.id.0).collect();
    assert!(
        !ids.contains(&2),
        "the superseded request never rendered: {ids:?}"
    );
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
    supervisor.open(1, "fixture:pages=30");

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

    // This is where the stall shows. With the harness deadline raised so the
    // worker is not killed, exactly five frames arrive and the sixth never
    // does — under parallel load, reproducibly, with no event to say why.
    // The 5s deadline in `config` is what currently recovers it, which is why
    // this test is flaky on CI rather than simply broken.
    let events = collect_until(&mut supervisor, Duration::from_secs(10), |events| {
        frames(events).len() == 6
    });
    assert_eq!(
        frames(&events).len(),
        6,
        "the rest followed. A short count here means work was dropped, not \
         merely slow — check for a killed worker in: {events:?}"
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
    // six frames does not mean the six rings have all been attempted yet.
    // Let the stragglers land, otherwise this counts a ring that arrives
    // between two waits and calls coalescing a queue.
    std::thread::sleep(Duration::from_millis(500));

    // Six frames have been drained. However many rings they produced, the
    // channel holds at most the one that says "there may be more".
    let mut rings = 0;
    while wakeup.wait(Duration::from_millis(10)) == Wakeup::Ring {
        rings += 1;
        assert!(rings <= 1, "the doorbell coalesces rather than queues");
    }
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
