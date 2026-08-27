//! Reusable shared-memory regions for bitmap transfer.
//!
//! Pixels never travel through the IPC pipe. The supervisor owns a small pool
//! of regions, hands one to a worker per job, and the worker writes the frame
//! directly into it. Sizes are validated on both sides before mapping.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use memmap2::{Mmap, MmapMut};

use crate::protocol::{ProtocolError, MAX_REGION_BYTES};

/// The path a region name refers to, refusing a name that could escape the
/// directory. The directory and the safety rule come from `pulpit-core`, so
/// this crate and the media crate cannot disagree about where regions live.
fn path_for(name: &str) -> Result<PathBuf, ProtocolError> {
    pulpit_core::ipc::shm::path_for(name)
        .ok_or_else(|| ProtocolError::Malformed(format!("unsafe region name {name:?}")))
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

// A region cannot be unlinked immediately after being mapped, the way an
// anonymous one would be: `AttachedRegion::open` has to find it by name from
// another process. So a process that is killed before `Drop` runs leaves its
// file behind in `/dev/shm`.
//
// The startup sweep is what reclaims those: `Names::for_this_process` runs it
// before handing out the first name, and it reads the owning pid out of every
// naming scheme pulpit writes, so a crash costs the space only until the next
// run. `Drop` still unlinks on the way out of a normal exit.

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

impl RegionNamer {
    pub fn new() -> Self {
        // `Names` sweeps as it is built, at the point where regions start
        // being created, which reclaims tmpfs from any prior crash. It also
        // owns the prefix, so the sweep can always read back who owns a file
        // it finds — including files this crate did not write.
        Self {
            prefix: pulpit_core::ipc::shm::Names::for_this_process(None)
                .prefix()
                .to_string(),
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
    use pulpit_core::ipc::shm::base_directory;

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
}
