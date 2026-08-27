//! Reusable shared-memory regions for bitmap transfer.
//!
//! Pixels never travel through the IPC pipe. The supervisor owns a small pool
//! of regions, hands one to a worker per job, and the worker writes the frame
//! directly into it. Sizes are validated on both sides before mapping.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use memmap2::{Mmap, MmapMut};

use crate::protocol::{ProtocolError, MAX_REGION_BYTES};

/// Sweep stale region files from a directory. This is the core sweep logic,
/// extracted to be testable. It is called from `sweep_stale_regions_once()`
/// which wraps it in a `std::sync::Once` to run only once per process.
fn sweep_stale_regions_in_directory(base: &Path, current_pid: u32) {
    // Try to read the directory. If it fails, bail silently: sweep failures
    // must never prevent region creation.
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let filename = match path.file_name() {
            Some(name) => match name.to_str() {
                Some(s) => s,
                None => continue,
            },
            None => continue,
        };

        // Match the pattern `pulpit-<pid>-*` and extract the pid.
        // The format is deterministic: prefix "pulpit-", then decimal pid, then "-".
        let pid = match extract_pid_from_region_name(filename) {
            Some(p) => p,
            None => continue,
        };

        // Never remove files owned by the current process.
        if pid == current_pid {
            continue;
        }

        // On non-Linux platforms, we cannot reliably check process liveness,
        // so skip the sweep entirely to avoid false positives.
        #[cfg(not(target_os = "linux"))]
        {
            continue;
        }

        // On Linux, check if the process is still running, then remove the
        // stale file. Other platforms skip the entry above because they
        // cannot reliably establish that the owning process is gone.
        #[cfg(target_os = "linux")]
        {
            if is_process_alive(pid) {
                continue;
            }

            // Silently ignore errors: a failed removal must never prevent a
            // region from being created, and errors here (permission denied,
            // file already gone, etc.) are not actionable.
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Sweep stale region files from `/dev/shm` once per process. Called from
/// `RegionNamer::new()` via `std::sync::Once` to run exactly once.
fn sweep_stale_regions() {
    let base = base_directory();
    let current_pid = std::process::id();
    sweep_stale_regions_in_directory(&base, current_pid);
}

/// Extract the pid from a region filename, for every naming scheme pulpit
/// uses: `pulpit-<pid>-*` for render regions, and `pulpit-<label>-<pid>-*` for
/// a labelled consumer such as the media rings (`pulpit-media-<pid>-<n>`).
///
/// The pid is the first `-`-separated component that parses as a number, and
/// never the last one, since every scheme puts a discriminator after it.
///
/// Reading only as far as the first dash — which is what this did — takes
/// `"media"` from a media ring and fails to parse it, so the file is skipped.
/// That is how media rings came to survive every sweep and sit in tmpfs until
/// reboot: the owning process was already dead, and `SurfaceRing`'s `Drop`,
/// the only other cleanup, does not run on a crash or a `SIGKILL`.
///
/// A label that was itself numeric would be mistaken for the pid, so labels
/// have to stay non-numeric. The tests below pin that.
fn extract_pid_from_region_name(filename: &str) -> Option<u32> {
    const PREFIX: &str = "pulpit-";

    let remainder = filename.strip_prefix(PREFIX)?;
    let mut components = remainder.split('-').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            // The last component is the discriminator, never the pid.
            break;
        }
        if let Ok(pid) = component.parse() {
            return Some(pid);
        }
    }
    None
}

/// Is a process with this id alive on Linux?
#[cfg(target_os = "linux")]
fn is_process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Where regions live. `/dev/shm` is tmpfs on Linux; the temp dir is the
/// portable fallback.
fn base_directory() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

fn path_for(name: &str) -> Result<PathBuf, ProtocolError> {
    if name.is_empty() || name.len() > 256 || name.contains(['/', '\\', '\0']) {
        return Err(ProtocolError::Malformed(format!(
            "unsafe region name {name:?}"
        )));
    }
    Ok(base_directory().join(name))
}

/// A writable region owned by its creator. Unlinked on drop.
#[derive(Debug)]
pub struct SharedRegion {
    name: String,
    path: PathBuf,
    map: MmapMut,
}

