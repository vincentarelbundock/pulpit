//! One presenter, one process.
//!
//! Two copies of pulpit on one machine is not a harmless mistake: both
//! open an audience window, both claim the projector, and the window manager
//! flips between them many times a second. What the presenter sees is a
//! violently flickering screen with no explanation, in the middle of a talk.
//!
//! So the second copy declines to start and says where the first one is. The
//! record is a small file holding the process id; a file left behind by a
//! crash is recognised as stale and reclaimed, because refusing to start
//! after a crash would be a worse failure than the one being prevented.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The outcome of trying to become *the* running copy.
#[derive(Debug)]
pub enum Instance {
    /// This process holds the claim; the lock is released when it is dropped.
    Acquired(InstanceLock),
    /// Another live process holds it.
    AlreadyRunning { pid: u32, lock: PathBuf },
    /// The claim could not be recorded (an unwritable directory, say). The
    /// application still starts: a missing guard must never block a talk.
    Unknown { reason: String },
}

/// Releases the claim on drop.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Only remove the file if it is still ours.
        if read_pid(&self.path) == Some(std::process::id()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Claim the single-instance lock at `path`.
pub fn acquire(path: &Path) -> Instance {
    if let Some(pid) = read_pid(path) {
        if pid != std::process::id() && is_running(pid) {
            return Instance::AlreadyRunning {
                pid,
                lock: path.to_path_buf(),
            };
        }
        // Stale: the previous run died without cleaning up.
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Instance::Unknown {
                reason: e.to_string(),
            };
        }
    }
    match std::fs::File::create(path)
        .and_then(|mut file| write!(file, "{}", std::process::id()).map(|_| ()))
    {
        Ok(()) => Instance::Acquired(InstanceLock {
            path: path.to_path_buf(),
        }),
        Err(e) => Instance::Unknown {
            reason: e.to_string(),
        },
    }
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Is a process with this id alive?
///
/// Only answered where it can be answered honestly. Everywhere else the
/// guard stands down rather than guessing, because a wrong "yes" would stop
/// a presenter from starting at all.
#[cfg(target_os = "linux")]
fn is_running(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn is_running(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_copy_gets_the_claim() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/instance.pid");
        let instance = acquire(&path);
        assert!(matches!(instance, Instance::Acquired(_)));
        assert_eq!(read_pid(&path), Some(std::process::id()));
    }

    #[test]
    fn a_second_copy_is_told_who_holds_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("instance.pid");
        // A live process that is not us: the test runner's parent is not
        // portable to assume, so use a pid we know exists — our own — and
        // check the *stale* path separately below.
        std::fs::write(&path, "1").unwrap();
        match acquire(&path) {
            Instance::AlreadyRunning { pid, .. } => assert_eq!(pid, 1),
            // On a platform where liveness cannot be checked the guard stands
            // down and the claim is taken; that is the documented behaviour.
            Instance::Acquired(_) if !cfg!(target_os = "linux") => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_lock_left_by_a_crash_is_reclaimed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("instance.pid");
        // A pid that cannot be running: the kernel maximum plus one.
        std::fs::write(&path, "4194305").unwrap();
        assert!(matches!(acquire(&path), Instance::Acquired(_)));
    }

    #[test]
    fn releasing_removes_the_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("instance.pid");
        match acquire(&path) {
            Instance::Acquired(lock) => drop(lock),
            other => panic!("expected the claim, got {other:?}"),
        }
        assert!(!path.exists(), "a released claim leaves nothing behind");
    }

    #[test]
    fn an_unwritable_location_does_not_stop_the_application() {
        // Blocked by a *file* standing where a directory would have to be,
        // which no platform will create through. An absolute path under
        // `/proc` looks unwritable but is not portable: on Windows
        // `create_dir_all` cheerfully makes `C:\proc\definitely\not`, the
        // claim succeeds, and the test fails while littering the machine.
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("not-a-directory");
        std::fs::write(&blocker, b"").unwrap();

        let instance = acquire(&blocker.join("instance.pid"));
        assert!(
            matches!(instance, Instance::Unknown { .. }),
            "a location that cannot be written is reported, not fatal: {instance:?}"
        );
    }
}
