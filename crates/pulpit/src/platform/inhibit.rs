//! Sleep and idle inhibition, as a value.
//!
//! The *state* lives here so the application can display it; the mechanism
//! lives in the platform adapter. Every path must be releasable, and a
//! process-based mechanism is preferred where the platform has one, because
//! the kernel reaps it if pulpit dies mid-talk.
//!
//! [`hold_with_child`] and [`release_child`] are the process-token half of
//! that mechanism, shared by the Linux (`systemd-inhibit`) and macOS
//! (`caffeinate`) adapters rather than written twice: spawn a long-lived
//! child as the inhibition, and end it with `SIGTERM` via `libc::kill`
//! rather than shelling out to the `kill` command, reaping it afterwards so
//! a released inhibition does not leave a zombie behind for however much
//! longer pulpit keeps running.

#[cfg(unix)]
use std::process::Command;

/// Which mechanism is holding the inhibition, if any.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InhibitState {
    #[default]
    Released,
    Held {
        mechanism: &'static str,
        /// Opaque handle the adapter needs in order to release it.
        token: InhibitToken,
    },
    /// No mechanism is available. Reported, never hidden.
    Unavailable {
        reason: String,
        /// Everything that was tried, for diagnostics.
        attempts: Vec<String>,
    },
}

/// What an adapter needs to hand back to release an inhibition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InhibitToken {
    /// A D-Bus object path or similar.
    #[cfg_attr(target_os = "macos", allow(dead_code))] // built on Linux and Windows
    Handle(String),
    /// A numeric cookie.
    #[cfg_attr(not(unix), allow(dead_code))] // constructed only by the Unix adapters
    Cookie(u32),
    /// A child process id.
    #[cfg_attr(not(unix), allow(dead_code))] // constructed only by the Unix adapters
    Process(u32),
    #[allow(dead_code)] // reached by its tests, not by the application
    None,
}

impl InhibitState {
    pub fn is_held(&self) -> bool {
        matches!(self, InhibitState::Held { .. })
    }

    #[allow(dead_code)] // reached by its tests, not by the application
    pub fn is_unavailable(&self) -> bool {
        matches!(self, InhibitState::Unavailable { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            InhibitState::Released => "screensaver inhibition released".into(),
            InhibitState::Held { mechanism, .. } => {
                format!("screensaver inhibited via {mechanism}")
            }
            InhibitState::Unavailable { reason, .. } => {
                format!("screensaver inhibition unavailable: {reason}")
            }
        }
    }

    /// Everything that was tried and failed.
    pub fn attempts(&self) -> &[String] {
        match self {
            InhibitState::Unavailable { attempts, .. } => attempts,
            _ => &[],
        }
    }
}

/// A small owner that keeps acquire/release balanced.
///
/// Acquiring twice holds one inhibition; releasing when nothing is held is
/// safe. There is deliberately no `Drop` impl: releasing needs a
/// `&dyn PlatformServices` that a destructor has no way to be handed, so a
/// caller that wants the inhibition released MUST call
/// [`Inhibitor::release`] itself before the value goes out of scope.
#[derive(Debug)]
pub struct Inhibitor {
    state: InhibitState,
}

impl Default for Inhibitor {
    fn default() -> Self {
        Inhibitor {
            state: InhibitState::Released,
        }
    }
}

impl Inhibitor {
    pub fn new() -> Inhibitor {
        Inhibitor::default()
    }

    pub fn state(&self) -> &InhibitState {
        &self.state
    }

    /// Follow the desired state, asking `services` only when it changes.
    pub fn set_desired(
        &mut self,
        wanted: bool,
        services: &dyn crate::platform::services::PlatformServices,
    ) -> &InhibitState {
        match (wanted, self.state.is_held()) {
            (true, false) => self.state = services.inhibit(),
            (false, true) => {
                record_release_outcome(services.release_inhibit(&self.state));
                self.state = InhibitState::Released;
            }
            _ => {}
        }
        &self.state
    }

