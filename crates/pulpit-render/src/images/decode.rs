//! Bounded decoding, a byte-bounded cache of decoded images, and scaling.
//!
//! `SPEC-images.md` §46.1 and §47. Two properties carry the whole tier:
//!
//! * **Dimensions are read from the header, never from a decode** (§46.1).
//!   Opening a folder of high-resolution photographs must not stall on pixel
//!   data nobody has asked for yet.
//! * **Input pixel dimensions are bounded before decode** (§47.2). The
//!   16 384px limit in [`crate::pdf::RenderRequest::validate`] bounds the
//!   *output*; a 64 000 × 64 000 PNG is a decompression bomb that never
//!   reaches it.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use image::imageops::FilterType;
use image::{GenericImageView, RgbaImage};
use pulpit_core::notes::Region;

/// The largest input either dimension may have before pulpit refuses to
/// decode it (§47.2).
///
/// The same number the output limit uses, and for the same reason: 16k × 16k
/// RGBA is already a gigabyte, and anything beyond it is a bug or an attack.
pub const MAX_INPUT_DIMENSION: u32 = 16_384;

/// The largest input *area*, which is the bound that actually matters: a
/// 16 000 × 16 000 image is inside the per-side limit and still a gigabyte of
/// decoded pixels.
pub const MAX_INPUT_PIXELS: u64 = 80_000_000;

/// How many bytes of decoded image one worker keeps (§47.1).
///
/// Its own budget, distinct from the frame cache's and from `thumbnails.rs`:
/// re-scaling one decoded image across the audience frame, the presenter
/// frame and a thumbnail must not decode it three times, and three
/// simultaneous decoded 24-megapixel photographs is what this has to hold.
pub const DEFAULT_DECODED_BUDGET_BYTES: u64 = 384 * 1024 * 1024;

/// Why one image could not become a page.
#[derive(Debug, thiserror::Error)]
pub enum ImageFailure {
    #[error("cannot read {path}: {reason}")]
    Unreadable { path: String, reason: String },
    #[error("{path} is {width}×{height}, beyond the {MAX_INPUT_DIMENSION}px input limit")]
    TooLarge {
        path: String,
        width: u32,
        height: u32,
    },
    #[error("cannot decode {path}: {reason}")]
    Undecodable { path: String, reason: String },
    #[error("render was cancelled")]
    Cancelled,
}

impl From<ImageFailure> for crate::pdf::PdfError {
    fn from(failure: ImageFailure) -> crate::pdf::PdfError {
        match failure {
            ImageFailure::Cancelled => crate::pdf::PdfError::Cancelled,
            // §49.1: a listed file that will not decode fails its *render*.
            // It is not dropped from the page table and it does not become a
            // placeholder; the last complete audience frame holds and the
            // presenter view names the file.
            other => crate::pdf::PdfError::Render(other.to_string()),
        }
    }
}

