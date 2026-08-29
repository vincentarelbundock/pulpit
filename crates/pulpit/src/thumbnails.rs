//! The deck as small pictures, kept apart from the frame cache.
//!
//! Two reasons this is not just more entries in [`FrameCache`]:
//!
//! An audience frame at 4K is about thirty megabytes; a thumbnail, held as
//! the encoded image the handle carries rather than as raw pixels, is tens
//! of kilobytes. Sharing a single LRU means a handful of page turns evicts
//! the entire grid, which is then rendered again from scratch the next time
//! it is opened — the churn is invisible until the moment it matters, which
//! is mid-talk. A separate budget makes the two kinds of frame unable to
//! hurt each other.
//!
//! And thumbnails are wanted for *every* page rather than the few around the
//! current one, so their eviction policy is different in kind: when the
//! budget is reached what should go is the page furthest from where the
//! presenter is, not the page least recently drawn. A grid is looked at all
//! at once.
//!
//! Warming is one pass at one width, chosen per document so the whole deck
//! fits the budget. A page's picture is rendered once and then never
//! replaced, which is what lets everything downstream treat a thumbnail
//! handle as permanent — no upgrade pass, no texture ever swapped under a
//! panel that is using the thumbnail as a stand-in.
//!
//! Nothing here is written to disk. The cost of a cold start is paid once per
//! session, in the background, before anyone asks for it.

use std::collections::HashMap;

use iced::widget::image::Handle;
use pulpit_core::RenderGeneration;

/// One page's picture, and what it costs to keep.
struct Thumbnail {
    handle: Handle,
    bytes: u64,
    /// How wide it was rendered. Warming is a single pass at a single width
    /// per document, so once a page has its picture the picture never
    /// changes — and no texture downstream ever swaps because of it.
    width: u32,
}

pub struct ThumbnailCache {
    entries: HashMap<usize, Thumbnail>,
    /// Which render generation these belong to. A new generation means a new
    /// document, or the same document re-read: every picture is stale.
    generation: RenderGeneration,
    budget_bytes: u64,
    used_bytes: u64,
}

impl std::fmt::Debug for ThumbnailCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbnailCache")
            .field("entries", &self.entries.len())
            .field("generation", &self.generation)
            .field("used_bytes", &self.used_bytes)
            .finish()
    }
}

