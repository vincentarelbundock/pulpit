//! Launching a worker process, and the one bound that stops it launching more.
//!
//! `pulpit`, the renderer worker and the media worker are three roles of one
//! binary, re-executed with a flag. That is what makes the marker below
//! load-bearing rather than a formality: the thing a worker would re-execute
//! is itself.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Set on every worker process a supervisor spawns, and refused as input.
///
/// A worker that spawns workers is a fork bomb: the growth is exponential and
/// takes the machine down long before a deadline or a restart budget can
/// notice, because the failure is unbounded breadth rather than one runaway
/// child. The marker is the only bound that holds, and it holds for an
/// explicitly named program too — a command that happens to name the spawning
/// binary recurses exactly the same way.
///
/// This used to be declared once per supervisor, with each copy's comment
/// noting that the two had to stay equal and nothing checking that they did.
/// One constant is the check.
pub const WORKER_MARKER: &str = "PULPIT_WORKER";

/// Are we already inside a worker process?
///
/// Every spawn site must ask before spawning. `spawn_guard` is the reason to
/// prefer [`WorkerCommand::build`], which asks on the caller's behalf.
pub fn inside_a_worker() -> bool {
    std::env::var_os(WORKER_MARKER).is_some()
}

/// Refuse to spawn from inside a worker, naming the role for the log.
pub fn spawn_guard(role: &str) -> std::io::Result<()> {
    if inside_a_worker() {
        return Err(std::io::Error::other(format!(
            "refusing to spawn a {role} from inside a worker process"
        )));
    }
    Ok(())
}

/// How a worker process is launched.
#[derive(Debug, Clone)]
pub enum WorkerCommand {
    /// Re-execute the current binary with an argument that makes it a worker.
    /// Used by the application so a single installed executable is enough.
    CurrentExe { arg: String },
    /// A named program, for a harness standing in for the real worker.
    Explicit { program: PathBuf, args: Vec<String> },
}

impl WorkerCommand {
    /// Build the command, refusing if this process is itself a worker.
    ///
    /// `role` appears in the refusal, so a log says which supervisor tried.
    pub fn build(&self, role: &str) -> std::io::Result<Command> {
        spawn_guard(role)?;
        let command = match self {
            WorkerCommand::CurrentExe { arg } => {
                let mut command = Command::new(std::env::current_exe()?);
                command.arg(arg);
                command
            }
            WorkerCommand::Explicit { program, args } => {
                let mut command = Command::new(program);
                command.args(args);
                command
            }
        };
        Ok(as_worker(command))
    }
}

/// Mark a command as a worker and give it the pipes a supervisor talks over.
///
/// Separate from [`WorkerCommand::build`] because a spawn site may assemble
/// its own command — the document worker names the file on the command line,
/// so it cannot use `CurrentExe` — and must still end up marked. Anything that
/// starts a pulpit role goes through here.
pub fn as_worker(mut command: Command) -> Command {
    command
        .env(WORKER_MARKER, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment is process-wide, so the marker tests take a lock rather
    /// than racing each other through it.
    static ENVIRONMENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_worker_may_not_spawn_a_worker() {
        let _guard = ENVIRONMENT.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: single-threaded within this test, and the lock keeps the
        // other environment-reading tests out for its duration.
        unsafe { std::env::set_var(WORKER_MARKER, "1") };
        let refused = WorkerCommand::CurrentExe {
            arg: "--render-worker".into(),
        }
        .build("renderer worker");
        unsafe { std::env::remove_var(WORKER_MARKER) };

        let error = refused.expect_err("a worker must refuse to spawn a worker");
        assert!(
            error.to_string().contains("renderer worker"),
            "the refusal names the role that asked: {error}"
        );
    }

    #[test]
    fn an_explicit_program_is_bound_by_the_marker_too() {
        // The bound is not about which binary is named: a command that happens
        // to name the spawning binary recurses exactly the same way.
        let _guard = ENVIRONMENT.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var(WORKER_MARKER, "1") };
        let refused = WorkerCommand::Explicit {
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        }
        .build("media worker");
        unsafe { std::env::remove_var(WORKER_MARKER) };

        assert!(refused.is_err(), "an explicit program is bound as well");
    }

    #[test]
    fn a_built_command_carries_the_marker() {
        let _guard = ENVIRONMENT.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var(WORKER_MARKER) };
        let command = WorkerCommand::Explicit {
            program: PathBuf::from("/bin/true"),
            args: vec!["--flag".into()],
        }
        .build("test worker")
        .expect("a plain process may spawn");

        let marked = command
            .get_envs()
            .any(|(key, value)| key == WORKER_MARKER && value == Some("1".as_ref()));
        assert!(marked, "the child is marked, or it could spawn its own");
    }
}