    pub fn release(&mut self, services: &dyn crate::platform::services::PlatformServices) {
        if self.state.is_held() {
            record_release_outcome(services.release_inhibit(&self.state));
        }
        self.state = InhibitState::Released;
    }
}

/// Spawn `command` as a long-lived child that is itself the inhibition:
/// `systemd-inhibit sleep infinity` on Linux, `caffeinate -d -i` on macOS.
/// The kernel reaps it if this process dies mid-talk, which is the point of
/// preferring a process over any other mechanism where one exists.
///
/// `command`'s stdio is redirected to null; the caller supplies the program
/// and arguments only. `attempts` is what a caller trying other mechanisms
/// first has already collected, and is carried into the `Unavailable` case
/// rather than discarded if this one fails too.
#[cfg(unix)]
pub fn hold_with_child(
    mut command: Command,
    mechanism: &'static str,
    mut attempts: Vec<String>,
) -> InhibitState {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match command.spawn() {
        Ok(child) => InhibitState::Held {
            mechanism,
            token: InhibitToken::Process(child.id()),
        },
        Err(e) => {
            attempts.push(format!("{mechanism}: {e}"));
            InhibitState::Unavailable {
                reason: "no inhibition mechanism answered".into(),
                attempts,
            }
        }
    }
}

/// End the child behind an [`InhibitToken::Process`] with `SIGTERM`, then
/// reap it.
///
/// `libc::kill` rather than shelling out to the `kill` command: one syscall
/// instead of a second process just to ask the kernel to end the first one.
/// The reap matters even though the shell-out this replaces never did it —
/// a `Child` this process spawned and does not `wait` on stays a zombie
/// until this process exits, and pulpit can run for hours after a talk ends.
#[cfg(unix)]
pub fn release_child(pid: u32) -> crate::platform::Outcome {
    use crate::platform::Outcome;

    // SAFETY: `pid` came from `Child::id()` of a child this process spawned
    // in `hold_with_child` and has not yet reaped; sending it a signal is
    // sound for any pid, and reaping below is sound because this process is
    // that child's parent.
    let killed = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if killed != 0 {
        return Outcome::failed(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: `pid` names a child of this process; blocking here is bounded
    // by the `SIGTERM` just sent, which this child never handles.
    let reaped = unsafe {
        let mut status = 0i32;
        libc::waitpid(pid as libc::pid_t, &mut status, 0)
    };
    if reaped == -1 {
        return Outcome::failed(std::io::Error::last_os_error().to_string());
    }
    Outcome::Done
}

/// A release that did not simply succeed is worth knowing about — the sleep
/// inhibitor staying held is the kind of thing a presenter only notices when
/// the screen blanks mid-talk — so it goes to diagnostics rather than being
/// silently discarded.
fn record_release_outcome(outcome: crate::platform::Outcome) {
    if let Some(reason) = outcome.describe() {
        tracing::warn!(reason = %reason, "releasing the sleep inhibitor did not simply succeed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::null::NullPlatform;

    #[test]
    fn an_unavailable_inhibition_explains_itself_and_lists_attempts() {
        let state = InhibitState::Unavailable {
            reason: "no mechanism answered".into(),
            attempts: vec!["portal: not running".into()],
        };
        assert!(state.is_unavailable());
        assert!(state.describe().contains("unavailable"));
        assert_eq!(state.attempts().len(), 1);
    }

    #[test]
    fn acquiring_twice_holds_one_inhibition() {
        let services = NullPlatform::new("test").holding_inhibition();
        let mut inhibitor = Inhibitor::new();

        inhibitor.set_desired(true, &services);
        assert!(inhibitor.state().is_held());
        assert_eq!(services.inhibit_calls(), 1);

        inhibitor.set_desired(true, &services);
        assert_eq!(services.inhibit_calls(), 1, "no second acquisition");

        inhibitor.set_desired(false, &services);
        assert!(!inhibitor.state().is_held());
        assert_eq!(services.release_calls(), 1);

        // Releasing again is safe and does nothing.
        inhibitor.release(&services);
        assert_eq!(services.release_calls(), 1);
    }

    #[test]
    fn an_unavailable_mechanism_is_not_retried_as_if_held() {
        let services = NullPlatform::new("test");
        let mut inhibitor = Inhibitor::new();
        let state = inhibitor.set_desired(true, &services).clone();
        assert!(state.is_unavailable());
        assert!(!inhibitor.state().is_held());
    }

    #[cfg(unix)]
    #[test]
    fn hold_with_child_spawns_and_release_child_ends_and_reaps_it() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let state = hold_with_child(command, "sleep", Vec::new());
        let InhibitState::Held {
            mechanism,
            token: InhibitToken::Process(pid),
        } = state
        else {
            panic!("expected a held process token, got {state:?}");
        };
        assert_eq!(mechanism, "sleep");
        // If `release_child` failed to reap, this call would hang waiting
        // on a child that has already exited from the `SIGTERM` above —
        // exactly the zombie the shelled-out `kill` command used to leave.
        assert_eq!(
            crate::platform::inhibit::release_child(pid),
            crate::platform::Outcome::Done
        );
    }

    #[cfg(unix)]
    #[test]
    fn hold_with_child_reports_a_missing_program_and_keeps_earlier_attempts() {
        let command = Command::new("pulpit-test-program-that-does-not-exist");
        let state = hold_with_child(
            command,
            "missing-program",
            vec!["earlier mechanism: no answer".into()],
        );
        let InhibitState::Unavailable { attempts, .. } = state else {
            panic!("expected Unavailable, got {state:?}");
        };
        assert_eq!(attempts.len(), 2, "the earlier attempt must not be lost");
        assert_eq!(attempts[0], "earlier mechanism: no answer");
        assert!(attempts[1].contains("missing-program"));
    }
}
