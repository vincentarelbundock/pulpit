//! §76.8: a session the supervisor gives up on must not be left running on
//! its worker.
//!
//! `MediaSupervisor::recover` used to remove a failed session from its own
//! table and either relaunch or fall back, but never told the worker
//! process that still hosted it. For the chromium worker that meant a page
//! left screencasting into a ring nobody drains any more; here, with a real
//! scripted worker standing in for it, the observable fact is simpler and
//! just as decisive: does `Close` ever reach the worker's stdin. Driving an
//! in-memory fake could not answer that, because the bug was specifically
//! about what does and does not go down the pipe.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use pulpit_core::overlay::ContentKind;
use pulpit_core::{OverlayId, RenderGeneration};
use pulpit_media::protocol::CapabilityRequest;
use pulpit_media::{
    MediaConfig, MediaSupervisor, RuntimeId, SessionEvent, SessionSource, Viewport, WorkerCommand,
};

fn scripted_worker() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pulpit-media-scripted-worker"))
}

fn available(id: RuntimeId, kinds: &[ContentKind]) -> pulpit_media::RuntimeProbe {
    use pulpit_media::capability::{Availability, ContentCapabilities, InputCapabilities};
    pulpit_media::RuntimeProbe {
        content: ContentCapabilities {
            kinds: kinds.to_vec(),
            continuous_frames: true,
            ..Default::default()
        },
        input: InputCapabilities {
            pointer: true,
            ..Default::default()
        },
        ..pulpit_media::RuntimeProbe::unavailable(id, Availability::Available)
    }
}

#[test]
fn a_scripted_worker_that_answers_failed_receives_close() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    let marker_path = marker.path().to_path_buf();

    let mut supervisor = MediaSupervisor::unprobed(MediaConfig {
        // No retries: the one `Failed` the scripted worker sends must go
        // straight to exhaustion, so exactly one `recover` call happens and
        // the test is not chasing a moving target.
        max_restarts: 0,
        ..MediaConfig::default()
    });
    supervisor.record_probe(available(
        RuntimeId::ExternalChromium,
        &[ContentKind::Video],
    ));

    let command = WorkerCommand::Explicit {
        program: scripted_worker(),
        args: vec![marker_path.display().to_string()],
    };

    supervisor.open(
        OverlayId(1),
        RenderGeneration(1),
        ContentKind::Video,
        SessionSource::File {
            path: "/staged/clip.mp4".into(),
        },
        Viewport::new(64, 64, 1.0),
        Default::default(),
        &CapabilityRequest::for_kind(ContentKind::Video),
        |_| command.clone(),
    );

    // Drive the supervisor until it reports the session exhausted: `Ready`,
    // then the scripted `Failed`, then whatever `recover` does about it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exhausted = false;
    while Instant::now() < deadline && !exhausted {
        for event in supervisor.poll(|_| command.clone()) {
            if let SessionEvent::Failed {
                exhausted: true, ..
            } = event
            {
                exhausted = true;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(exhausted, "the session was never reported exhausted");

    // The marker file only gets a line when the scripted worker actually
    // receives `Close`. Give the worker's already-buffered write a moment
    // to land on disk after the supervisor's own view settled.
    let mut closed = String::new();
    let read_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < read_deadline && closed.is_empty() {
        closed = std::fs::read_to_string(&marker_path).unwrap_or_default();
        if closed.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert_eq!(
        closed.trim(),
        "1",
        "the worker never received Close for the session it kept answering Failed for"
    );

    supervisor.shutdown();
}
