//! A scripted stand-in for a media worker, built only to drive the
//! supervisor's recovery contract (§76.8) with a real child process instead
//! of a fake in memory: the bug it regresses is specifically that
//! `MediaSupervisor::recover` gave up on a session without ever telling the
//! worker that still hosted it, which is a claim about what goes down the
//! pipe and can only be checked by actually running one.
//!
//! Protocol: greets normally; every `Open` gets a `Ready` immediately
//! followed by a `Failed` naming the same session, mirroring what a real
//! worker sends when `SetActive`/`Input` fails on a live page
//! (`worker/chromium.rs`); a `Close` is recorded — the session id is
//! appended to the marker file named on the command line — before the
//! worker replies `Closed`.
//!
//! Usage: `scripted-worker <marker-file>`

use std::io::{BufReader, Write};

use pulpit_media::protocol::{
    read_message, write_message, MediaError, MediaErrorKind, MediaEvent, MediaRequest, RuntimeId,
    WorkerDescription, MEDIA_PROTOCOL_VERSION,
};

fn main() {
    let marker = std::env::args()
        .nth(1)
        .expect("scripted-worker requires a marker file path");
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    while let Ok(request) = read_message::<MediaRequest>(&mut reader) {
        match request {
            MediaRequest::Hello { .. } => {
                let _ = write_message(
                    &mut out,
                    &MediaEvent::Hello(WorkerDescription {
                        version: MEDIA_PROTOCOL_VERSION,
                        runtime: RuntimeId::ExternalChromium,
                        features: Vec::new(),
                    }),
                );
                let _ = out.flush();
            }
            MediaRequest::Open(spec) => {
                let session = spec.session;
                let _ = write_message(&mut out, &MediaEvent::Ready { session });
                let _ = write_message(
                    &mut out,
                    &MediaEvent::Failed {
                        session: Some(session),
                        error: MediaError::new(MediaErrorKind::Crashed, "scripted failure"),
                    },
                );
                let _ = out.flush();
            }
            MediaRequest::Close { session } => {
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&marker)
                {
                    let _ = writeln!(file, "{}", session.0);
                }
                let _ = write_message(&mut out, &MediaEvent::Closed { session });
                let _ = out.flush();
            }
            MediaRequest::Shutdown => break,
            _ => {}
        }
    }
}
