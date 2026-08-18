//! The supervisor side of the document worker: one child process, one open
//! document, one conversation.
//!
//! Deliberately much simpler than [`crate::supervisor`], and for a reason that
//! is worth stating rather than discovering. That supervisor multiplexes many
//! render jobs across a pool of interchangeable workers, cancels obsolete work
//! and replaces a worker that stops answering, because a dropped frame is
//! recoverable — the last complete frame stays on the projector.
//!
//! A document session is the opposite case. There is exactly one document,
//! requests are ordered and mostly synchronous, and a mutation that is
//! silently retried is a duplicated edit. So this is a request-and-answer
//! channel with no pool, no queue and no automatic replay: after a crash the
//! supervisor reopens the last durable base and replays only *confirmed*
//! entries from the journal (§11.5), which is a decision this type reports
//! rather than makes.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::protocol::{read_message, write_message, ProtocolError};
use crate::supervisor::WORKER_MARKER;

use super::protocol::{DocumentFailure, DocumentRequest, DocumentResponse};
use super::worker::Hello;

/// How the document worker is started.
///
/// A *role* of the running executable, re-executed with a flag (§5.1) — the
/// same arrangement the render worker uses, and the reason there is no second
/// binary to ship, sign or find on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentWorkerCommand {
    /// The running executable, re-executed with `flag=PATH`.
    CurrentExe { flag: String },
    /// A named program, for a harness standing in for the real worker.
    Explicit { program: PathBuf, args: Vec<String> },
}

impl Default for DocumentWorkerCommand {
    fn default() -> Self {
        DocumentWorkerCommand::CurrentExe {
            flag: "--document-worker".into(),
        }
    }
}

impl DocumentWorkerCommand {
    fn build(&self, source: &Path) -> std::io::Result<Command> {
        // The same fork-bomb bound the render supervisor keeps, for the same
        // reason: a worker that spawns workers grows exponentially and takes
        // the machine down before any deadline can notice.
        if std::env::var_os(WORKER_MARKER).is_some() {
            return Err(std::io::Error::other(
                "refusing to spawn a document worker from inside a worker process",
            ));
        }
        let mut command = match self {
            DocumentWorkerCommand::CurrentExe { flag } => {
                use std::ffi::OsString;

                let mut command = Command::new(std::env::current_exe()?);
                // The document is named on the command line because a worker
                // serves exactly one and is started for it: there is no state
                // in which it is running with nothing open.
                // Build the argument as OsString to preserve non-UTF-8 bytes.
                let mut arg = OsString::from(flag);
                arg.push("=");
                arg.push(source.as_os_str());
                command.arg(arg);
                command
            }
            DocumentWorkerCommand::Explicit { program, args } => {
                let mut command = Command::new(program);
                command.args(args);
                command
            }
        };
        command
            .env(WORKER_MARKER, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        Ok(command)
    }
}

/// Why a document session could not do what was asked.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("cannot start the document worker: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("the document worker speaks protocol {theirs}, we speak {ours}")]
    VersionMismatch { ours: u32, theirs: u32 },
    #[error("the document worker stopped answering")]
    WorkerGone,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("{0}")]
    Refused(DocumentFailure),
}

impl SessionError {
    /// Did the *worker* die, as opposed to answering with a refusal?
    ///
    /// The distinction is what §11.5 turns on: a read-only request may be
    /// retried against a fresh worker, and a mutation may not be assumed
    /// committed without a response.
    pub fn is_worker_loss(&self) -> bool {
        matches!(
            self,
            SessionError::WorkerGone | SessionError::Spawn(_) | SessionError::Protocol(_)
        )
    }
}

/// A running document worker, and the pipe to it.
pub struct DocumentSession {
    child: Child,
    to_worker: BufWriter<std::process::ChildStdin>,
    from_worker: BufReader<std::process::ChildStdout>,
    source: PathBuf,
    /// Set once a request has failed in a way that means the worker is gone.
    /// A session does not resurrect itself: whoever owns the journal decides
    /// what may be replayed (§11.5).
    lost: bool,
}

impl std::fmt::Debug for DocumentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentSession")
            .field("source", &self.source)
            .field("pid", &self.child.id())
            .field("lost", &self.lost)
            .finish()
    }
}

