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

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// The outcome of trying to become *the* running copy.
#[derive(Debug)]
pub enum Instance {
    /// This process holds the claim; the lock is released when it is dropped.
    Acquired(InstanceLock),
    /// Another live process holds it.
    AlreadyRunning { pid: Option<u32>, lock: PathBuf },
    /// The claim could not be recorded (an unwritable directory, say). The
    /// application still starts: a missing guard must never block a talk.
    Unknown { reason: String },
}

/// Releases the claim on drop.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // flock releases when `_file` closes, so the unlink is only tidiness.
        // It still has to be tidiness about *our* file: `remove_file` deletes
        // by path, not by inode, so unlinking unconditionally lets a departing
        // process delete the file a newly started one has just created. The
        // next launch then creates a third file and flocks a different inode,
        // leaving two processes both believing they hold the claim — the one
        // outcome this module exists to prevent. Only remove it while it still
        // names us.
        if read_pid(&self.path) == Some(std::process::id()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Claim the single-instance lock at `path`.
pub fn acquire(path: &Path) -> Instance {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Instance::Unknown {
                reason: e.to_string(),
            };
        }
    }

    // Open or create the lock file. We use OpenOptions to allow both creation
    // and opening of existing files.
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            return Instance::Unknown {
                reason: format!("cannot open lock file: {e}"),
            }
        }
    };

    // Try to acquire an exclusive non-blocking lock. The kernel arbitrates:
    // if flock succeeds, we own the lock; if it fails, someone else does.
    // Pid is used only for reporting, never for the decision.
    #[cfg(unix)]
    {
        // LOCK_EX = 2, LOCK_NB = 4 (non-blocking)
        let lock_result = unsafe { libc::flock(file.as_raw_fd(), 2 | 4) };

        if lock_result == 0 {
            // We got the lock. Write our PID to the file so the holder can be
            // reported on subsequent lock contention.
            match write_pid(&file) {
                Ok(()) => Instance::Acquired(InstanceLock {
                    path: path.to_path_buf(),
                    _file: file,
                }),
                Err(e) => Instance::Unknown {
                    reason: format!("cannot write lock file: {e}"),
                },
            }
        } else {
            // Lock is held by another process. Read the pid for reporting, but
            // don't use it to decide — flock already decided we don't own it.
            let pid = read_pid(path);
            Instance::AlreadyRunning {
                pid,
                lock: path.to_path_buf(),
            }
        }
    }

    #[cfg(not(unix))]
    {
        // Fallback for non-Unix (e.g., Windows): flock is not available, so use
        // the old pid-based approach. Pid reuse risk accepted on these systems.
        if let Some(pid) = read_pid(path) {
            if pid != std::process::id() && is_running(pid) {
                return Instance::AlreadyRunning {
                    pid: Some(pid),
                    lock: path.to_path_buf(),
                };
            }
        }

        match write_pid(&file) {
            Ok(()) => Instance::Acquired(InstanceLock {
                path: path.to_path_buf(),
                _file: file,
            }),
            Err(e) => Instance::Unknown {
                reason: e.to_string(),
            },
        }
    }
}

fn write_pid(file: &std::fs::File) -> std::io::Result<()> {
    use std::io::Seek;
    let mut file = file;
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    write!(file, "{}", std::process::id())?;
    Ok(())
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

    /// The kernel decides, so the claim is refused only while someone really
    /// holds it — and the pid in the file is used to *name* that holder, never
    /// to make the decision.
    #[test]
    #[cfg(unix)]
    fn a_second_copy_is_told_who_holds_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("instance.pid");

        // Keep the first claim alive: the lock lives on the open file, so
        // dropping it here would release it and the second call would win.
        let held = match acquire(&path) {
            Instance::Acquired(lock) => lock,
            other => panic!("the first copy should get the claim, got {other:?}"),
        };
        match acquire(&path) {
            Instance::AlreadyRunning { pid, .. } => {
                assert_eq!(pid, Some(std::process::id()), "the holder is named");
            }
            other => panic!("a second copy must be refused, got {other:?}"),
        }
        drop(held);
    }

    /// Pid reuse must not refuse a launch. A leftover file naming a live but
    /// unrelated process (pid 1 is always alive and is never us) once produced
    /// `AlreadyRunning` — the outcome this module's own docs call "a worse
    /// failure" — because the pid, not the kernel, was deciding.
    #[test]
    #[cfg(unix)]
    fn a_recycled_pid_in_a_stale_file_does_not_refuse_the_claim() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("instance.pid");
        std::fs::write(&path, "1").unwrap();
        match acquire(&path) {
            Instance::Acquired(_) => {}
            other => panic!("nobody holds the lock, so the claim is ours: got {other:?}"),
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
