//! The worker roles this executable can take.
//!
//! The browser adapter only ever *launches* an installed browser rather than
//! linking one, so it rides inside the application's own executable and is
//! re-executed with a flag, exactly as the renderer worker is. That is what
//! makes it impossible to be missing: there is no second binary to forget to
//! build, install or package.
//!
//! A runtime that linked an optional system library would have to stay a
//! separate executable, so that one absent shared library could not stop the
//! application from starting. None currently does.

#[cfg(feature = "chromium-runtime")]
pub mod chromium;
pub mod mpv;

use crate::protocol::RuntimeId;

/// The flag value that selects each in-process worker role.
///
/// Kept beside the roles themselves so the spawning side and the dispatching
/// side cannot drift apart.
pub fn role_flag(runtime: RuntimeId) -> Option<&'static str> {
    match runtime {
        RuntimeId::ExternalChromium => Some("chromium"),
        RuntimeId::LibMpv => Some("libmpv"),
        // Everything else is a separate executable by design.
        _ => None,
    }
}

/// Run the worker role named by `flag`, if this build has it.
///
/// Returns false when the name is not a role, so the caller can report an
/// unusable argument rather than silently starting the wrong thing.
pub fn run_role(flag: &str) -> bool {
    match flag {
        #[cfg(feature = "chromium-runtime")]
        "chromium" => {
            chromium::run();
            true
        }
        "libmpv" => {
            mpv::run();
            true
        }
        _ => false,
    }
}

/// The replies every worker owes its parent, in the one form they all take.
///
/// A session that could not be opened, one that is ready, one that is closed
/// and a greeting are not runtime-specific events: they say the same thing
/// whichever runtime is behind them, and the parent reads them with one
/// decoder. Writing them here rather than at each runtime keeps them
/// byte-for-byte the same and keeps the ignored-write policy — a parent that
/// has gone away is not this worker's error to report — in one place.
///
/// Only these four live here. Everything that carries a runtime's own
/// meaning, and the event loops themselves, stay where they are; the two
/// workers keep clocks and wake for different reasons, and merging them would
/// hide that.
pub(crate) mod reply {
    use super::*;
    use crate::protocol::MEDIA_PROTOCOL_VERSION;
    use crate::protocol::{write_message, MediaError, MediaEvent, SessionId, WorkerDescription};
    use std::io::Write;

    /// The greeting, with this runtime's identity and feature list.
    ///
    /// The version the parent announced is logged when it differs from ours
    /// rather than refused: the parent decides what to do about a mismatch,
    /// and a worker that hung up here would be harder to diagnose than one
    /// that answered.
    pub fn hello<W: Write>(out: &mut W, theirs: u32, runtime: RuntimeId, features: Vec<String>) {
        if theirs != MEDIA_PROTOCOL_VERSION {
            tracing::warn!(theirs, ours = MEDIA_PROTOCOL_VERSION, "version mismatch");
        }
        let _ = write_message(
            out,
            &MediaEvent::Hello(WorkerDescription {
                version: MEDIA_PROTOCOL_VERSION,
                runtime,
                features,
            }),
        );
    }

    /// A session is open and will start producing frames.
    pub fn ready<W: Write>(out: &mut W, session: SessionId) {
        let _ = write_message(out, &MediaEvent::Ready { session });
    }

    /// A session is gone, whether or not it was ever open.
    pub fn closed<W: Write>(out: &mut W, session: SessionId) {
        let _ = write_message(out, &MediaEvent::Closed { session });
    }

    /// Something failed, named against the session it happened to.
    pub fn failed<W: Write>(out: &mut W, session: Option<SessionId>, error: MediaError) {
        let _ = write_message(out, &MediaEvent::Failed { session, error });
    }
}

/// Wait until stdin has a message or `timeout` elapses.
///
/// Returns true when there is something to read. Polling this way is what
/// lets one thread both keep a clock and stay responsive to commands.
///
/// The reader is consulted first and this is not optional: `read_message`
/// fills the buffer from the pipe, so a second message can already be *in*
/// the reader while `poll` sees an empty descriptor. Polling alone therefore
/// waits for ever on a message that has already arrived — which is exactly
/// waits for ever on a message that has already arrived.
#[cfg(unix)]
pub fn wait_for_input<R: std::io::Read>(
    reader: &mut std::io::BufReader<R>,
    timeout: std::time::Duration,
) -> bool {
    use std::os::fd::AsRawFd;

    if !reader.buffer().is_empty() {
        return true;
    }
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    extern "C" {
        fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
    }
    let mut descriptor = PollFd {
        fd: std::io::stdin().as_raw_fd(),
        events: 0x001,
        revents: 0,
    };
    // SAFETY: one initialised pollfd is passed with a length of one.
    unsafe {
        poll(
            &mut descriptor,
            1,
            timeout.as_millis().min(i32::MAX as u128) as i32,
        ) > 0
    }
}

#[cfg(not(unix))]
pub fn wait_for_input<R: std::io::Read>(
    _reader: &mut std::io::BufReader<R>,
    timeout: std::time::Duration,
) -> bool {
    std::thread::sleep(timeout);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn a_message_already_in_the_buffer_counts_as_input_waiting() {
        // The failure this pins is not hypothetical: the browser worker used
        // a bare poll of the descriptor, so when the application sent a burst
        // — set-active, viewport and input together, as a slide change does —
        // the first read drained the pipe into the buffered reader and every
        // later message was stranded there. The overlay then ignored input
        // until something else happened to arrive.
        use std::io::{BufReader, Read};

        // A reader holding bytes that the descriptor no longer has.
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"two messages worth of bytes").unwrap();
        let mut reader = BufReader::new(std::fs::File::open(file.path()).unwrap());
        let mut one = [0u8; 1];
        reader.read_exact(&mut one).unwrap();
        assert!(
            !reader.buffer().is_empty(),
            "the reader should hold the rest of the file"
        );

        // `wait_for_input` takes the reader, so it can see what poll cannot.
        // The descriptor here is stdin, which has nothing waiting; the answer
        // must still be yes, on the strength of the buffer alone.
        assert!(
            wait_for_input(&mut reader, std::time::Duration::from_millis(1)),
            "buffered bytes were not counted as input waiting"
        );
    }

    #[test]
    fn the_in_process_roles_are_exactly_the_ones_that_link_nothing_optional() {
        assert_eq!(role_flag(RuntimeId::ExternalChromium), Some("chromium"));
        // The system webviews would link optional system libraries, so they
        // must stay out of the application's own executable.
        for runtime in [
            RuntimeId::WebKitGtk,
            RuntimeId::WebView2,
            RuntimeId::WkWebView,
        ] {
            assert_eq!(role_flag(runtime), None, "{runtime} must stay separate");
        }
    }

    #[test]
    fn every_role_flag_is_one_run_role_understands() {
        for runtime in RuntimeId::ALL {
            let Some(flag) = role_flag(runtime) else {
                continue;
            };
            assert_ne!(flag, "", "{runtime} has an empty flag");
        }
        assert!(!run_role("nonesuch"), "an unknown role must be refused");
    }
}