/// Pixel dimensions from the file header alone (§46.1).
pub fn dimensions(path: &Path) -> Result<(u32, u32), ImageFailure> {
    let reader = image::ImageReader::open(path).map_err(|e| ImageFailure::Unreadable {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    let reader = reader
        .with_guessed_format()
        .map_err(|e| ImageFailure::Unreadable {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    reader
        .into_dimensions()
        .map_err(|e| ImageFailure::Undecodable {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
}

/// Pixel dimensions of an image already in memory, from its header alone.
///
/// The archive path (§54.5): an entry is never extracted to disk, so the
/// bytes are here and only the header is parsed. Header-only still means what
/// it means — no pixel data is decoded — even though getting to the header of
/// a compressed entry meant inflating it.
pub fn dimensions_of(bytes: &[u8], label: &str) -> Result<(u32, u32), ImageFailure> {
    reader_over(bytes, label)?
        .into_dimensions()
        .map_err(|e| ImageFailure::Undecodable {
            path: label.to_string(),
            reason: e.to_string(),
        })
}

/// Decode an image already in memory to RGBA8, under the same input bound as
/// [`decode`] (§47.2).
pub fn decode_bytes(bytes: &[u8], label: &str) -> Result<RgbaImage, ImageFailure> {
    let (width, height) = dimensions_of(bytes, label)?;
    if !within_input_bounds(width, height) {
        return Err(ImageFailure::TooLarge {
            path: label.to_string(),
            width,
            height,
        });
    }
    let decoded = reader_over(bytes, label)?
        .decode()
        .map_err(|e| ImageFailure::Undecodable {
            path: label.to_string(),
            reason: e.to_string(),
        })?;
    Ok(decoded.into_rgba8())
}

type MemoryReader<'a> = image::ImageReader<std::io::Cursor<&'a [u8]>>;

fn reader_over<'a>(bytes: &'a [u8], label: &str) -> Result<MemoryReader<'a>, ImageFailure> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ImageFailure::Unreadable {
            path: label.to_string(),
            reason: e.to_string(),
        })
}

/// Is an image of this size worth decoding at all?
pub fn within_input_bounds(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_INPUT_DIMENSION
        && height <= MAX_INPUT_DIMENSION
        && u64::from(width) * u64::from(height) <= MAX_INPUT_PIXELS
}

/// Decode one image to RGBA8, refusing anything past the input bound *before*
/// any pixel memory is touched (§47.2).
pub fn decode(path: &Path) -> Result<RgbaImage, ImageFailure> {
    let (width, height) = dimensions(path)?;
    if !within_input_bounds(width, height) {
        return Err(ImageFailure::TooLarge {
            path: path.display().to_string(),
            width,
            height,
        });
    }
    let reader = image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|e| ImageFailure::Unreadable {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    // Animated GIF and WebP present their first frame (§41.3): a presenter
    // window is not a media player, and this is what a plain decode gives.
    let decoded = reader.decode().map_err(|e| ImageFailure::Undecodable {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    Ok(decoded.into_rgba8())
}

/// How a page names itself in a diagnostic: a path, or `archive!entry`.
pub fn label_of(location: &crate::images::table::PageLocation<'_>) -> String {
    use crate::images::table::PageLocation;
    match location {
        PageLocation::File(path) => path.display().to_string(),
        PageLocation::ArchiveEntry { archive, name, .. } => {
            format!("{}!{}", archive.display(), name.to_string_lossy())
        }
    }
}

/// Pixel dimensions of one page, wherever it lives (§46.1, §54.5).
pub fn dimensions_at(
    location: &crate::images::table::PageLocation<'_>,
) -> Result<(u32, u32), ImageFailure> {
    use crate::images::table::PageLocation;
    match location {
        PageLocation::File(path) => dimensions(path),
        PageLocation::ArchiveEntry {
            archive,
            kind,
            name,
        } => {
            let bytes = crate::images::archive::read_entry(archive, *kind, name)?;
            dimensions_of(&bytes, &label_of(location))
        }
    }
}

/// Decode one page, wherever it lives.
pub fn decode_at(
    location: &crate::images::table::PageLocation<'_>,
) -> Result<RgbaImage, ImageFailure> {
    use crate::images::table::PageLocation;
    match location {
        PageLocation::File(path) => decode(path),
        PageLocation::ArchiveEntry {
            archive,
            kind,
            name,
        } => {
            let bytes = crate::images::archive::read_entry(archive, *kind, name)?;
            decode_bytes(&bytes, &label_of(location))
        }
    }
}

/// Which decoded image a cache entry is.
///
/// Carries the file's length and mtime, not only its position: an export that
/// rewrites `slide03.png` in place must not be answered from the pixels of
/// the picture that used to be there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedKey {
    pub document: u64,
    pub page: usize,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

struct CacheEntry {
    key: DecodedKey,
    image: Arc<RgbaImage>,
    /// Monotonic tick of the last hit; the smallest is evicted first.
    used_at: u64,
}

/// A byte-bounded LRU of decoded images, held in the worker (§47.1).
pub struct DecodedCache {
    budget: u64,
    used: u64,
    tick: u64,
    entries: Vec<CacheEntry>,
}

impl Default for DecodedCache {
    fn default() -> DecodedCache {
        DecodedCache::new(DEFAULT_DECODED_BUDGET_BYTES)
    }
}

impl DecodedCache {
    pub fn new(budget: u64) -> DecodedCache {
        DecodedCache {
            budget,
            used: 0,
            tick: 0,
            entries: Vec::new(),
        }
    }

    pub fn bytes_used(&self) -> u64 {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&mut self, key: &DecodedKey) -> Option<Arc<RgbaImage>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.iter_mut().find(|entry| &entry.key == key)?;
        entry.used_at = tick;
        Some(Arc::clone(&entry.image))
    }

    pub fn insert(&mut self, key: DecodedKey, image: Arc<RgbaImage>) {
        let cost = cost_of(&image);
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            let replaced = self.entries.remove(index);
            self.used = self.used.saturating_sub(cost_of(&replaced.image));
        }
        // A single image larger than the whole budget is held anyway — the
        // alternative is decoding it again for every one of the three frames
        // that want it — but it is the first thing evicted.
        self.tick += 1;
        self.entries.push(CacheEntry {
            key,
            image,
            used_at: self.tick,
        });
        self.used += cost;
        self.evict_to_budget();
    }

    /// Drop everything belonging to one document, which is what closing it
    /// means: a reload gives the replacement a new id, so its pages would
    /// otherwise sit in the cache for the rest of the process.
    pub fn forget_document(&mut self, document: u64) {
        let mut freed = 0;
        self.entries.retain(|entry| {
            let leaving = entry.key.document == document;
            if leaving {
                freed += cost_of(&entry.image);
            }
            !leaving
        });
        self.used = self.used.saturating_sub(freed);
    }

    fn evict_to_budget(&mut self) {
        while self.used > self.budget && self.entries.len() > 1 {
            let Some(index) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used_at)
                .map(|(index, _)| index)
            else {
                break;
            };
            let evicted = self.entries.remove(index);
            self.used = self.used.saturating_sub(cost_of(&evicted.image));
        }
    }
}

fn cost_of(image: &RgbaImage) -> u64 {
    u64::from(image.width()) * u64::from(image.height()) * 4
}

/// Crop `image` to `region` and scale it to `width` × `height`, writing
/// tightly packed RGBA8 into `target`.
///
/// `target` must hold at least `width * height * 4` bytes; the caller sizes
/// it, which is what lets the worker point this straight at its shared-memory
/// mapping.
pub fn scale_into(
    image: &RgbaImage,
    region: Region,
    width: u32,
    height: u32,
    target: &mut [u8],
) -> Result<(), ImageFailure> {
    let needed = width as usize * height as usize * 4;
    if target.len() < needed {
        return Err(ImageFailure::Undecodable {
            path: "<frame>".into(),
            reason: format!("target holds {} bytes, needs {needed}", target.len()),
        });
    }
    // §82.7: a whole-page render at the page's own pixel size — the common
    // case, and the only shape a target too small for the frame ever refuses
    // above — used to clone the decoded image to "crop" it to itself and
    // then copy that clone into `target`: two full-frame copies for a no-op.
    // The decoded buffer already *is* the answer here, so it is copied
    // straight out of `image` with neither clone nor resample.
    if region == Region::FULL && image.width() == width && image.height() == height {
        target[..needed].copy_from_slice(image.as_raw());
        return Ok(());
    }
    // CatmullRom rather than Lanczos3: a projected photograph shows the
    // ringing Lanczos puts on hard edges, and the difference in sharpness at
    // presentation sizes is not visible from a seat.
    if region == Region::FULL {
        // No crop at all: resample straight from the borrowed source, rather
        // than cloning it first only to resample the clone.
        let scaled = image::imageops::resize(image, width, height, FilterType::CatmullRom);
        target[..needed].copy_from_slice(scaled.as_raw());
        return Ok(());
    }
    // A real crop borrows a view rather than allocating a copy just to throw
    // it at the resampler (or, when no resampling is even needed below, just
    // to throw it away): `crop_imm` costs nothing until something is done
    // with what it names.
    let cropped = crop_view(image, region);
    if cropped.width() == width && cropped.height() == height {
        // The one case where the crop *is* the whole answer: materialising
        // it is unavoidable here, because a `SubImage` is not contiguous and
        // `target` needs contiguous bytes, but it is exactly one copy rather
        // than the resample path's would-be copy of a copy.
        let materialized = cropped.to_image();
        target[..needed].copy_from_slice(materialized.as_raw());
    } else {
        let scaled =
            image::imageops::resize(cropped.inner(), width, height, FilterType::CatmullRom);
        target[..needed].copy_from_slice(scaled.as_raw());
    }
    Ok(())
}

/// A borrowed view of the part of `image` that `region` names, in pixels.
///
/// Rounded outwards to at least one pixel: a crop that rounded to zero would
/// panic in the resampler rather than draw a very small piece of the page.
fn crop_view(image: &RgbaImage, region: Region) -> image::SubImage<&RgbaImage> {
    let (full_width, full_height) = (image.width(), image.height());
    let x = ((region.x * full_width as f32).round() as i64).clamp(0, full_width as i64 - 1) as u32;
    let y =
        ((region.y * full_height as f32).round() as i64).clamp(0, full_height as i64 - 1) as u32;
    let w = ((region.width * full_width as f32).round() as i64).clamp(1, (full_width - x) as i64)
        as u32;
    let h = ((region.height * full_height as f32).round() as i64).clamp(1, (full_height - y) as i64)
        as u32;
    image::imageops::crop_imm(image, x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> Arc<RgbaImage> {
        Arc::new(RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([10, 20, 30, 255]),
        ))
    }

    fn key(page: usize) -> DecodedKey {
        DecodedKey {
            document: 1,
            page,
            len: 100,
            modified: None,
        }
    }

    #[test]
    fn a_decoded_image_is_answered_from_the_cache_rather_than_decoded_again() {
        let mut cache = DecodedCache::new(1024 * 1024);
        cache.insert(key(0), image(4, 4));
        assert!(cache.get(&key(0)).is_some());
        assert_eq!(cache.bytes_used(), 4 * 4 * 4);
    }

    #[test]
    fn a_file_overwritten_in_place_is_not_answered_from_its_old_pixels() {
        let mut cache = DecodedCache::new(1024 * 1024);
        cache.insert(key(0), image(4, 4));
        let rewritten = DecodedKey {
            modified: Some(SystemTime::UNIX_EPOCH),
            ..key(0)
        };
        assert!(cache.get(&rewritten).is_none());
    }

    #[test]
    fn the_least_recently_used_image_is_the_one_that_goes() {
        let mut cache = DecodedCache::new(4 * 4 * 4 * 2);
        cache.insert(key(0), image(4, 4));
        cache.insert(key(1), image(4, 4));
        assert!(cache.get(&key(0)).is_some(), "page 0 touched most recently");
        cache.insert(key(2), image(4, 4));
        assert!(cache.get(&key(1)).is_none(), "page 1 was the coldest");
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.get(&key(2)).is_some());
    }

    #[test]
    fn closing_a_document_releases_its_pixels() {
        let mut cache = DecodedCache::new(1024 * 1024);
        cache.insert(key(0), image(8, 8));
        cache.forget_document(1);
        assert!(cache.is_empty());
        assert_eq!(cache.bytes_used(), 0);
    }

    #[test]
    fn the_input_bound_refuses_a_decompression_bomb_before_any_pixels() {
        assert!(within_input_bounds(4000, 3000));
        assert!(!within_input_bounds(64_000, 64_000));
        assert!(
            !within_input_bounds(16_000, 16_000),
            "inside the per-side limit and still a gigabyte of pixels"
        );
        assert!(!within_input_bounds(0, 10));
    }

    #[test]
    fn a_full_region_scales_the_whole_image() {
        let source = RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
        let mut target = vec![0u8; 2 * 2 * 4];
        scale_into(&source, Region::FULL, 2, 2, &mut target).unwrap();
        assert_eq!(&target[..4], &[1, 2, 3, 255]);
    }

    /// §82.7: a whole-page render at the page's own size is the no-crop,
    /// no-resample path — the one that used to clone the decoded image to
    /// "crop" it to itself before copying that clone out. Every pixel,
    /// not just the first, must still be the byte-for-byte source.
    #[test]
    fn a_full_region_at_native_size_copies_the_source_buffer_exactly() {
        let mut source = RgbaImage::from_pixel(3, 2, image::Rgba([0, 0, 0, 0]));
        source.put_pixel(0, 0, image::Rgba([1, 2, 3, 255]));
        source.put_pixel(2, 1, image::Rgba([9, 8, 7, 255]));
        let mut target = vec![0xAAu8; 3 * 2 * 4];
        scale_into(&source, Region::FULL, 3, 2, &mut target).unwrap();
        assert_eq!(target, source.as_raw().as_slice());
    }

    #[test]
    fn a_crop_takes_the_part_of_the_image_the_region_names() {
        let mut source = RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        source.put_pixel(2, 2, image::Rgba([9, 9, 9, 255]));
        let mut target = vec![0u8; 4];
        scale_into(
            &source,
            Region::new(0.5, 0.5, 0.25, 0.25),
            1,
            1,
            &mut target,
        )
        .unwrap();
        assert_eq!(target, [9, 9, 9, 255]);
    }

    #[test]
    fn a_target_too_small_for_the_frame_is_refused_rather_than_overrun() {
        let source = RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
        let mut target = vec![0u8; 4];
        assert!(scale_into(&source, Region::FULL, 2, 2, &mut target).is_err());
    }
}
