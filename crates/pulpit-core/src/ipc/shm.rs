//! Where shared-memory regions live, what they are called, and reclaiming the
//! ones whose owner died.
//!
//! Mapping a region is each crate's own business — a render frame and a media
//! ring are different objects with different lifetimes. Naming them is not:
//! the sweep can only reclaim a file whose name it can read, so a consumer
//! that invents its own name invents a leak.
//!
//! That is not hypothetical. Media rings were named `pulpit-media-<pid>-<n>`
//! while the sweep, which lived in the other crate, read the pid only as far
//! as the first dash; it took `"media"`, failed to parse it, and skipped the
//! file for as long as that code existed. Every crash with an overlay playing
//! left its rings in tmpfs until the machine was rebooted. Names come from
//! [`Names`] now so that the sweep and the namer cannot disagree again.

use std::path::{Path, PathBuf};

/// Where regions live. `/dev/shm` is tmpfs on Linux; the temp dir is the
/// portable fallback.
pub fn base_directory() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// The path a region name refers to, refusing a name that could escape the
/// directory or confuse the filesystem.
///
/// Returns `None` for an unsafe name; the caller turns that into its own
/// protocol error, since the two crates word theirs differently.
pub fn path_for(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.len() > 256
        || name.contains(['/', '\\', '\0'])
        || name == "."
        || name == ".."
    {
        return None;
    }
    Some(base_directory().join(name))
}

/// The naming scheme for one consumer's regions in this process.
///
/// `label` distinguishes consumers sharing the directory: `None` gives
/// `pulpit-<pid>-…` and `Some("media")` gives `pulpit-media-<pid>-…`. What
/// follows the prefix is the consumer's own business — a counter, a hash —
/// because only the prefix has to be legible to the sweep.
///
/// A label must not be a number, or [`owner_of`] would read it as the pid.
pub struct Names {
    prefix: String,
}

impl Names {
    /// Begin naming regions for this process, sweeping stale ones first.
    ///
    /// The sweep runs once per process however many consumers ask, which is
    /// why it is attached to naming rather than left for a caller to remember:
    /// the first region created is the moment the space is wanted.
    pub fn for_this_process(label: Option<&str>) -> Names {
        debug_assert!(
            !label.is_some_and(|l| l.parse::<u32>().is_ok()),
            "a numeric label would be read as the pid"
        );
        sweep_once();
        let pid = std::process::id();
        Names {
            prefix: match label {
                Some(label) => format!("pulpit-{label}-{pid}"),
                None => format!("pulpit-{pid}"),
            },
        }
    }

    /// The prefix every name from this consumer starts with.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

static SWEEP_ONCE: std::sync::Once = std::sync::Once::new();

/// Reclaim stale regions once per process.
pub fn sweep_once() {
    SWEEP_ONCE.call_once(|| sweep(&base_directory(), std::process::id()));
}

/// Read the owning pid out of a region filename.
///
/// The pid is the first `-`-separated component that parses as a number, and
/// never the last one, since every scheme puts a discriminator after it. That
/// is what lets one sweep serve `pulpit-<pid>-…` and `pulpit-<label>-<pid>-…`
/// alike.
pub(crate) fn owner_of(filename: &str) -> Option<u32> {
    const PREFIX: &str = "pulpit-";

    let remainder = filename.strip_prefix(PREFIX)?;
    let mut components = remainder.split('-').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            break;
        }
        if let Ok(pid) = component.parse() {
            return Some(pid);
        }
    }
    None
}

