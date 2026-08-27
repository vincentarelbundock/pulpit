//! End-to-end playback against the real worker binary.
//!
//! The unit tests prove the protocol and the selection rules in isolation;
//! this proves the thing a presenter actually cares about — that a GIF, a
//! clip or a page put on a slide *moves*. It spawns the real application
//! binary in its browser-worker role, opens a real session over a real
//! shared-memory ring, and waits for successive frames to differ.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use pulpit_core::overlay::{ContentKind, PlaybackParams};
use pulpit_core::{OverlayId, RenderGeneration};
use pulpit_media::protocol::{
    CapabilityRequest, InputEvent, PlaybackProgress, SessionId, SessionSource, VideoCommand,
};
use pulpit_media::{
    MediaConfig, MediaSupervisor, RuntimeId, SessionEvent, Viewport, WorkerCommand,
};

/// A browser start is slow; every deadline here is a generous ceiling.
const SLOW: Duration = Duration::from_secs(40);

/// Only one test drives a browser at a time.
///
/// Not merely to be polite about load. `browser_profiles` counts a *global*
/// namespace — every `/tmp/pulpit-browser-*` on the machine — so the
/// browser-sharing test can only tell how many browsers it started if nothing
/// else is starting one meanwhile. Run in parallel it sees the other tests'
/// profiles and reports that two overlays started four browsers, which is a
/// statement about `cargo test`'s thread count rather than about the code.
///
/// Serialising costs wall-clock time and buys an answer that means something.
///
/// Poisoning is ignored deliberately: a panicking test has already reported
/// its own failure, and there is no shared state here to be left inconsistent,
/// so cascading a second failure into every later test would only hide it.
static BROWSER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn browser_lock() -> std::sync::MutexGuard<'static, ()> {
    BROWSER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The application binary, which hosts the browser worker as a role.
///
/// Deliberately the real executable and the real `--media-worker=` flag: the
/// worker used to be a separate binary that `cargo run` never built, so the
/// application shipped with media that silently never played. Spawning it the
/// way the supervisor does is what keeps that from coming back.
///
/// Returning `None` skips rather than fails: a checkout that has not built
/// the application yet should not report a red suite.
fn app_binary() -> Option<PathBuf> {
    let mut directory = std::env::current_exe().ok()?;
    directory.pop();
    if directory.ends_with("deps") {
        directory.pop();
    }
    let path = directory.join("pulpit");
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("skipping: {} has not been built", path.display());
        None
    }
}

