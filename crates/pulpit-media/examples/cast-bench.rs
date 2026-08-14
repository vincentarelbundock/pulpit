// Scratch: what frame rate does the JPEG screencast path actually sustain,
// and where do frames fall out of the pipeline on the way?
use pulpit_core::overlay::{ContentKind, PlaybackParams};
use pulpit_core::{OverlayId, RenderGeneration};
use pulpit_media::protocol::{CapabilityRequest, SessionSource};
use pulpit_media::{MediaConfig, MediaSupervisor, SessionEvent, Viewport, WorkerCommand};
use std::time::{Duration, Instant};

const PAGE: &str = r#"<!doctype html><body style="margin:0;background:#000">
<canvas id=c style="width:100vw;height:100vh"></canvas><script>
const x=document.getElementById('c').getContext('2d');let i=0;
function f(){i=(i+3)%360;x.fillStyle=`hsl(${i},80%,50%)`;
x.fillRect(0,0,4000,4000);requestAnimationFrame(f);} f();
</script></body>"#;

fn worker_command() -> Option<WorkerCommand> {
    let mut dir = std::env::current_exe().unwrap();
    dir.pop();
    if dir.ends_with("examples") || dir.ends_with("deps") {
        dir.pop();
    }
    let app = dir.join("pulpit");
    if !app.is_file() {
        return None;
    }
    Some(WorkerCommand::Explicit {
        program: app,
        args: vec!["--media-worker=chromium".into()],
    })
}

fn stage(label: &str) -> std::path::PathBuf {
    let staging = std::env::temp_dir().join(format!("bench-{}-{label}", std::process::id()));
    let _ = std::fs::create_dir_all(&staging);
    std::fs::write(staging.join("index.html"), PAGE).unwrap();
    staging
}

fn open(
    s: &mut MediaSupervisor,
    overlay: u64,
    staging: &std::path::Path,
    width: u32,
    height: u32,
    command: &WorkerCommand,
) -> pulpit_media::SessionId {
    s.open(
        OverlayId(overlay),
        RenderGeneration(1),
        ContentKind::Web,
        SessionSource::Bundle {
            root: staging.display().to_string(),
            entrypoint: "index.html".into(),
        },
        Viewport::new(width, height, 1.0),
        PlaybackParams::default(),
        &CapabilityRequest::for_kind(ContentKind::Web),
        |_| command.clone(),
    )
}

fn wait_first_frame(s: &mut MediaSupervisor, command: &WorkerCommand) -> Option<Instant> {
    let warm = Instant::now();
    while warm.elapsed() < Duration::from_secs(25) {
        for e in s.poll(|_| command.clone()) {
            if let SessionEvent::Frame { .. } = e {
                return Some(Instant::now());
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn report(s: &MediaSupervisor, frames: u32, bytes: usize, secs: f64) {
    println!(
        "  delivered: {:.1} fps, {:.0} MB/s to the app",
        frames as f64 / secs,
        bytes as f64 / secs / 1e6
    );
    let c = s.worker_counters();
    println!(
        "  worker: {} from browser, {} discarded before decode, {} decoded ({} scaled, {} as-is), {} published, {} ring-dropped",
        c.cdp_frames_received,
        c.frames_discarded_before_decode,
        c.frames_decoded,
        c.frames_scaled,
        c.frames_scale_elided,
        c.frames_published,
        c.ring_dropped,
    );
    println!(
        "  supervisor: {} forwarded, {} coalesced, {:.1} MiB of rings",
        s.frames_forwarded(),
        s.frames_coalesced(),
        s.ring_bytes() as f64 / 1_048_576.0,
    );
}

fn bench(label: &str, config: MediaConfig, width: u32, height: u32, seconds: u64) {
    let Some(command) = worker_command() else {
        println!("{label}: pulpit not built");
        return;
    };
    let staging = stage(label);
    let mut s = MediaSupervisor::new(config);
    open(&mut s, 1, &staging, width, height, &command);

    // Let the browser start before timing.
    let Some(start) = wait_first_frame(&mut s, &command) else {
        println!("{label}: no frames");
        return;
    };

    let (mut frames, mut bytes) = (0u32, 0usize);
    while start.elapsed() < Duration::from_secs(seconds) {
        for e in s.poll(|_| command.clone()) {
            if let SessionEvent::Frame { rgba, .. } = e {
                frames += 1;
                bytes += rgba.len();
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // One more beat so the worker's final one-second counter report lands.
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_millis(1300) {
        for _ in s.poll(|_| command.clone()) {}
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("{label} {width}x{height}:");
    report(&s, frames, bytes, start.elapsed().as_secs_f64());
    let _ = std::fs::remove_dir_all(&staging);
}

/// Three sessions, two deactivated after their first frame: the inactive
/// ones must stop producing browser frames entirely.
fn bench_inactive(seconds: u64) {
    let Some(command) = worker_command() else {
        println!("inactive: pulpit not built");
        return;
    };
    let staging = stage("inactive");
    let mut s = MediaSupervisor::new(MediaConfig::default());
    let keep = open(&mut s, 1, &staging, 960, 540, &command);
    let park_a = open(&mut s, 2, &staging, 960, 540, &command);
    let park_b = open(&mut s, 3, &staging, 960, 540, &command);
    let Some(_) = wait_first_frame(&mut s, &command) else {
        println!("inactive: no frames");
        return;
    };
    s.set_active(park_a, false);
    s.set_active(park_b, false);
    // Give the stop a moment, then count who still produces.
    std::thread::sleep(Duration::from_millis(500));
    for _ in s.poll(|_| command.clone()) {}
    let baseline = s.worker_counters();
    let start = Instant::now();
    let (mut active_frames, mut parked_frames) = (0u32, 0u32);
    while start.elapsed() < Duration::from_secs(seconds) {
        for e in s.poll(|_| command.clone()) {
            if let SessionEvent::Frame { session, .. } = e {
                if session == keep {
                    active_frames += 1;
                } else if session == park_a || session == park_b {
                    parked_frames += 1;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_millis(1300) {
        for _ in s.poll(|_| command.clone()) {}
        std::thread::sleep(Duration::from_millis(20));
    }
    let now = s.worker_counters();
    println!("inactive 1 active + 2 parked, 960x540 over {seconds}s:");
    println!(
        "  active session: {:.1} fps; parked sessions: {} frames (want 0)",
        active_frames as f64 / start.elapsed().as_secs_f64(),
        parked_frames,
    );
    println!(
        "  browser frames while parked: {} (one active session's worth)",
        now.cdp_frames_received - baseline.cdp_frames_received,
    );
    let _ = std::fs::remove_dir_all(&staging);
}

fn main() {
    let uncapped = MediaConfig {
        max_capture_fps: 0,
        ..MediaConfig::default()
    };
    bench("uncapped-inset", uncapped.clone(), 960, 540, 6);
    bench("uncapped-fullscreen", uncapped, 1920, 1080, 6);
    bench("capped-inset", MediaConfig::default(), 960, 540, 6);
    bench("capped-fullscreen", MediaConfig::default(), 1920, 1080, 6);
    bench_inactive(6);
}