impl SharedRegion {
    pub fn create(name: &str, bytes: u64) -> Result<Self, ProtocolError> {
        if bytes == 0 || bytes > MAX_REGION_BYTES {
            return Err(ProtocolError::Malformed(format!(
                "region size {bytes} out of range"
            )));
        }
        let path = path_for(name)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        file.set_len(bytes)?;
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self {
            name: name.to_string(),
            path,
            map,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.map
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.map
    }

    /// Grow the region when a later job needs more room. Regions are reused
    /// across jobs precisely so this is rare.
    pub fn ensure_capacity(&mut self, bytes: u64) -> Result<(), ProtocolError> {
        if bytes as usize <= self.map.len() {
            return Ok(());
        }
        if bytes > MAX_REGION_BYTES {
            return Err(ProtocolError::Malformed(format!(
                "region size {bytes} out of range"
            )));
        }
        let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        file.set_len(bytes)?;
        self.map = unsafe { MmapMut::map_mut(&file)? };
        Ok(())
    }
}

impl Drop for SharedRegion {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// RESIDUAL LEAK: If the process holding a SharedRegion is killed or crashes
// before Drop runs, the file persists in /dev/shm. The file cannot be unlinked
// immediately after mmap in create() because AttachedRegion::open() must be
// able to open it by name in a separate worker process. A comprehensive fix
// would require either: (1) a startup sweep of stale pulpit-* files, or
// (2) a separate cleanup service. For now, regions are unlinked on normal
// process shutdown via Drop.

/// A region opened by the other side of the protocol.
#[derive(Debug)]
pub struct AttachedRegion {
    map: MmapMut,
}

impl AttachedRegion {
    /// Open an existing region, refusing anything smaller than the frame the
    /// message claims to have produced.
    pub fn open(name: &str, expected_bytes: u64) -> Result<Self, ProtocolError> {
        if expected_bytes == 0 || expected_bytes > MAX_REGION_BYTES {
            return Err(ProtocolError::Malformed(format!(
                "declared size {expected_bytes} out of range"
            )));
        }
        let path = path_for(name)?;
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let length = file.metadata()?.len();
        if length < expected_bytes {
            return Err(ProtocolError::Malformed(format!(
                "region {name} holds {length} bytes but {expected_bytes} were declared"
            )));
        }
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self { map })
    }

    pub fn read_only(name: &str, expected_bytes: u64) -> Result<Mmap, ProtocolError> {
        let path = path_for(name)?;
        let file = File::open(&path)?;
        let length = file.metadata()?.len();
        if length < expected_bytes {
            return Err(ProtocolError::Malformed(format!(
                "region {name} holds {length} bytes but {expected_bytes} were declared"
            )));
        }
        Ok(unsafe { Mmap::map(&file)? })
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.map
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.map
    }
}

/// Generates unique region names for one process.
#[derive(Debug)]
/// Names are unique per process: the counter is process-global so two
/// namers (a supervisor and a test, say) can never collide and unlink each
/// other's regions. Names include entropy derived from time and a per-process
/// random seed to prevent predictability.
pub struct RegionNamer {
    prefix: String,
}

static REGION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SWEEP_ONCE: std::sync::Once = std::sync::Once::new();

impl RegionNamer {
    pub fn new() -> Self {
        // Sweep stale regions once per process, at the point where regions
        // start being created. This reclaims tmpfs space from any prior crashes.
        SWEEP_ONCE.call_once(sweep_stale_regions);

        Self {
            prefix: format!("pulpit-{}", std::process::id()),
        }
    }

    pub fn next(&self) -> String {
        use std::hash::{BuildHasher, Hasher, RandomState};

        let ticket = REGION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);

        // RandomState is seeded per process by the standard library; hashing
        // the ticket and the clock through it gives a name an attacker cannot
        // predict from the pid alone.
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(ticket);
        hasher.write_u64(nanos);
        hasher.write_u32(std::process::id());
        format!("{}-{:016x}", self.prefix, hasher.finish())
    }
}

impl Default for RegionNamer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_round_trips_between_owner_and_attacher() {
        let namer = RegionNamer::new();
        let name = namer.next();
        let mut region = SharedRegion::create(&name, 1024).unwrap();
        region.as_mut_slice()[..4].copy_from_slice(&[1, 2, 3, 4]);