/// Drive the supervisor until `want` frames have arrived or time runs out.
fn collect_frames(
    supervisor: &mut MediaSupervisor,
    command: WorkerCommand,
    want: usize,
    deadline: Duration,
) -> (Vec<Vec<u8>>, Vec<String>) {
    let started = Instant::now();
    let mut frames = Vec::new();
    let mut problems = Vec::new();
    while frames.len() < want && started.elapsed() < deadline {
        for event in supervisor.poll(|_| command.clone()) {
            match event {
                SessionEvent::Frame { rgba, .. } => frames.push((*rgba).clone()),
                SessionEvent::Failed { error, .. } => problems.push(error.message),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    (frames, problems)
}

#[test]
fn an_html_overlay_renders_through_an_installed_browser() {
    // The browser adapter had no end-to-end test at all: every check was of
    // its pieces — flag construction, the asset origin, base64 — never of a
    // page actually reaching a surface. This is that test.
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let mut supervisor = MediaSupervisor::new(MediaConfig::default());
    let probe = supervisor
        .probe(RuntimeId::ExternalChromium)
        .cloned()
        .expect("the browser runtime should have been probed");
    if !probe.is_available() {
        eprintln!("skipping: {}", probe.availability.detail());
        return;
    }
    eprintln!(
        "using {} ({})",
        probe
            .executable
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        probe.version.clone().unwrap_or_default()
    );

    // A page that changes on its own, so "it animated" is provable.
    let staging = tempfile::tempdir().unwrap();
    let root = staging.path().join("bundle");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("index.html"),
        r#"<!doctype html><html><body style="margin:0;background:#000">
           <canvas id=c width=64 height=64></canvas><script>
           const x=document.getElementById('c').getContext('2d');let i=0;
           function f(){i=(i+8)%256;x.fillStyle=`rgb(${i},${255-i},128)`;
           x.fillRect(0,0,64,64);requestAnimationFrame(f);} f();
           </script></body></html>"#,
    )
    .unwrap();

    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    supervisor.open(
        OverlayId(9),
        RenderGeneration(1),
        ContentKind::Web,
        SessionSource::Bundle {
            root: root.display().to_string(),
            entrypoint: "index.html".to_string(),
        },
        Viewport::new(64, 64, 1.0),
        PlaybackParams::default(),
        &CapabilityRequest::for_kind(ContentKind::Web),
        |_| command.clone(),
    );

    // A browser start is slow; this is a generous ceiling, not a target.
    let (frames, problems) = collect_frames(&mut supervisor, command, 3, Duration::from_secs(40));
    assert!(problems.is_empty(), "the session failed: {problems:?}");
    assert!(
        frames.len() >= 3,
        "expected the page to keep producing frames, got {}",
        frames.len()
    );
    assert!(
        frames.iter().any(|frame| frame != &frames[0]),
        "every frame was identical — the page is not animating"
    );
}

/// One file from `examples/media-assets`, or `None` when it is not checked out.
///
/// These tests need real media to play, and a working tree without the assets
/// should skip rather than fail -- the same reason the browser probe below
/// hands back `None`.
fn media_asset(name: &str) -> Option<PathBuf> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/media-assets")
        .join(name);
    if !source.is_file() {
        eprintln!("skipping: {} is not present", source.display());
        return None;
    }
    Some(source)
}

/// A supervisor with an installed Chromium-family browser behind it, or `None`
/// on a machine that has none.
fn chromium_supervisor() -> Option<MediaSupervisor> {
    let supervisor = MediaSupervisor::new(MediaConfig::default());
    if !supervisor
        .probe(RuntimeId::ExternalChromium)
        .is_some_and(|probe| probe.is_available())
    {
        eprintln!("skipping: no Chromium-family browser installed");
        return None;
    }
    Some(supervisor)
}

/// Chrome playing a bare media file through a generated wrapper page.
fn browser_plays(kind: ContentKind, asset: &str, label: &str) {
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let Some(source) = media_asset(asset) else {
        return;
    };
    let Some(mut supervisor) = chromium_supervisor() else {
        return;
    };

    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    supervisor.open(
        OverlayId(11),
        RenderGeneration(1),
        kind,
        SessionSource::File {
            path: source.display().to_string(),
        },
        Viewport::new(160, 90, 1.0),
        PlaybackParams {
            autoplay: true,
            repeat: true,
            mute: true,
            ..Default::default()
        },
        &CapabilityRequest::for_kind(kind),
        |_| command.clone(),
    );

    let (frames, problems) = collect_frames(&mut supervisor, command, 4, Duration::from_secs(40));
    assert!(problems.is_empty(), "{label}: {problems:?}");
    assert!(frames.len() >= 4, "{label}: only {} frames", frames.len());
    assert!(
        frames.iter().any(|frame| frame != &frames[0]),
        "{label}: every frame was identical"
    );
}

#[test]
fn the_browser_plays_an_animated_gif() {
    // No decoder of our own involved: the browser already knows GIF, and the
    // only thing pulpit supplies is the document around it.
    browser_plays(ContentKind::AnimatedImage, "bouncing.gif", "gif");
}

#[test]
fn the_browser_plays_a_video() {
    browser_plays(ContentKind::Video, "clip.mp4", "video");
}

#[test]
fn clicking_an_animated_image_stops_it_and_clicking_again_restarts_it() {
    // The presenter's only control over a GIF is the click, so this is the
    // whole of its transport and it is worth proving against a real browser
    // rather than by reading the generated page back.
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let Some(source) = media_asset("bouncing.gif") else {
        return;
    };
    let Some(mut supervisor) = chromium_supervisor() else {
        return;
    };

    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    let session = supervisor.open(
        OverlayId(12),
        RenderGeneration(1),
        ContentKind::AnimatedImage,
        SessionSource::File {
            path: source.display().to_string(),
        },
        Viewport::new(160, 90, 1.0),
        PlaybackParams {
            autoplay: true,
            repeat: true,
            ..Default::default()
        },
        &CapabilityRequest::for_kind(ContentKind::AnimatedImage),
        |_| command.clone(),
    );

    // Running: successive frames differ.
    let (moving, problems) = collect_frames(&mut supervisor, command.clone(), 4, SLOW);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(
        moving.iter().any(|frame| frame != &moving[0]),
        "the GIF was not animating before the click"
    );

    click(&mut supervisor, session, command.clone());
    // Frozen: whatever arrives from here on is the same picture. Frames are
    // still collected rather than assumed absent — a screencast may repeat a
    // frame — so this asserts stillness, not silence.
    let (still, _) = collect_frames(&mut supervisor, command.clone(), 6, Duration::from_secs(3));
    if let Some(first) = still.first() {
        assert!(
            still.iter().all(|frame| frame == first),
            "the GIF kept moving after the click: {} distinct frames",
            still.iter().filter(|frame| *frame != first).count() + 1
        );
    }

    click(&mut supervisor, session, command.clone());
    let (again, problems) = collect_frames(&mut supervisor, command, 4, SLOW);
    assert!(problems.is_empty(), "{problems:?}");
    eprintln!(
        "frames: {} moving, {} while frozen, {} after restart",
        moving.len(),
        still.len(),
        again.len()
    );
    assert!(
        again.iter().any(|frame| frame != &again[0]),
        "the second click did not restart the GIF ({} frames)",
        again.len()
    );
}

#[test]
fn two_overlays_on_one_slide_share_a_single_browser_process() {
    // The cost of the single-runtime design was a browser per overlay: a
    // process and a hundred megabytes each, paid again on every slide with
    // media on it. One process, one page per overlay, and both still animate.
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let Some(mut supervisor) = chromium_supervisor() else {
        return;
    };

    let staging = tempfile::tempdir().unwrap();
    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    let before_opening = browser_profiles();

    let mut sessions = Vec::new();
    for (index, offset) in [(0u8, 8), (1, 16)] {
        let root = staging.path().join(format!("bundle{index}"));
        std::fs::create_dir_all(&root).unwrap();
        // Each page animates on its own, so "both are live" is provable and
        // not just "both were opened".
        std::fs::write(
            root.join("index.html"),
            format!(
                r#"<!doctype html><html><body style="margin:0;background:#000">
                   <canvas id=c width=64 height=64></canvas><script>
                   const x=document.getElementById('c').getContext('2d');let i=0;
                   function f(){{i=(i+{offset})%256;x.fillStyle=`rgb(${{i}},${{255-i}},128)`;
                   x.fillRect(0,0,64,64);requestAnimationFrame(f);}} f();
                   </script></body></html>"#
            ),
        )
        .unwrap();
        sessions.push(supervisor.open(
            OverlayId(20 + index as u64),
            RenderGeneration(1),
            ContentKind::Web,
            SessionSource::Bundle {
                root: root.display().to_string(),
                entrypoint: "index.html".to_string(),
            },
            Viewport::new(64, 64, 1.0),
            PlaybackParams::default(),
            &CapabilityRequest::for_kind(ContentKind::Web),
            |_| command.clone(),
        ));
    }

    // Frames per session, not in total: one overlay animating for both would
    // otherwise pass.
    let started = Instant::now();
    let mut frames: std::collections::HashMap<SessionId, Vec<Vec<u8>>> = Default::default();
    let mut problems = Vec::new();
    while started.elapsed() < SLOW
        && sessions
            .iter()
            .any(|id| frames.get(id).map(Vec::len).unwrap_or(0) < 3)
    {
        for event in supervisor.poll(|_| command.clone()) {
            match event {
                SessionEvent::Frame { session, rgba, .. } => {
                    frames.entry(session).or_default().push((*rgba).clone())
                }
                SessionEvent::Failed { error, .. } => problems.push(error.message),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(problems.is_empty(), "a session failed: {problems:?}");
    for id in &sessions {
        let seen = frames.get(id).map(Vec::as_slice).unwrap_or_default();
        assert!(
            seen.len() >= 3,
            "session {id} produced {} frames",
            seen.len()
        );
        assert!(
            seen.iter().any(|frame| frame != &seen[0]),
            "session {id} never changed — its page is not animating"
        );
    }
    assert_eq!(
        supervisor.worker_count(),
        1,
        "both overlays must be hosted by one worker process"
    );
    let started_here: Vec<String> = browser_profiles()
        .difference(&before_opening)
        .cloned()
        .collect();
    assert_eq!(
        started_here.len(),
        1,
        "two overlays started {} browsers: {started_here:?}",
        started_here.len()
    );
}

/// Shutting the supervisor down takes the browser with it.
///
/// This is a regression test for a leak that cost gigabytes. `Worker::shutdown`
/// wrote `Shutdown` to the worker and then called `try_wait` *once*: no process
/// exits in the microsecond that takes, so the answer was always "still
/// running" and the worker was killed on the spot. `SIGKILL` runs no
/// destructors, so the browser it had launched was never closed and never
/// killed either — it was reparented and outlived the machine's patience,
/// profile directory and all. Every presentation leaked one.
///
/// Both halves are checked because they fail together and are fixed together:
/// the process must be gone, and the private profile it was given must have
/// been removed — that removal only happens in the destructor a kill skips.
#[test]
fn closing_the_supervisor_leaves_no_browser_behind() {
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let Some(source) = media_asset("clip.mp4") else {
        return;
    };
    let Some(mut supervisor) = chromium_supervisor() else {
        return;
    };

    let before = browser_profiles();
    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    supervisor.open(
        OverlayId(14),
        RenderGeneration(1),
        ContentKind::Video,
        SessionSource::File {
            path: source.display().to_string(),
        },
        Viewport::new(160, 90, 1.0),
        PlaybackParams {
            autoplay: true,
            mute: true,
            ..Default::default()
        },
        &CapabilityRequest::for_kind(ContentKind::Video),
        |_| command.clone(),
    );

    // Wait until it is genuinely up, or "nothing leaked" would be trivially
    // true of a browser that never started.
    let (frames, problems) = collect_frames(&mut supervisor, command, 2, SLOW);
    assert!(problems.is_empty(), "the session failed: {problems:?}");
    assert!(!frames.is_empty(), "the browser never produced a frame");

    let started: Vec<String> = browser_profiles().difference(&before).cloned().collect();
    assert_eq!(started.len(), 1, "expected one browser, got {started:?}");
    let profile = started.into_iter().next().unwrap();

    supervisor.shutdown();

    // The process list is allowed a moment to catch up: the browser's children
    // are reaped after it exits, not with it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while browser_profiles().contains(&profile) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !browser_profiles().contains(&profile),
        "the browser outlived the supervisor: {profile}"
    );
    assert!(
        !std::path::Path::new(&profile).exists(),
        "the browser's private profile was left on disk: {profile}"
    );
}

/// The private profiles pulpit-started browsers are currently using.
///
/// One browser is many processes — renderer, GPU, zygote — but exactly one
/// profile, so the profiles are what can be counted. Only pulpit's own
/// are named this way, and the answer is compared against a baseline taken
/// before opening anything, so another test's browser or the developer's own
/// Chrome cannot decide the result.
fn browser_profiles() -> std::collections::HashSet<String> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-eo", "args"])
        .output()
    else {
        eprintln!("ps is unavailable; the profile count is skipped");
        return Default::default();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            line.split("--user-data-dir=")
                .nth(1)?
                .split_whitespace()
                .next()
                .filter(|profile| profile.contains("pulpit-browser-"))
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn an_overlay_keeps_animating_after_its_viewport_changes() {
    // Resizing restarts the screencast, so that Chrome encodes frames at the
    // new size instead of the worker resampling every one of them. Restarting
    // it is also the easiest thing to get wrong — a stopped screencast that
    // never starts again is a frozen overlay — so the resize is proved to
    // leave the content still moving.
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let Some(mut supervisor) = chromium_supervisor() else {
        return;
    };

    let staging = tempfile::tempdir().unwrap();
    let root = staging.path().join("bundle");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("index.html"),
        r#"<!doctype html><html><body style="margin:0;background:#000">
           <canvas id=c width=64 height=64 style="width:100%;height:100%"></canvas><script>
           const x=document.getElementById('c').getContext('2d');let i=0;
           function f(){i=(i+8)%256;x.fillStyle=`rgb(${i},${255-i},128)`;
           x.fillRect(0,0,64,64);requestAnimationFrame(f);} f();
           </script></body></html>"#,
    )
    .unwrap();

    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    let session = supervisor.open(
        OverlayId(21),
        RenderGeneration(1),
        ContentKind::Web,
        SessionSource::Bundle {
            root: root.display().to_string(),
            entrypoint: "index.html".to_string(),
        },
        Viewport::new(64, 64, 1.0),
        PlaybackParams::default(),
        &CapabilityRequest::for_kind(ContentKind::Web),
        |_| command.clone(),
    );

    let (before, problems) = collect_frames(&mut supervisor, command.clone(), 3, SLOW);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(before.len() >= 3, "the overlay never started");

    // Smaller than the ring slot the session was opened with, so the change is
    // one the worker will accept.
    supervisor.set_viewport(session, Viewport::new(48, 48, 1.0));

    let started = Instant::now();
    let mut after: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    let mut problems = Vec::new();
    while started.elapsed() < SLOW && after.len() < 4 {
        for event in supervisor.poll(|_| command.clone()) {
            match event {
                SessionEvent::Frame {
                    width,
                    height,
                    rgba,
                    ..
                } => after.push((width, height, (*rgba).clone())),
                SessionEvent::Failed { error, .. } => problems.push(error.message),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        problems.is_empty(),
        "the resize failed the session: {problems:?}"
    );
    assert!(
        after.len() >= 4,
        "the overlay stopped producing frames after the resize ({} arrived)",
        after.len()
    );
    let resized = after.last().unwrap();
    assert_eq!(
        (resized.0, resized.1),
        (48, 48),
        "frames still arrive at the old size"
    );
    assert!(
        after.iter().any(|frame| frame.2 != after[0].2),
        "the page stopped animating after the resize"
    );
}

/// The presenter's transport is drawn by pulpit, not by the page, so the
/// host has to be able to both drive playback and see where it has reached.
/// Neither half is provable by reading the generated document back: the
/// commands go out as CDP and the position comes back as a CDP binding call.
#[test]
fn the_host_can_drive_a_video_and_watch_its_playhead() {
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let Some(source) = media_asset("clip.mp4") else {
        return;
    };
    let Some(mut supervisor) = chromium_supervisor() else {
        return;
    };

    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    let session = supervisor.open(
        OverlayId(13),
        RenderGeneration(1),
        ContentKind::Video,
        SessionSource::File {
            path: source.display().to_string(),
        },
        Viewport::new(160, 90, 1.0),
        PlaybackParams {
            autoplay: true,
            repeat: true,
            mute: true,
            ..Default::default()
        },
        &CapabilityRequest::for_kind(ContentKind::Video),
        |_| command.clone(),
    );

    // Playing: the reported position advances on its own. Waited for rather
    // than counted — the opening burst is `loadedmetadata`, `durationchange`
    // and `play`, all of them truthfully at zero, so "the first few reports"
    // is not the same question as "did it move".
    let playing = collect_progress(
        &mut supervisor,
        command.clone(),
        SLOW,
        |seen| matches!(seen.first(), Some(first) if seen.iter().any(|p| p.position > first.position)),
    );
    assert!(
        !playing.is_empty(),
        "the page never reported a playhead, so a transport could draw nothing"
    );
    assert!(
        playing.iter().any(|p| p.position > playing[0].position),
        "the position never advanced: {playing:?}"
    );
    assert!(
        playing.iter().any(|p| p.duration.is_some()),
        "no duration was ever reported, so a scrub bar has no range"
    );

    // Paused by a host command, which is the thing the widget will send.
    supervisor.video_command(session, VideoCommand::Pause);
    let settling = collect_progress(&mut supervisor, command.clone(), SLOW, |seen| {
        seen.iter().any(|p| p.paused)
    });
    assert!(
        settling.iter().any(|p| p.paused),
        "a pause command did not reach the page: {settling:?}"
    );

    // Sought by a host command: the playhead moves where it was told.
    supervisor.video_command(session, VideoCommand::Seek { seconds: 1.5 });
    let sought = collect_progress(&mut supervisor, command, SLOW, |seen| {
        seen.iter().any(|p| (p.position - 1.5).abs() < 0.5)
    });
    assert!(
        sought.iter().any(|p| (p.position - 1.5).abs() < 0.5),
        "a seek to 1.5s did not move the playhead: {sought:?}"
    );
}

/// Drive the supervisor until the reports so far satisfy `done`, or until the
/// deadline. Returns everything seen, so a failing assertion can show it.
fn collect_progress(
    supervisor: &mut MediaSupervisor,
    command: WorkerCommand,
    deadline: Duration,
    done: impl Fn(&[PlaybackProgress]) -> bool,
) -> Vec<PlaybackProgress> {
    let started = Instant::now();
    let mut reports = Vec::new();
    while !done(&reports) && started.elapsed() < deadline {
        for event in supervisor.poll(|_| command.clone()) {
            if let SessionEvent::Progress { progress, .. } = event {
                reports.push(progress);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    reports
}

/// A press and release in the middle of the overlay.
fn click(supervisor: &mut MediaSupervisor, session: SessionId, command: WorkerCommand) {
    use pulpit_media::protocol::PointerButton;
    for event in [
        InputEvent::PointerMoved { x: 80.0, y: 45.0 },
        InputEvent::PointerPressed {
            x: 80.0,
            y: 45.0,
            button: PointerButton::Left,
            click_count: 1,
        },
        InputEvent::PointerReleased {
            x: 80.0,
            y: 45.0,
            button: PointerButton::Left,
            click_count: 1,
        },
    ] {
        supervisor.input(session, event);
    }
    // Let the worker forward them and the page react.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(400) {
        supervisor.poll(|_| command.clone());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Deactivating an overlay must stop its screencast at the browser — no new
/// frames at all — and reactivating must resume it on the same session with
/// sequences that keep superseding the frames from before the pause.
#[test]
fn deactivating_an_overlay_stops_its_frames_and_reactivating_resumes_them() {
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let mut supervisor = MediaSupervisor::new(MediaConfig::default());
    let probe = supervisor
        .probe(RuntimeId::ExternalChromium)
        .cloned()
        .expect("the browser runtime should have been probed");
    if !probe.is_available() {
        eprintln!("skipping: {}", probe.availability.detail());
        return;
    }

    let staging = tempfile::tempdir().unwrap();
    let root = staging.path().join("bundle");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("index.html"),
        r#"<!doctype html><html><body style="margin:0;background:#000">
           <canvas id=c width=64 height=64></canvas><script>
           const x=document.getElementById('c').getContext('2d');let i=0;
           function f(){i=(i+8)%256;x.fillStyle=`rgb(${i},${255-i},128)`;
           x.fillRect(0,0,64,64);requestAnimationFrame(f);} f();
           </script></body></html>"#,
    )
    .unwrap();

    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=chromium".to_string()],
    };
    let session = supervisor.open(
        OverlayId(11),
        RenderGeneration(1),
        ContentKind::Web,
        SessionSource::Bundle {
            root: root.display().to_string(),
            entrypoint: "index.html".to_string(),
        },
        Viewport::new(64, 64, 1.0),
        PlaybackParams::default(),
        &CapabilityRequest::for_kind(ContentKind::Web),
        |_| command.clone(),
    );

    // Frames flow while active.
    let (frames, problems) = collect_frames(&mut supervisor, command.clone(), 3, SLOW);
    assert!(problems.is_empty(), "the session failed: {problems:?}");
    assert!(frames.len() >= 3, "no animation before deactivation");

    // Deactivate, drain what was in flight, then require silence.
    supervisor.set_active(session, false);
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_millis(800) {
        supervisor.poll(|_| command.clone());
        std::thread::sleep(Duration::from_millis(10));
    }
    let quiet = Instant::now();
    let mut while_parked = 0usize;
    while quiet.elapsed() < Duration::from_secs(2) {
        for event in supervisor.poll(|_| command.clone()) {
            if let SessionEvent::Frame { .. } = event {
                while_parked += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(while_parked, 0, "an inactive overlay kept producing frames");

    // Reactivate: the same session must produce again.
    supervisor.set_active(session, true);
    let (resumed, problems) = collect_frames(&mut supervisor, command, 2, Duration::from_secs(15));
    assert!(
        problems.is_empty(),
        "the session failed on resume: {problems:?}"
    );
    assert!(
        resumed.len() >= 2,
        "the overlay did not resume after reactivation"
    );
}

/// The libmpv runtime plays a video natively — required, not merely
/// preferred, so a silent fallback to the browser cannot fake a pass.
fn libmpv_plays(kind: ContentKind, asset: &str, label: &str) {
    let _serial = browser_lock();
    let Some(binary) = app_binary() else { return };
    let Some(source) = media_asset(asset) else {
        return;
    };
    let policy = pulpit_media::RuntimePolicy::Require(RuntimeId::LibMpv);
    let mut supervisor = MediaSupervisor::new(MediaConfig {
        video_runtime: policy,
        image_runtime: policy,
        ..MediaConfig::default()
    });
    if !supervisor
        .probe(RuntimeId::LibMpv)
        .is_some_and(|probe| probe.is_available())
    {
        eprintln!("skipping: no libmpv installed");
        return;
    }

    let command = WorkerCommand::Explicit {
        program: binary,
        args: vec!["--media-worker=libmpv".to_string()],
    };
    supervisor.open(
        OverlayId(12),
        RenderGeneration(1),
        kind,
        SessionSource::File {
            path: source.display().to_string(),
        },
        Viewport::new(160, 90, 1.0),
        PlaybackParams {
            autoplay: true,
            repeat: true,
            mute: true,
            ..Default::default()
        },
        &CapabilityRequest::for_kind(kind),
        |_| command.clone(),
    );

    let (frames, problems) = collect_frames(&mut supervisor, command, 4, Duration::from_secs(30));
    assert!(problems.is_empty(), "{label}: {problems:?}");
    assert!(frames.len() >= 4, "{label}: only {} frames", frames.len());
    assert!(
        frames.iter().any(|frame| frame != &frames[0]),
        "{label}: every frame was identical"
    );
}

#[test]
fn libmpv_plays_a_video_natively() {
    libmpv_plays(ContentKind::Video, "clip.mp4", "mpv video");
}

#[test]
fn libmpv_plays_an_animated_gif_natively() {
    libmpv_plays(ContentKind::AnimatedImage, "bouncing.gif", "mpv gif");
}
