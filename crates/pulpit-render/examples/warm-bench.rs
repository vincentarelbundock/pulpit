//! How fast a whole deck's thumbnails can actually be warmed, end to end.
//!
//! [`thumb-bench`](thumb-bench.rs) measures what one page costs inside the
//! renderer. This measures what the *harness* costs: the application drains
//! renderer events on its tick, so a warming pass can only ever go as fast as
//! `outstanding` frames per tick however quickly the workers finish them. The
//! numbers below are what decides whether the limit is set anywhere near
//! right.
//!
//! `cargo run --release -p pulpit-render --example warm-bench -- [pages]`
//! Uses the fixture backend, so it needs neither PDFium nor a display.

use std::time::{Duration, Instant};

use pulpit_core::notes::Region;
use pulpit_core::RenderGeneration;
use pulpit_render::protocol::{Priority, Quality, RenderJob};
use pulpit_render::supervisor::{RenderEvent, RendererSupervisor, SupervisorConfig, WorkerCommand};

/// The application's live tick: renderer events are drained once per tick.
const TICK: Duration = Duration::from_millis(50);

/// The worker binary sitting beside this example.
///
/// Never `CurrentExe`: `current_exe` here is the example, which does not
/// answer to a worker flag and would simply run `main` again — each copy
/// starting its own pool, which is a fork bomb, not a benchmark. Only the
/// application, whose binary really does have a worker role, may use
/// `CurrentExe`.
fn worker() -> Option<std::path::PathBuf> {
    let mut dir = std::env::current_exe().ok()?;
    dir.pop();
    if dir.ends_with("examples") || dir.ends_with("deps") {
        dir.pop();
    }
    let worker = dir.join("pulpit-render-worker");
    worker.is_file().then_some(worker)
}

fn main() {
    let Some(worker) = worker() else {
        eprintln!("build the worker first: cargo build --release -p pulpit-render");
        return;
    };
    std::env::set_var("PULPIT_FORCE_FIXTURE_BACKEND", "1");
    if std::env::args().nth(1).as_deref() == Some("sweep") {
        return sweep(&worker);
    }
    let pages: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(500);

    println!("warming {pages} pages, draining events every {TICK:?}\n");
    println!(
        "{:>7}  {:>11}  {:>9}  {:>9}",
        "workers", "outstanding", "elapsed", "pages/s"
    );
    for workers in [2usize, 6] {
        for outstanding in [8usize, 32, 64, 128] {
            let elapsed = warm(&worker, pages, workers, outstanding, 240, 135);
            println!(
                "{workers:>7}  {outstanding:>11}  {:>7.0} ms  {:>9.0}",
                elapsed.as_secs_f64() * 1000.0,
                pages as f64 / elapsed.as_secs_f64()
            );
        }
    }
}

/// Frame-size sweep across `INLINE_FRAME_BYTES`: what one frame costs the
/// harness on either side of the threshold, at the application's own settings
/// (two workers, thirty-two outstanding).
///
/// Two things to read off the table. The cliff: the first row past the
/// threshold falls from pipelined throughput to one frame per worker per
/// tick, however small the crossing. The slope: how throughput decays with
/// size *within* the inline band is the copy overhead — if the decay is
/// gentle up to the threshold, the threshold sits on the flat part of the
/// plateau and its exact value is not load-bearing on this machine.
fn sweep(worker: &std::path::Path) {
    use pulpit_render::protocol::INLINE_FRAME_BYTES;
    const PAGES: usize = 200;
    println!("sweeping frame sizes, 2 workers, 32 outstanding, {PAGES} pages\n");
    println!(
        "{:>10}  {:>9}  {:>7}  {:>9}  {:>9}  {:>8}",
        "size", "bytes", "path", "elapsed", "pages/s", "ms/page"
    );
    for (width, height) in [
        (240u32, 135u32),
        (480, 270),
        (640, 360),
        (800, 450),
        (880, 495),
        (960, 540),
        (1000, 562),
        (1280, 720),
        (1920, 1080),
    ] {
        let bytes = width as u64 * height as u64 * 4;
        let inline = bytes <= INLINE_FRAME_BYTES;
        let elapsed = warm(worker, PAGES, 2, 32, width, height);
        println!(
            "{:>10}  {bytes:>9}  {:>7}  {:>7.0} ms  {:>9.0}  {:>8.2}",
            format!("{width}x{height}"),
            if inline { "inline" } else { "region" },
            elapsed.as_secs_f64() * 1000.0,
            PAGES as f64 / elapsed.as_secs_f64(),
            elapsed.as_secs_f64() * 1000.0 / PAGES as f64,
        );
    }
}

/// One warming pass, driven exactly the way the application drives it: submit
/// up to `outstanding`, wait for the tick, drain, refill.
fn warm(
    worker: &std::path::Path,
    pages: usize,
    workers: usize,
    outstanding: usize,
    width: u32,
    height: u32,
) -> Duration {
    let mut supervisor = RendererSupervisor::start(SupervisorConfig {
        workers,
        command: WorkerCommand::Explicit {
            program: worker.to_path_buf(),
            args: Vec::new(),
        },
        deadline: Duration::from_secs(5),
        max_restarts: 3,
        restart_window: Duration::from_secs(60),
    })
    .expect("start supervisor");

    let mut queued = 0usize;
    let mut done = 0usize;
    let mut in_flight = 0usize;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(60);

    while done < pages && Instant::now() < deadline {
        while in_flight < outstanding && queued < pages {
            let id = supervisor.next_request_id();
            supervisor.submit(RenderJob {
                id,
                generation: RenderGeneration(0),
                document: 1,
                page: queued,
                region: Region::FULL,
                width,
                height,
                priority: Priority::Ancillary,
                quality: Quality::Refined,
                region_name: String::new(),
            });
            queued += 1;
            in_flight += 1;
        }
        // The application's tick: one drain per interval, however much the
        // workers finished in between.
        std::thread::sleep(TICK);
        for event in supervisor.pump() {
            if matches!(
                event,
                RenderEvent::Frame { .. }
                    | RenderEvent::Failed { .. }
                    | RenderEvent::Cancelled { .. }
            ) {
                done += 1;
                in_flight -= 1;
            }
        }
    }
    start.elapsed()
}