impl DocumentSession {
    /// Start a worker and check that it speaks the same protocol.
    pub fn start(
        command: &DocumentWorkerCommand,
        source: &Path,
    ) -> Result<DocumentSession, SessionError> {
        let mut child = command.build(source)?.spawn()?;
        let to_worker = BufWriter::new(child.stdin.take().ok_or(SessionError::WorkerGone)?);
        let mut from_worker = BufReader::new(child.stdout.take().ok_or(SessionError::WorkerGone)?);

        // The handshake is first and is not optional: a worker built against a
        // different document protocol is shut down rather than trusted (§5.2).
        let hello: Hello = read_message(&mut from_worker)?;
        if !hello.is_compatible() {
            let _ = child.kill();
            return Err(SessionError::VersionMismatch {
                ours: super::protocol::DOCUMENT_PROTOCOL_VERSION,
                theirs: hello.document_protocol_version,
            });
        }

        Ok(DocumentSession {
            child,
            to_worker,
            from_worker,
            source: source.to_path_buf(),
            lost: false,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Has this session lost its worker?
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Send one request and wait for its answer.
    ///
    /// Refusals come back as [`SessionError::Refused`] rather than as an
    /// answer, so a caller cannot accidentally treat "the document said no"
    /// as "the document did it".
    pub fn request(&mut self, request: DocumentRequest) -> Result<DocumentResponse, SessionError> {
        if self.lost {
            return Err(SessionError::WorkerGone);
        }
        // Bounds on the sending side as well as the receiving one (§9.5).
        if let Err(limit) = request.validate() {
            return Err(SessionError::Refused(DocumentFailure::Refused(
                limit.to_string(),
            )));
        }

        if let Err(error) = write_message(&mut self.to_worker, &request) {
            self.lost = true;
            return Err(error.into());
        }
        let response: DocumentResponse = match read_message(&mut self.from_worker) {
            Ok(response) => response,
            Err(error) => {
                self.lost = true;
                return Err(error.into());
            }
        };
        if let Err(limit) = response.validate() {
            return Err(SessionError::Refused(DocumentFailure::Refused(
                limit.to_string(),
            )));
        }
        match response {
            DocumentResponse::Failed(failure) => Err(SessionError::Refused(failure)),
            other => Ok(other),
        }
    }

    /// Ask the worker to close, then wait for it.
    ///
    /// Best effort: a worker that has already died is closed, and saying so
    /// twice helps nobody.
    pub fn close(mut self) {
        if !self.lost {
            let _ = write_message(&mut self.to_worker, &DocumentRequest::Close);
            let _ = read_message::<DocumentResponse>(&mut self.from_worker);
        }
        let _ = self.child.wait();
    }
}

impl Drop for DocumentSession {
    fn drop(&mut self) {
        // A dropped session must not leave a process holding a document open.
        // The worker exits when its input closes, so dropping the pipe is the
        // ordinary path; the kill is for a worker that is wedged.
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Serve the document-worker role, for the binary's `--document-worker` arm.
///
/// Split out here so the executable is three lines: choosing the engine and
/// opening the document is the only part that knows about PDFium, and it is
/// the part a build without PDFium does not have.
pub fn serve_stdio(
    worker: super::worker::DocumentWorker<'_>,
    input: impl Read,
    output: impl Write,
) -> Result<(), ProtocolError> {
    super::worker::serve(input, output, worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_cannot_spawn_a_worker() {
        // The fork-bomb bound, which holds for an explicit command too: a
        // command that happens to name the spawning binary recurses exactly
        // the same way.
        let guard = MarkerGuard::set();
        for command in [
            DocumentWorkerCommand::default(),
            DocumentWorkerCommand::Explicit {
                program: "/bin/true".into(),
                args: Vec::new(),
            },
        ] {
            let error = command
                .build(Path::new("/tmp/x.pdf"))
                .expect_err("a worker must not spawn one");
            assert!(error.to_string().contains("refusing"));
        }
        drop(guard);
    }

    #[test]
    fn the_default_command_is_a_role_of_this_executable() {
        // §5.1: no new binary. If this ever becomes an `Explicit` path, the
        // package has gained something to ship and sign.
        assert!(matches!(
            DocumentWorkerCommand::default(),
            DocumentWorkerCommand::CurrentExe { .. }
        ));
    }

    #[test]
    fn a_lost_worker_is_told_apart_from_a_document_that_said_no() {
        assert!(SessionError::WorkerGone.is_worker_loss());
        assert!(SessionError::Protocol(ProtocolError::Closed).is_worker_loss());
        assert!(!SessionError::Refused(DocumentFailure::Refused("no".into())).is_worker_loss());
        assert!(!SessionError::VersionMismatch { ours: 1, theirs: 2 }.is_worker_loss());
    }

    /// Sets the worker marker for the duration of a test and puts the
    /// environment back afterwards, because it is process-global.
    struct MarkerGuard(Option<std::ffi::OsString>);

    impl MarkerGuard {
        fn set() -> MarkerGuard {
            let previous = std::env::var_os(WORKER_MARKER);
            std::env::set_var(WORKER_MARKER, "1");
            MarkerGuard(previous)
        }
    }

    impl Drop for MarkerGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var(WORKER_MARKER, value),
                None => std::env::remove_var(WORKER_MARKER),
            }
        }
    }
}