        let attached = AttachedRegion::open(&name, 1024).unwrap();
        assert_eq!(&attached.as_slice()[..4], &[1, 2, 3, 4]);
    }

    #[test]
    fn a_region_that_is_too_small_is_refused_rather_than_mapped() {
        let namer = RegionNamer::new();
        let name = namer.next();
        let _region = SharedRegion::create(&name, 64).unwrap();
        assert!(AttachedRegion::open(&name, 4096).is_err());
    }

    #[test]
    fn unsafe_names_and_sizes_are_refused() {
        assert!(SharedRegion::create("../escape", 64).is_err());
        assert!(SharedRegion::create("ok", 0).is_err());
        assert!(SharedRegion::create("ok", MAX_REGION_BYTES + 1).is_err());
        assert!(AttachedRegion::open("also/bad", 64).is_err());
    }

    #[test]
    fn regions_grow_and_are_unlinked_on_drop() {
        let namer = RegionNamer::new();
        let name = namer.next();
        let path = base_directory().join(&name);
        {
            let mut region = SharedRegion::create(&name, 128).unwrap();
            region.ensure_capacity(8192).unwrap();
            assert!(region.len() >= 8192);
            assert!(path.exists());
        }
        assert!(!path.exists(), "regions do not leak into /dev/shm");
    }

    #[test]
    fn a_preplanted_file_is_refused_rather_than_adopted() {
        let namer = RegionNamer::new();
        let name = namer.next();
        let path = base_directory().join(&name);

        // Plant a file at the region name
        std::fs::write(&path, b"planted").unwrap();
        assert!(path.exists(), "file was planted");

        // Attempt to create a region with the same name should fail
        let result = SharedRegion::create(&name, 1024);
        assert!(
            result.is_err(),
            "create_new should refuse an existing file instead of adopting it"
        );

        // Clean up the planted file
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[cfg(unix)]
    fn region_files_are_created_with_mode_0o600() {
        use std::os::unix::fs::PermissionsExt;

        let namer = RegionNamer::new();
        let name = namer.next();
        let path = base_directory().join(&name);
        {
            let _region = SharedRegion::create(&name, 1024).unwrap();
            assert!(path.exists(), "region file was created");

            let metadata = std::fs::metadata(&path).unwrap();
            let mode = metadata.permissions().mode();
            // Check that the mode is 0o600 (readable and writable only by owner)
            assert_eq!(
                mode & 0o777,
                0o600,
                "region file mode should be 0o600, got {:#o}",
                mode & 0o777
            );
        }
        assert!(!path.exists(), "region was cleaned up");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stale_region_files_are_swept_but_current_process_files_are_kept() {
        let base = base_directory();
        let current_pid = std::process::id();
        // Use a definitely-dead pid: the kernel max plus one will never be assigned.
        let dead_pid = 4194305u32;

        // Create a file matching the dead-pid pattern.
        let dead_file_name = format!("pulpit-{}-0", dead_pid);
        let dead_path = base.join(&dead_file_name);
        std::fs::write(&dead_path, b"stale").unwrap();
        assert!(dead_path.exists(), "dead-pid file was created");

        // Create a file matching the current-pid pattern.
        let current_file_name = format!("pulpit-{}-test", current_pid);
        let current_path = base.join(&current_file_name);
        std::fs::write(&current_path, b"current").unwrap();
        assert!(current_path.exists(), "current-pid file was created");

        // Call the sweep logic directly. This is not protected by Once in tests,
        // so we can call it multiple times to verify the behavior.
        sweep_stale_regions_in_directory(&base, current_pid);

        // After the sweep, the dead-pid file should be removed (because the
        // process is not alive), but the current-pid file should remain.
        assert!(!dead_path.exists(), "stale file for dead process was swept");
        assert!(current_path.exists(), "file for current process was kept");

        // Clean up the current-pid test file.
        let _ = std::fs::remove_file(&current_path);
    }

    /// `pulpit-media` names its rings `pulpit-media-<pid>-<n>`, and this
    /// sweeper is the only thing that reclaims them after a crash: the ring's
    /// own `Drop` does not run on a `SIGKILL`. Reading the pid only as far as
    /// the first dash took `"media"`, failed to parse it, and skipped the
    /// file — so every crash with an overlay playing leaked its rings until
    /// the machine was rebooted.
    ///
    /// The two crates cannot see each other, so nothing but this test ties the
    /// sweeper to the name `surface.rs` actually produces. Keep them in step.
    #[test]
    #[cfg(target_os = "linux")]
    fn stale_media_rings_are_swept_too() {
        let base = base_directory();
        let current_pid = std::process::id();
        let dead_pid = 4194305u32;

        // Exactly the shape of `RingNamer`'s prefix in pulpit-media.
        let dead_ring = base.join(format!("pulpit-media-{dead_pid}-0"));
        std::fs::write(&dead_ring, b"stale ring").unwrap();
        let live_ring = base.join(format!("pulpit-media-{current_pid}-0"));
        std::fs::write(&live_ring, b"live ring").unwrap();

        sweep_stale_regions_in_directory(&base, current_pid);

        assert!(
            !dead_ring.exists(),
            "a media ring whose owner is gone must be reclaimed"
        );
        assert!(
            live_ring.exists(),
            "a media ring belonging to this process must be left alone"
        );

        let _ = std::fs::remove_file(&live_ring);
    }

    #[test]
    fn a_pid_is_read_past_a_label_and_never_from_the_last_component() {
        // Render regions: the pid comes first.
        assert_eq!(
            extract_pid_from_region_name("pulpit-1234-abcdef"),
            Some(1234)
        );
        // Media rings: the pid comes after a non-numeric label.
        assert_eq!(
            extract_pid_from_region_name("pulpit-media-1234-0"),
            Some(1234)
        );
        // Not ours.
        assert_eq!(extract_pid_from_region_name("something-else-1234-0"), None);
        // A bare label with nothing after it names no process.
        assert_eq!(extract_pid_from_region_name("pulpit-media"), None);
        // The trailing component is a discriminator, not a pid, so a name that
        // ends in a number must not be read as owned by it.
        assert_eq!(extract_pid_from_region_name("pulpit-media-7"), None);
    }
}
