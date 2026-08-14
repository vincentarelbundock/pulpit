//! The continuous surface transport (`docs-src/internals.typ`).
//!
//! A session owns a bounded ring of shared-memory slots. The worker writes
//! only into a free slot and announces it; the application releases the slot
//! once the pixels are uploaded. When the consumer falls behind the worker
//! *drops* intermediate frames rather than blocking playback or growing a
//! queue, because a browser's clock and an audio clock do not wait.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{Mmap, MmapMut};

use crate::protocol::{ProtocolError, SurfaceSlot, MAX_SLOT_BYTES};

/// Three slots: one being displayed, one just published, one being written.
pub const DEFAULT_SLOTS: u32 = 3;

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

/// Hands out process-unique ring names so two documents, or two runs, never
/// collide in the shared-memory namespace.
#[derive(Debug)]
pub struct RingNamer {
    prefix: String,
    next: AtomicU64,
}

impl RingNamer {
    pub fn new() -> Self {
        Self {
            prefix: format!("pulpit-media-{}", std::process::id()),
            next: AtomicU64::new(0),
        }
    }

    pub fn next_name(&self) -> String {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        format!("{}-{index}", self.prefix)
    }
}

impl Default for RingNamer {
    fn default() -> Self {
        Self::new()
    }
}

fn ring_bytes(slots: u32, slot_bytes: u64) -> Result<u64, ProtocolError> {
    if slots == 0 || slots > 8 {
        return Err(ProtocolError::Malformed(format!(
            "{slots} slots is outside the supported range"
        )));
    }
    if slot_bytes == 0 || slot_bytes > MAX_SLOT_BYTES {
        return Err(ProtocolError::Malformed(format!(
            "slot size {slot_bytes} out of range"
        )));
    }
    (slots as u64)
        .checked_mul(slot_bytes)
        .filter(|total| *total <= MAX_SLOT_BYTES * 8)
        .ok_or_else(|| ProtocolError::Malformed("ring size overflows".into()))
}

/// The supervisor's end of a ring: it creates the region and tracks which
/// slots the application still holds.
#[derive(Debug)]
pub struct SurfaceRing {
    name: String,
    path: PathBuf,
    map: Mmap,
    slots: u32,
    slot_bytes: u64,
    /// Slots currently held by the application, by slot index.
    held: Vec<bool>,
}

impl SurfaceRing {
    pub fn create(name: &str, slots: u32, slot_bytes: u64) -> Result<Self, ProtocolError> {
        let total = ring_bytes(slots, slot_bytes)?;
        let path = path_for(name)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.set_len(total)?;
        // The supervisor only ever reads: workers are the writers. Mapping
        // read-only means a supervisor bug cannot corrupt a frame in flight.
        let map = unsafe { Mmap::map(&file)? };
        Ok(Self {
            name: name.to_string(),
            path,
            map,
            slots,
            slot_bytes,
            held: vec![false; slots as usize],
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }

    pub fn slot_bytes(&self) -> u64 {
        self.slot_bytes
    }

    /// Borrow one slot's bytes. `len` has already been validated against
    /// `slot_bytes` by [`crate::protocol::SurfaceFrame::validate`].
    pub fn read_slot(&self, slot: SurfaceSlot, len: u64) -> Result<&[u8], ProtocolError> {
        if slot.0 >= self.slots || len > self.slot_bytes {
            return Err(ProtocolError::Malformed("slot read out of range".into()));
        }
        let start = (slot.0 as u64) * self.slot_bytes;
        let end = start + len;
        let (start, end) = (start as usize, end as usize);
        self.map
            .get(start..end)
            .ok_or_else(|| ProtocolError::Malformed("slot read past the mapping".into()))
    }

    pub fn hold(&mut self, slot: SurfaceSlot) {
        if let Some(held) = self.held.get_mut(slot.0 as usize) {
            *held = true;
        }
    }

    pub fn release(&mut self, slot: SurfaceSlot) {
        if let Some(held) = self.held.get_mut(slot.0 as usize) {
            *held = false;
        }
    }

    pub fn is_held(&self, slot: SurfaceSlot) -> bool {
        self.held.get(slot.0 as usize).copied().unwrap_or(false)
    }
}

impl Drop for SurfaceRing {
    fn drop(&mut self) {
        // A leaked region would survive the process; losing the unlink is a
        // diagnostic, never a presentation failure.
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(region = %self.name, error = %e, "could not unlink a surface ring");
            }
        }
    }
}