impl ThumbnailCache {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            generation: RenderGeneration(0),
            budget_bytes,
            used_bytes: 0,
        }
    }

    pub fn generation(&self) -> RenderGeneration {
        self.generation
    }

    /// Start again for a new generation. Old pictures are of an old document
    /// — or an old version of this one — and showing them would be a lie.
    pub fn reset(&mut self, generation: RenderGeneration) {
        self.entries.clear();
        self.used_bytes = 0;
        self.generation = generation;
    }

    pub fn contains(&self, slide: usize) -> bool {
        self.entries.contains_key(&slide)
    }

    /// Is there a picture at least this wide already?
    pub fn has_at_least(&self, slide: usize, width: u32) -> bool {
        self.entries
            .get(&slide)
            .is_some_and(|entry| entry.width >= width)
    }

    pub fn get(&self, slide: usize) -> Option<Handle> {
        self.entries.get(&slide).map(|entry| entry.handle.clone())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// The width this document's pictures were warmed at.
    ///
    /// One pass at one width per document, so any entry answers for all of
    /// them. It is the whole story of how legible a thumbnail is — and so of
    /// whether one could stand in for a page being scrolled past — and until
    /// it was reported the only way to learn it was to read the code that
    /// chose it.
    pub fn width(&self) -> Option<u32> {
        self.entries.values().next().map(|entry| entry.width)
    }

    /// Roughly how many pages of `width` and `height` this budget holds,
    /// never more than `count`.
    ///
    /// Used to bound a re-sweep of pages that went missing: a window this
    /// size around the presenter is one the cache can hold all of, so filling
    /// it cannot evict anything else in it. That property is the whole point,
    /// and an estimate that comes out too *high* destroys it — the sweep
    /// reaches past what fits, evicts the near end to reach the far end, then
    /// re-requests the near end, for ever.
    ///
    /// Which is what happened. This assumed a square page on the stated
    /// grounds that "a page is taller than it is wide, so square is the
    /// conservative reading" — but that has it backwards: a square page is
    /// *cheaper* than a real one, so the estimate made pages look small and
    /// the capacity look large. On a 655-page deck at 176px it claimed 135
    /// pages fit where 87 did, and the warmer rendered the deck twenty-one
    /// times in one sitting.
    ///
    /// What the pages actually cost is better evidence than what they ought
    /// to cost, so the measured average is used once there is one; the
    /// dimensions are the estimate for an empty cache — a raw-pixel figure,
    /// which overstates an encoded page enormously and so only makes the
    /// first window smaller than it could be, the safe direction. An eighth
    /// is held back either way, because pages within one document are not
    /// all the same size and the average is only an average.
    pub fn capacity_at(&self, width: u32, height: u32, count: usize) -> usize {
        let per_page = if self.entries.is_empty() {
            (width as u64)
                .saturating_mul(height.max(1) as u64)
                .saturating_mul(4)
        } else {
            self.used_bytes / self.entries.len() as u64
        };
        if per_page == 0 {
            return count;
        }
        let headroom = self.budget_bytes - self.budget_bytes / 8;
        ((headroom / per_page) as usize).min(count)
    }

    /// Keep a picture, making room by dropping the pages furthest from
    /// `around` — which is where the presenter is, and so where the next
    /// thing they look at almost certainly is too.
    pub fn insert(&mut self, slide: usize, handle: Handle, bytes: u64, width: u32, around: usize) {
        // A wider picture is never replaced by a narrower one: a late coarse
        // frame — requested before a reload changed the plan — must not undo
        // a sharper picture already up.
        if self.has_at_least(slide, width.saturating_add(1)) {
            return;
        }
        if let Some(previous) = self.entries.remove(&slide) {
            self.used_bytes = self.used_bytes.saturating_sub(previous.bytes);
        }
        self.used_bytes += bytes;
        self.entries.insert(
            slide,
            Thumbnail {
                handle,
                bytes,
                width,
            },
        );
        self.trim(around);
    }

    fn trim(&mut self, around: usize) {
        // A deck whose pictures exceed the budget gives up its furthest
        // pages. The warming plan chooses a width the whole deck fits at, so
        // this is a backstop, not a policy that fires in normal use.
        while self.used_bytes > self.budget_bytes && self.entries.len() > 1 {
            let Some(&furthest) = self
                .entries
                .keys()
                .max_by_key(|slide| slide.abs_diff(around))
            else {
                return;
            };
            // Never drop the page we are keeping room for.
            if furthest == around {
                return;
            }
            if let Some(entry) = self.entries.remove(&furthest) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> Handle {
        Handle::from_rgba(1, 1, vec![0, 0, 0, 255])
    }

    fn cache(budget: u64) -> ThumbnailCache {
        ThumbnailCache::new(budget)
    }

    #[test]
    fn a_new_generation_forgets_everything() {
        let mut cache = cache(1000);
        cache.insert(3, handle(), 100, 240, 3);
        assert!(cache.contains(3));

        cache.reset(RenderGeneration(1));
        assert!(!cache.contains(3), "an old document's pictures are stale");
        assert_eq!(cache.used_bytes(), 0);
        assert_eq!(cache.generation(), RenderGeneration(1));
    }

    #[test]
    fn the_budget_drops_the_furthest_page_first() {
        // Room for three at a hundred bytes each.
        let mut cache = cache(300);
        for slide in [10, 11, 40, 12] {
            cache.insert(slide, handle(), 100, 240, 10);
        }

        assert!(cache.used_bytes() <= 300);
        assert!(cache.contains(10), "the page we are on stays");
        assert!(cache.contains(11));
        assert!(cache.contains(12));
        assert!(!cache.contains(40), "the far one goes first");
    }

    #[test]
    fn replacing_a_picture_does_not_double_count_it() {
        let mut cache = cache(1000);
        cache.insert(5, handle(), 100, 240, 5);
        cache.insert(5, handle(), 100, 240, 5);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.used_bytes(), 100);
    }

    #[test]
    fn a_wide_picture_is_not_undone_by_a_late_narrow_one() {
        // A reload can change the deck's chosen width; a straggler render
        // from the old plan must not undo a sharper picture already up.
        let mut cache = cache(1000);
        cache.insert(5, handle(), 400, 480, 5);
        cache.insert(5, handle(), 100, 240, 5);
        assert!(cache.has_at_least(5, 480), "the wider picture stays");
        assert_eq!(cache.used_bytes(), 400);

        cache.insert(5, handle(), 800, 960, 5);
        assert!(cache.has_at_least(5, 960), "a wider one still replaces it");
        assert_eq!(cache.used_bytes(), 800, "and frees the one it replaced");
    }

    #[test]
    fn the_page_being_kept_room_for_is_never_evicted() {
        // A budget smaller than one picture must not spin or empty itself.
        let mut cache = cache(10);
        cache.insert(7, handle(), 100, 240, 7);
        assert!(cache.contains(7));
    }
}
