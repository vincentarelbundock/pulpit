//! Claims: which copy of pulpit owns something only one copy may own.
//!
//! Several copies of pulpit may run at once — a second deck, a second
//! document, a file clicked in a file manager while the first window is
//! open — and reading, annotating, filling and signing are per-process work
//! that costs nothing to duplicate.
//!
//! The projector is not. Two audience windows on one screen make the window
//! manager flip between them many times a second, and what the presenter sees
//! is a violently flickering screen with no explanation, in the middle of a
//! talk. So the audience window is claimed, and a copy that cannot have it is
//! told who does rather than opening a second one.
//!
//! Each copy also claims a file naming itself, which is how another copy can
//! tell an abandoned crash-recovery file from one a running instance is still
//! writing to. The claim is a small file holding the process id, locked for as
//! long as the process lives; a file left behind by a crash is recognised as
//! stale and reclaimed, because refusing after a crash would be a worse
//! failure than the one being prevented.

use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// The outcome of trying to take a claim.
#[derive(Debug)]
pub enum Instance {
    /// This process holds the claim; the lock is released when it is dropped.
    Acquired(InstanceLock),
    /// Another live process holds it.
    #[allow(dead_code)] // unreached, including by its own tests
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
        // The lock releases when `_file` closes — with flock on Unix, with the
        // handle itself on Windows — so the unlink is only tidiness.
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

/// Take the claim recorded at `path`, if nobody else holds it.
pub fn acquire(path: &Path) -> Instance {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Instance::Unknown {
                reason: e.to_string(),
            };
        }
    }

    // Open or create the lock file. On Windows the open itself is the lock:
    // denying *write* sharing is what makes `is_claimed` answerable there,
    // since there is no flock to ask.
    //
    // Reading and deleting are shared, and both are load-bearing rather than
    // generous. Denying every kind of sharing — which this did — locked the
    // holder out of its own record: `read_pid` opens the file, so the pid
    // could not be read back while anybody held it. A second copy could
    // therefore never name the copy that was running, and the release in
    // `Drop` could never recognise the file as its own, so a claim released
    // cleanly left its file behind for good. Sharing deletion is what lets
    // that release remove the file while the handle proving it is ours is
    // still open.
    #[cfg(windows)]
    const SHARE_READ_AND_DELETE: u32 = 0x0000_0001 | 0x0000_0004;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(SHARE_READ_AND_DELETE);
    }
    let file = match options.open(path) {
        Ok(f) => f,
        // A file that exists but will not open is one somebody else has open
        // with sharing denied, which on Windows is precisely the holder.
        #[cfg(windows)]
        Err(_) if path.exists() => {
            return Instance::AlreadyRunning {
                pid: read_pid(path),
                lock: path.to_path_buf(),
            }
        }
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

/// Is a live process holding the claim at `path`?
///
/// A read-only probe. It never writes to the file and never leaves a lock
/// behind, because the file belongs to whoever holds it: this is how one
/// instance decides whether *another* instance's crash-recovery file has been
/// abandoned, and a probe that wrote would corrupt the record it is asking
/// about.
///
/// Undecidable cases answer "held". Recovering a file whose owner is still
/// running would take a live instance's journal away from it, which is worse
/// than leaving a genuinely stale file on disk until the next launch.
pub fn is_claimed(path: &Path) -> bool {
    #[cfg(unix)]
    {
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            Ok(file) => file,
            // No file at all is nobody's claim. A file that exists but cannot
            // be opened is somebody's, as far as this process can tell.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        };
        // LOCK_EX | LOCK_NB, then LOCK_UN: taking the lock is how the question
        // is asked, so it is given straight back.
        let taken = unsafe { libc::flock(file.as_raw_fd(), 2 | 4) } == 0;
        if taken {
            unsafe { libc::flock(file.as_raw_fd(), 8) };
        }
        !taken
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        if !path.exists() {
            return false;
        }
        // The holder opens its claim denying write sharing, so an open that
        // asks to write and succeeds proves there is no holder. The probe
        // shares nothing itself, which is what keeps two simultaneous probes
        // from both answering "free".
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
            .is_err()
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Nothing here can be asked honestly, so nothing is reclaimed.
        path.exists()
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
#[allow(dead_code)] // unreached, including by its own tests
fn is_running(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
#[cfg_attr(unix, allow(dead_code))] // its caller is the non-Unix fallback in `acquire`
fn is_running(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe is how one copy tells another copy's live recovery file from
    /// a dead one, so it must agree with the claim it is asking about.
    #[test]
    #[cfg(unix)]
    fn a_held_claim_reads_as_held_and_a_released_one_does_not() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live/4242.lock");

        assert!(!is_claimed(&path), "nothing has been claimed yet");
        let held = match acquire(&path) {
            Instance::Acquired(lock) => lock,
            other => panic!("expected the claim, got {other:?}"),
        };
        assert!(is_claimed(&path), "a claim in hand reads as held");
        drop(held);
        assert!(!is_claimed(&path), "a released claim is nobody's");
    }

    /// A probe must not become a claim, or the first question asked about a
    /// file would answer every later one wrongly.
    #[test]
    #[cfg(unix)]
    fn probing_leaves_the_claim_where_it_was() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live/7.lock");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "7").unwrap();

        assert!(!is_claimed(&path));
        assert!(!is_claimed(&path), "asking twice gives the same answer");
        // And the file the probe opened is still available to be claimed.
        match acquire(&path) {
            Instance::Acquired(_) => {}
            other => panic!("a probed file is still free, got {other:?}"),
        }
    }

    /// The pid recorded belongs to the holder. A probe that rewrote it would
    /// make the holder's own file name somebody else.
    #[test]
    #[cfg(unix)]
    fn probing_does_not_rewrite_the_holders_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("instance.pid");
        let held = match acquire(&path) {
            Instance::Acquired(lock) => lock,
            other => panic!("expected the claim, got {other:?}"),
        };
        assert!(is_claimed(&path));
        assert_eq!(read_pid(&path), Some(std::process::id()));
        drop(held);
    }
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