/// Remove regions in `base` whose owning process is gone.
///
/// Failures are ignored throughout: a sweep that cannot read the directory or
/// cannot remove a file must never stop a region from being created. Reclaim
/// is best-effort by nature — the space is already lost if this does nothing.
pub(crate) fn sweep(base: &Path, current_pid: u32) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(pid) = owner_of(filename) else {
            continue;
        };
        // Never remove files owned by the current process.
        if pid == current_pid {
            continue;
        }
        // On Linux, `/proc/<pid>` says outright whether the owner is still
        // alive, so a live owner's region is never touched.
        #[cfg(target_os = "linux")]
        {
            if Path::new(&format!("/proc/{pid}")).exists() {
                continue;
            }
            let _ = std::fs::remove_file(&path);
        }
        // Elsewhere there is no reliable liveness check (§77.9), so a region
        // is reclaimed by age instead: one untouched for a day is either
        // abandoned by a dead process or stale enough that relying on it
        // further would be worse than losing it. This is what keeps a crash
        // off Linux from leaking rings forever, the way the Linux-only check
        // used to.
        #[cfg(not(target_os = "linux"))]
        {
            let stale = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE_REGION_AGE);
            if stale {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// How long an off-Linux region may sit untouched before the sweep reclaims
/// it in place of the liveness check Linux gets from `/proc`.
///
/// A day, not a session: a region can legitimately outlive a short suspend,
/// and the sweep only runs once per process anyway, so there is no cost to
/// erring toward patience.
#[cfg(not(target_os = "linux"))]
const STALE_REGION_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pid_is_read_past_a_label_and_never_from_the_last_component() {
        // Render regions: the pid comes first.
        assert_eq!(owner_of("pulpit-1234-abcdef"), Some(1234));
        // Media rings: the pid comes after a non-numeric label.
        assert_eq!(owner_of("pulpit-media-1234-0"), Some(1234));
        // Not ours.
        assert_eq!(owner_of("something-else-1234-0"), None);
        // A bare label names no process.
        assert_eq!(owner_of("pulpit-media"), None);
        // The trailing component is a discriminator, so a name ending in a
        // number must not be read as owned by it.
        assert_eq!(owner_of("pulpit-media-7"), None);
    }

    #[test]
    fn every_name_a_consumer_can_make_is_legible_to_the_sweep() {
        // The property that keeps the leak from coming back: whatever a
        // consumer appends, the prefix still says who owns it.
        let pid = std::process::id();
        for label in [None, Some("media"), Some("overlay")] {
            let names = Names::for_this_process(label);
            for suffix in ["0", "abcdef", "7"] {
                let name = format!("{}-{suffix}", names.prefix());
                assert_eq!(
                    owner_of(&name),
                    Some(pid),
                    "{name} must be attributable to this process"
                );
            }
        }
    }

    #[test]
    fn unsafe_names_are_refused() {
        assert!(path_for("").is_none());
        assert!(path_for("../escape").is_none());
        assert!(path_for("with/slash").is_none());
        assert!(path_for(&"x".repeat(257)).is_none());
        assert!(path_for("pulpit-1-0").is_some());
        // §77.9: a bare "." or ".." names the region directory itself, or
        // its parent, rather than a region inside it.
        assert!(path_for(".").is_none());
        assert!(path_for("..").is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_dead_owners_region_is_reclaimed_whatever_its_label() {
        let base = base_directory();
        let current_pid = std::process::id();
        // The kernel maximum plus one is never assigned.
        let dead_pid = 4194305u32;

        let dead = [
            base.join(format!("pulpit-{dead_pid}-0")),
            base.join(format!("pulpit-media-{dead_pid}-0")),
        ];
        let live = [
            base.join(format!("pulpit-{current_pid}-sweeptest")),
            base.join(format!("pulpit-media-{current_pid}-sweeptest")),
        ];
        for path in dead.iter().chain(live.iter()) {
            std::fs::write(path, b"region").unwrap();
        }

        sweep(&base, current_pid);

        for path in &dead {
            assert!(!path.exists(), "{path:?} outlived its owner and must go");
        }
        for path in &live {
            assert!(path.exists(), "{path:?} belongs to us and must stay");
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn off_linux_a_stale_owners_region_is_reclaimed_by_age() {
        // §77.9: without `/proc` there is no liveness check, so a dead
        // owner's region must still go, just on a different signal — age
        // rather than a live PID.
        let base = base_directory();
        let current_pid = std::process::id();
        let dead_pid = 4194305u32;

        let old = base.join(format!("pulpit-{dead_pid}-old"));
        let fresh = base.join(format!("pulpit-{dead_pid}-fresh"));
        let live = base.join(format!("pulpit-{current_pid}-sweeptest"));
        for path in [&old, &fresh, &live] {
            std::fs::write(path, b"region").unwrap();
        }
        let long_ago = std::time::SystemTime::now() - (STALE_REGION_AGE * 2);
        std::fs::File::open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        sweep(&base, current_pid);

        assert!(!old.exists(), "old enough, and its owner is gone");
        assert!(fresh.exists(), "too fresh to assume the owner is gone");
        assert!(live.exists(), "belongs to us and must stay");
        for path in [&fresh, &live] {
            let _ = std::fs::remove_file(path);
        }
    }
}