/// The worker's end of a ring: it attaches to the region the supervisor named
/// and writes frames into whichever slot is currently free.
#[derive(Debug)]
pub struct AttachedRing {
    map: MmapMut,
    slots: u32,
    slot_bytes: u64,
    /// Slots the supervisor has not yet released, newest first.
    outstanding: Vec<(SurfaceSlot, u64)>,
    next_slot: u32,
    dropped: u64,
}

impl AttachedRing {
    pub fn attach(name: &str, slots: u32, slot_bytes: u64) -> Result<Self, ProtocolError> {
        let total = ring_bytes(slots, slot_bytes)?;
        let path = path_for(name)?;
        let file = File::options().read(true).write(true).open(&path)?;
        let length = file.metadata()?.len();
        if length < total {
            return Err(ProtocolError::Malformed(format!(
                "ring {name} is {length} bytes but {total} were promised"
            )));
        }
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self {
            map,
            slots,
            slot_bytes,
            outstanding: Vec::new(),
            next_slot: 0,
            dropped: 0,
        })
    }

    pub fn slot_bytes(&self) -> u64 {
        self.slot_bytes
    }

    /// How many frames were discarded because every slot was busy.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The supervisor is finished with a slot.
    pub fn release(&mut self, slot: SurfaceSlot, sequence: u64) {
        self.outstanding
            .retain(|(held, held_sequence)| !(*held == slot && *held_sequence == sequence));
    }

    /// Claim a free slot and write `frame` into it.
    ///
    /// Returns `None` when every slot is still held — the caller drops this
    /// frame and carries on, which is the documented behaviour: playback must
    /// never block on a slow consumer.
    pub fn write_frame(
        &mut self,
        frame: &[u8],
        sequence: u64,
    ) -> Result<Option<SurfaceSlot>, ProtocolError> {
        if frame.len() as u64 > self.slot_bytes {
            return Err(ProtocolError::Malformed(format!(
                "a {} byte frame does not fit a {} byte slot",
                frame.len(),
                self.slot_bytes
            )));
        }
        let Some(slot) = self.claim_slot() else {
            self.dropped += 1;
            return Ok(None);
        };
        let start = (slot.0 as u64 * self.slot_bytes) as usize;
        let end = start + frame.len();
        let destination = self
            .map
            .get_mut(start..end)
            .ok_or_else(|| ProtocolError::Malformed("slot write past the mapping".into()))?;
        destination.copy_from_slice(frame);
        self.outstanding.push((slot, sequence));
        Ok(Some(slot))
    }

    fn claim_slot(&mut self) -> Option<SurfaceSlot> {
        for _ in 0..self.slots {
            let candidate = SurfaceSlot(self.next_slot);
            self.next_slot = (self.next_slot + 1) % self.slots;
            if !self.outstanding.iter().any(|(slot, _)| *slot == candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique(tag: &str) -> String {
        format!(
            "pulpit-test-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')', ' '], "")
    }

    #[test]
    fn a_ring_round_trips_a_frame_through_shared_memory() {
        let name = unique("roundtrip");
        let mut ring = SurfaceRing::create(&name, 3, 64).unwrap();
        let mut attached = AttachedRing::attach(&name, 3, 64).unwrap();

        let pixels = vec![0xABu8; 64];
        let slot = attached.write_frame(&pixels, 1).unwrap().unwrap();
        assert_eq!(ring.read_slot(slot, 64).unwrap(), &pixels[..]);
        ring.release(slot);
    }

    #[test]
    fn slots_rotate_so_a_published_frame_is_not_overwritten_while_held() {
        let name = unique("rotate");
        let _ring = SurfaceRing::create(&name, 3, 16).unwrap();
        let mut attached = AttachedRing::attach(&name, 3, 16).unwrap();

        let first = attached.write_frame(&[1u8; 16], 1).unwrap().unwrap();
        let second = attached.write_frame(&[2u8; 16], 2).unwrap().unwrap();
        let third = attached.write_frame(&[3u8; 16], 3).unwrap().unwrap();
        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_ne!(first, third);
    }

    #[test]
    fn a_full_ring_drops_the_frame_instead_of_blocking_playback() {
        let name = unique("full");
        let _ring = SurfaceRing::create(&name, 2, 16).unwrap();
        let mut attached = AttachedRing::attach(&name, 2, 16).unwrap();

        let first = attached.write_frame(&[1u8; 16], 1).unwrap().unwrap();
        attached.write_frame(&[2u8; 16], 2).unwrap().unwrap();
        assert!(
            attached.write_frame(&[3u8; 16], 3).unwrap().is_none(),
            "with both slots held the newest frame is dropped, not queued"
        );
        assert_eq!(attached.dropped(), 1);

        // Releasing one slot lets playback publish again, and the slot that
        // comes back is the one that was released.
        attached.release(first, 1);
        assert_eq!(attached.write_frame(&[4u8; 16], 4).unwrap(), Some(first));
    }

    #[test]
    fn a_frame_larger_than_its_slot_is_refused_rather_than_truncated() {
        let name = unique("oversize");
        let _ring = SurfaceRing::create(&name, 2, 16).unwrap();
        let mut attached = AttachedRing::attach(&name, 2, 16).unwrap();
        assert!(attached.write_frame(&[0u8; 17], 1).is_err());
    }

    #[test]
    fn reads_outside_the_ring_are_refused() {
        let name = unique("bounds");
        let ring = SurfaceRing::create(&name, 2, 16).unwrap();
        assert!(ring.read_slot(SurfaceSlot(2), 16).is_err());
        assert!(ring.read_slot(SurfaceSlot(0), 17).is_err());
        assert!(ring.read_slot(SurfaceSlot(0), 16).is_ok());
    }

    #[test]
    fn implausible_ring_geometry_is_refused_before_any_mapping() {
        assert!(SurfaceRing::create(&unique("zero"), 0, 16).is_err());
        assert!(SurfaceRing::create(&unique("many"), 99, 16).is_err());
        assert!(SurfaceRing::create(&unique("huge"), 3, MAX_SLOT_BYTES + 1).is_err());
        assert!(SurfaceRing::create("has/separator", 3, 16).is_err());
    }

    #[test]
    fn a_ring_unlinks_itself_so_nothing_leaks_into_dev_shm() {
        let name = unique("cleanup");
        let path = {
            let ring = SurfaceRing::create(&name, 2, 16).unwrap();
            ring.path.clone()
        };
        assert!(
            !path.exists(),
            "the region is gone once the ring is dropped"
        );
    }

    #[test]
    fn attaching_to_a_shorter_region_than_promised_is_refused() {
        let name = unique("short");
        let _ring = SurfaceRing::create(&name, 2, 16).unwrap();
        assert!(
            AttachedRing::attach(&name, 4, 64).is_err(),
            "a worker must not map more than the supervisor allocated"
        );
    }

    #[test]
    fn ring_names_are_unique_per_process_and_per_ring() {
        let namer = RingNamer::new();
        let first = namer.next_name();
        let second = namer.next_name();
        assert_ne!(first, second);
        assert!(first.contains(&std::process::id().to_string()));
    }
}
