//! Bounded frame cache.
//!
//! Eviction is bounded by *decoded memory cost*, never by page count: a
//! 3840×2160 RGBA bitmap is 33,177,600 bytes, so "keep 20 pages" is not a
//! memory policy.
//!
//! What is counted is the decoded bitmap, which is what this cache holds. The
//! textures made from those bitmaps belong to a window's renderer, one copy
//! per window that draws them, and neither their size nor their lifetime is
//! visible from here — so they are not guessed at. Keeping a window's set of
//! them small is `pulpit::residency`'s job.

use std::collections::HashMap;
use std::sync::Arc;

use pulpit_core::RenderGeneration;

/// Which projection of the document a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FrameKind {
    Slide,
    Notes,
    /// A reader page, drawn with the document's own annotations in the
    /// pixels. `FrameKey::slide` carries the page index for this kind.
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameKey {
    pub generation: RenderGeneration,
    pub slide: usize,
    pub kind: FrameKind,
    pub width: u32,
    pub height: u32,
}

/// A decoded RGBA frame. Cheap to clone: the pixels are shared.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
}

impl Frame {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
        }
    }

    pub fn cpu_bytes(&self) -> u64 {
        self.pixels.len() as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub frames: usize,
    pub cpu_bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    /// Frames rejected because they were larger than the whole budget.
    pub rejected: u64,
    /// How far over budget the cache currently sits because everything left
    /// was pinned. Zero whenever the budget actually holds; nonzero is the
    /// honest admission the budget could not be enforced.
    pub pinned_overcommit_bytes: u64,
}

impl CacheStats {
    pub fn total_bytes(&self) -> u64 {
        self.cpu_bytes
    }
}

#[derive(Debug)]
struct Entry {
    frame: Frame,
    /// A `Cell` because the hot lookups — `best`, `best_fitting` — take
    /// `&self` from view construction. Without touching recency there, the
    /// "least recently used" eviction was really insertion order: the
    /// views never call `get`, so nothing the views showed ever counted as
    /// used.
    last_used: std::cell::Cell<u64>,
}

/// A cache bounded by the bytes of the bitmaps it holds.
#[derive(Debug)]
pub struct FrameCache {
    entries: HashMap<FrameKey, Entry>,
    budget_bytes: u64,
    clock: std::cell::Cell<u64>,
    stats: CacheStats,
    /// Keys that must not be evicted: the frames currently on screen.
    pinned: Vec<FrameKey>,
    /// How many entries each generation still has resident. Fallback lookups
    /// walk these actual generations rather than every integer since the
    /// application started.
    resident: std::collections::BTreeMap<RenderGeneration, usize>,
    /// Keys evicted since the caller last asked, so whatever it derived from
    /// those frames — image handles, textures — can be dropped in step.
    evicted_keys: Vec<FrameKey>,
}

/// The default combined budget from the specification.
pub const DEFAULT_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

impl Default for FrameCache {
    fn default() -> Self {
        Self::new(DEFAULT_BUDGET_BYTES)
    }
}

impl FrameCache {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            budget_bytes,
            clock: std::cell::Cell::new(0),
            stats: CacheStats::default(),
            pinned: Vec::new(),
            resident: std::collections::BTreeMap::new(),
            evicted_keys: Vec::new(),
        }
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Mark the frames that are on screen right now. They are never evicted.
    pub fn pin(&mut self, keys: Vec<FrameKey>) {
        self.pinned = keys;
    }

    /// How many frames are resident.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: &FrameKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Advance the usage clock. `&self`: lookups from view construction
    /// count as use.
    fn tick(&self) -> u64 {
        let clock = self.clock.get() + 1;
        self.clock.set(clock);
        clock
    }

    pub fn get(&mut self, key: &FrameKey) -> Option<Frame> {
        let clock = self.tick();
        match self.entries.get_mut(key) {
            Some(entry) => {
                entry.last_used.set(clock);
                self.stats.hits += 1;
                Some(entry.frame.clone())
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    /// The widest frame there is for a slide, whatever it was drawn for.
    /// This is what keeps a valid image on screen while the one a surface
    /// actually wants renders.
    pub fn best(
        &self,
        generation: RenderGeneration,
        slide: usize,
        kind: FrameKind,
    ) -> Option<(FrameKey, Frame)> {
        self.entries
            .iter()
            .filter(|(key, _)| {
                key.generation == generation && key.slide == slide && key.kind == kind
            })
            .max_by_key(|(key, _)| key.width)
            .map(|(key, entry)| {
                entry.last_used.set(self.tick());
                (*key, entry.frame.clone())
            })
    }

    /// Best frame at exactly one size. Canonical display slots use this so an
    /// audience-size texture can never become an intermediate display step.
    ///
    /// Height is part of the identity, not decoration: a `/FitR` zoom re-crops
    /// the committed page, so the cache can hold two frames of the same page
    /// at the same width and different heights. Matching on width alone left
    /// the choice between them to hash order, and the projector could flip
    /// between cropped and uncropped from pass to pass.
    pub fn best_exact(
        &self,
        generation: RenderGeneration,
        slide: usize,
        kind: FrameKind,
        width: u32,
        height: u32,
    ) -> Option<(FrameKey, Frame)> {
        self.entries
            .iter()
            .find(|(key, _)| {
                key.generation == generation
                    && key.slide == slide
                    && key.kind == kind
                    && key.width == width
                    && key.height == height
            })
            .map(|(key, entry)| {
                entry.last_used.set(self.tick());
                (*key, entry.frame.clone())
            })
    }

    /// The best frame no wider than `max_width`, for a small panel.
    ///
    /// Returns `None` when every cached frame is larger, so the caller can
    /// decide between showing an oversized one and showing nothing.
    pub fn best_within(
        &self,
        generation: RenderGeneration,
        slide: usize,
        kind: FrameKind,
        max_width: u32,
    ) -> Option<(FrameKey, Frame)> {
        self.entries
            .iter()
            .filter(|(key, _)| {
                key.generation == generation
                    && key.slide == slide
                    && key.kind == kind
                    && key.width <= max_width
            })
            .max_by_key(|(key, _)| key.width)
            .map(|(key, entry)| {
                entry.last_used.set(self.tick());
                (*key, entry.frame.clone())
            })
    }

    /// [`best_within`](Self::best_within) and [`best`](Self::best) in one
    /// scan: the best frame no wider than `max_width`, or failing that the
    /// best at any width.
    pub fn best_fitting(
        &self,
        generation: RenderGeneration,
        slide: usize,
        kind: FrameKind,
        max_width: u32,
    ) -> Option<(FrameKey, Frame)> {
        // Downsampling beats upsampling, so the smallest frame *at least* as
        // wide as the cell wins; only when there is none does the widest
        // narrower one stand in.
        //
        // Preferring the widest that fits — the obvious reading of "fitting"
        // — is how leaving fullscreen used to leave every page soft and keep
        // it that way. The cell shrinks, a small frame now fits, it is
        // upsampled into the cell, and `satisfies` sees the *wide* frame,
        // decides the page is covered and asks for no replacement. Nothing
        // ever sharpens it. The two functions have to agree about which
        // frame answers a request, and this is that agreement.
        let mut at_least: Option<(&FrameKey, &Entry)> = None;
        let mut below: Option<(&FrameKey, &Entry)> = None;
        for (key, entry) in &self.entries {
            if key.generation != generation || key.slide != slide || key.kind != kind {
                continue;
            }
            if key.width >= max_width {
                if at_least.is_none_or(|(best, _)| key.width < best.width) {
                    at_least = Some((key, entry));
                }
            } else if below.is_none_or(|(best, _)| key.width > best.width) {
                below = Some((key, entry));
            }
        }
        let choice = at_least.or(below);
        choice.map(|(key, entry)| {
            entry.last_used.set(self.tick());
            (*key, entry.frame.clone())
        })
    }

    /// Whether a cached frame already satisfies a render request, so the
    /// request need not be submitted at all.
    ///
    /// A request asks for a frame *near* the requested width: satisfied only
    /// by one in `[width, 2 × width]`. "Any wider frame counts" was wrong
    /// here — once a slide had an audience-resolution frame, its preview-size
    /// render was skipped forever, and the presenter panels were left leaning
    /// on a thirty-megabyte frame that eviction takes first. The panels' own
    /// small frames must keep being rendered even when a giant exists.
    pub fn satisfies(
        &self,
        generation: RenderGeneration,
        slide: usize,
        kind: FrameKind,
        width: u32,
    ) -> bool {
        self.best_within(generation, slide, kind, width.saturating_mul(2))
            .is_some_and(|(existing, _)| existing.width >= width)
    }

    pub fn insert(&mut self, key: FrameKey, frame: Frame) -> bool {
        let cost = frame.cpu_bytes();
        if cost > self.budget_bytes {
            // A single frame larger than the entire budget is refused rather
            // than allowed to evict everything and still not fit.
            self.stats.rejected += 1;
            return false;
        }
        let clock = self.tick();
        if let Some(previous) = self.entries.remove(&key) {
            self.account(&previous, -1);
            self.forget_resident(key.generation);
        }
        self.enforce_budget(cost);
        let entry = Entry {
            frame,
            last_used: std::cell::Cell::new(clock),
        };
        self.account(&entry, 1);
        self.entries.insert(key, entry);
        *self.resident.entry(key.generation).or_insert(0) += 1;
        true
    }

    /// Generations with at least one resident frame, newest first, at or
    /// below `generation`. This is the honest fallback order: iterating
    /// every integer generation since application start scans the cache once
    /// per empty generation for nothing.
    pub fn generations_at_or_below(&self, generation: RenderGeneration) -> Vec<RenderGeneration> {
        self.resident
            .range(..=generation)
            .rev()
            .map(|(generation, _)| *generation)
            .collect()
    }

    /// Keys evicted since the last call, so the caller can drop whatever it
    /// derived from those frames — image handles, textures — in step.
    pub fn take_evicted(&mut self) -> Vec<FrameKey> {
        std::mem::take(&mut self.evicted_keys)
    }

    fn forget_resident(&mut self, generation: RenderGeneration) {
        if let Some(count) = self.resident.get_mut(&generation) {
            *count -= 1;
            if *count == 0 {
                self.resident.remove(&generation);
            }
        }
    }

    /// Remove every entry `predicate` selects, and do the bookkeeping an
    /// eviction always needs: the byte account, the eviction counter, the
    /// resident count, and the record kept for whoever is watching what left
    /// the cache. Shared by [`Self::evict_older_than`] and
    /// [`Self::evict_kind`], which differ only in which entries `predicate`
    /// selects.
    fn evict_where(&mut self, predicate: impl Fn(&FrameKey) -> bool) -> usize {
        let doomed: Vec<FrameKey> = self
            .entries
            .keys()
            .filter(|key| predicate(key))
            .copied()
            .collect();
        let count = doomed.len();
        for key in doomed {
            if let Some(entry) = self.entries.remove(&key) {
                self.account(&entry, -1);
                self.stats.evictions += 1;
                self.forget_resident(key.generation);
                self.evicted_keys.push(key);
            }
        }
        count
    }

    /// Discard everything older than `generation`. Called on every accepted
    /// reload, DPI change and mapping change.
    pub fn evict_older_than(&mut self, generation: RenderGeneration) -> usize {
        self.evict_where(|key| key.generation < generation)
    }

    /// Discard every frame of one kind, whatever generation it belongs to.
    ///
    /// For a change that alters what a picture of a page *contains* without
    /// changing the document it came from — the reader's crop is the one such
    /// change — where a generation bump would be a lie: the document has not
    /// been reloaded, and the slides rendered from it are still correct.
    pub fn evict_kind(&mut self, kind: FrameKind) -> usize {
        self.evict_where(|key| key.kind == kind)
    }

    pub fn clear(&mut self) {
        self.evicted_keys.extend(self.entries.keys().copied());
        self.entries.clear();
        self.resident.clear();
        // Stale pins would silently exempt the next frames that happen to
        // reuse these keys from eviction.
        self.pinned.clear();
        self.stats.cpu_bytes = 0;
        self.stats.frames = 0;
        self.stats.pinned_overcommit_bytes = 0;
    }

    fn account(&mut self, entry: &Entry, sign: i64) {
        let cpu = entry.frame.cpu_bytes();
        if sign > 0 {
            self.stats.cpu_bytes += cpu;
            self.stats.frames += 1;
        } else {
            self.stats.cpu_bytes = self.stats.cpu_bytes.saturating_sub(cpu);
            self.stats.frames = self.stats.frames.saturating_sub(1);
        }
    }

    /// Evict least-recently-used unpinned frames until `incoming` bytes fit.
    fn enforce_budget(&mut self, incoming: u64) {
        loop {
            let used = self.stats.total_bytes();
            if used + incoming <= self.budget_bytes {
                self.stats.pinned_overcommit_bytes = 0;
                return;
            }
            let victim = self
                .entries
                .iter()
                .filter(|(key, _)| !self.pinned.contains(key))
                .min_by_key(|(_, entry)| entry.last_used.get())
                .map(|(key, _)| *key);
            let Some(victim) = victim else {
                // Everything left is pinned: the on-screen frames always win,
                // and the overrun is recorded rather than pretended away.
                self.stats.pinned_overcommit_bytes = (used + incoming) - self.budget_bytes;
                return;
            };
            if let Some(entry) = self.entries.remove(&victim) {
                self.account(&entry, -1);
                self.stats.evictions += 1;
                self.forget_resident(victim.generation);
                self.evicted_keys.push(victim);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(generation: u64, slide: usize) -> FrameKey {
        FrameKey {
            generation: RenderGeneration(generation),
            slide,
            kind: FrameKind::Slide,
            width: 1920,
            height: 1080,
        }
    }

    fn frame(bytes: usize) -> Frame {
        Frame {
            width: 1920,
            height: 1080,
            pixels: Arc::new(vec![0u8; bytes]),
        }
    }

    fn sized_key(width: u32) -> FrameKey {
        FrameKey {
            generation: RenderGeneration(1),
            slide: 0,
            kind: FrameKind::Slide,
            width,
            height: width * 9 / 16,
        }
    }

    fn sized_frame(width: u32) -> Frame {
        Frame {
            width,
            height: width * 9 / 16,
            pixels: Arc::new(vec![0u8; 64]),
        }
    }

    #[test]
    fn a_giant_frame_does_not_satisfy_a_panel_request() {
        // The starvation this method exists to prevent: an audience-size
        // frame must not suppress the preview-size render the panels rely
        // on, because the giant is the first thing eviction takes.
        let mut cache = FrameCache::new(1_000_000);
        cache.insert(sized_key(3840), sized_frame(3840));
        let generation = RenderGeneration(1);
        assert!(!cache.satisfies(generation, 0, FrameKind::Slide, 1152));
        // But the same giant satisfies a request for its own size.
        assert!(cache.satisfies(generation, 0, FrameKind::Slide, 3840));
    }

    #[test]
    fn exact_lookup_never_promotes_an_audience_frame_into_a_presenter_slot() {
        let mut cache = FrameCache::new(100_000_000);
        cache.insert(sized_key(1280), sized_frame(1280));
        cache.insert(sized_key(3840), sized_frame(3840));

        let (key, _) = cache
            .best_exact(RenderGeneration(1), 0, FrameKind::Slide, 1280, 720)
            .expect("canonical presenter frame");
        assert_eq!(key.width, 1280);
        assert!(cache
            .best_exact(RenderGeneration(1), 0, FrameKind::Slide, 640, 360)
            .is_none());
    }

    #[test]
    fn a_zoom_crop_and_a_whole_page_are_two_pictures_at_one_width() {
        // The `/FitR` case: same page, same width, different height. Choosing
        // by width alone left the projector's picture to hash order.
        let mut cache = FrameCache::new(100_000_000);
        let whole = sized_key(1280);
        let cropped = FrameKey {
            height: 900,
            ..whole
        };
        cache.insert(whole, sized_frame(1280));
        cache.insert(
            cropped,
            Frame {
                width: 1280,
                height: 900,
                pixels: Arc::new(vec![0u8; 64]),
            },
        );

        let (key, _) = cache
            .best_exact(RenderGeneration(1), 0, FrameKind::Slide, 1280, 900)
            .expect("the cropped frame");
        assert_eq!(key, cropped);
        let (key, _) = cache
            .best_exact(RenderGeneration(1), 0, FrameKind::Slide, 1280, 720)
            .expect("the whole page");
        assert_eq!(key, whole);
    }

    #[test]
    fn a_frame_near_the_requested_width_satisfies_it() {
        let mut cache = FrameCache::new(1_000_000);
        cache.insert(sized_key(2000), sized_frame(2000));
        let generation = RenderGeneration(1);
        // Within [width, 2 × width]: good enough, no re-render.
        assert!(cache.satisfies(generation, 0, FrameKind::Slide, 1152));
        // Undersized never satisfies.
        assert!(!cache.satisfies(generation, 0, FrameKind::Slide, 3840));
    }

    #[test]
    fn accounting_is_in_bytes_not_pages() {
        let mut cache = FrameCache::new(10_000_000);
        cache.insert(key(1, 0), frame(4_000_000));
        assert_eq!(cache.stats().cpu_bytes, 4_000_000);
        assert_eq!(cache.stats().frames, 1);
    }

    #[test]
    fn the_budget_is_never_exceeded() {
        let mut cache = FrameCache::new(10_000_000);
        for slide in 0..20 {
            cache.insert(key(1, slide), frame(2_000_000));
            assert!(
                cache.stats().total_bytes() <= cache.budget_bytes(),
                "budget exceeded at slide {slide}: {} bytes",
                cache.stats().total_bytes()
            );
        }
        assert!(cache.stats().evictions > 0);
    }

    #[test]
    fn pinned_frames_survive_pressure() {
        let mut cache = FrameCache::new(6_000_000);
        let on_screen = key(1, 0);
        cache.insert(on_screen, frame(2_000_000));
        cache.pin(vec![on_screen]);

        for slide in 1..10 {
            cache.insert(key(1, slide), frame(2_000_000));
        }
        assert!(
            cache.get(&on_screen).is_some(),
            "the audience frame must never be evicted"
        );
    }

    #[test]
    fn a_frame_bigger_than_the_budget_is_refused_not_ruinous() {
        let mut cache = FrameCache::new(1_000_000);
        cache.insert(key(1, 0), frame(500_000));
        assert!(!cache.insert(key(1, 1), frame(4_000_000)));
        assert_eq!(cache.stats().rejected, 1);
        assert!(cache.get(&key(1, 0)).is_some(), "existing frames survive");
    }

    #[test]
    fn a_frame_the_views_keep_fetching_counts_as_recently_used() {
        // The views fetch through `best`, never `get`. Before lookups
        // touched recency, "LRU" eviction was insertion order, and the
        // frame on screen the longest was the first to go.
        let mut cache = FrameCache::new(5_000_000);
        let shown = key(1, 0);
        let idle = key(1, 1);
        cache.insert(shown, frame(2_000_000));
        cache.insert(idle, frame(2_000_000));
        // The older entry is the one the view keeps drawing.
        cache.best(RenderGeneration(1), 0, FrameKind::Slide);
        cache.insert(key(1, 2), frame(2_000_000));
        assert!(cache.contains(&shown), "the fetched frame survives");
        assert!(!cache.contains(&idle), "the untouched frame is the victim");
    }

    #[test]
    fn resident_generations_track_what_is_actually_cached() {
        let mut cache = FrameCache::new(100_000_000);
        cache.insert(key(3, 0), frame(1_000_000));
        cache.insert(key(900, 0), frame(1_000_000));
        // Newest first, only generations with entries, capped at the asked
        // generation — a thousand reloads must not mean a thousand probes.
        assert_eq!(
            cache.generations_at_or_below(RenderGeneration(1000)),
            vec![RenderGeneration(900), RenderGeneration(3)]
        );
        assert_eq!(
            cache.generations_at_or_below(RenderGeneration(10)),
            vec![RenderGeneration(3)]
        );
        cache.evict_older_than(RenderGeneration(900));
        assert_eq!(
            cache.generations_at_or_below(RenderGeneration(1000)),
            vec![RenderGeneration(900)]
        );
    }

    /// A crop changes what a picture of a page contains without changing the
    /// document, so the reader's frames go and the deck's stay: a generation
    /// bump would evict both and claim a reload that never happened.
    #[test]
    fn evicting_a_kind_leaves_the_other_kinds_standing() {
        let mut cache = FrameCache::new(100_000_000);
        let page = FrameKey {
            kind: FrameKind::Page,
            ..key(1, 0)
        };
        cache.insert(page, frame(1_000));
        cache.insert(key(1, 0), frame(1_000));
        assert_eq!(cache.evict_kind(FrameKind::Page), 1);
        assert!(!cache.contains(&page));
        assert!(cache.contains(&key(1, 0)));
        assert_eq!(cache.take_evicted(), vec![page]);
    }

    /// The tightest frame at least as wide as the cell, or — only when every
    /// frame is narrower — the widest of those.
    #[test]
    fn best_fitting_takes_the_tightest_frame_that_covers_the_cell() {
        let mut cache = FrameCache::new(100_000_000);
        let sized = |width: u32| FrameKey { width, ..key(1, 0) };
        cache.insert(sized(1920), frame(1_000_000));
        cache.insert(sized(800), frame(1_000_000));
        cache.insert(sized(400), frame(1_000_000));
        let (found, _) = cache
            .best_fitting(RenderGeneration(1), 0, FrameKind::Slide, 600)
            .unwrap();
        assert_eq!(found.width, 800, "covers the cell with the least to spare");
        // Exactly the right size is covered by "at least as wide".
        let (found, _) = cache
            .best_fitting(RenderGeneration(1), 0, FrameKind::Slide, 1920)
            .unwrap();
        assert_eq!(found.width, 1920);
        // Nothing covers it: the widest there is stands in rather than
        // nothing at all.
        let (found, _) = cache
            .best_fitting(RenderGeneration(1), 0, FrameKind::Slide, 4000)
            .unwrap();
        assert_eq!(found.width, 1920, "nothing fits, so the best stands in");
    }

    /// Leaving fullscreen, exactly: a frame rendered for the full screen no
    /// longer fits the cell, and an older small one does. Taking the small
    /// one leaves the page soft — and `satisfies` sees the same wide frame,
    /// decides the page is covered and asks for no replacement, so nothing
    /// ever sharpens it again.
    #[test]
    fn a_wide_frame_that_does_not_fit_beats_a_narrow_one_that_does() {
        let mut cache = FrameCache::new(100_000_000);
        let sized = |width: u32| FrameKey { width, ..key(1, 0) };
        cache.insert(sized(1920), frame(1_000_000));
        cache.insert(sized(400), frame(100_000));
        let (found, _) = cache
            .best_fitting(RenderGeneration(1), 0, FrameKind::Slide, 1200)
            .unwrap();
        assert_eq!(found.width, 1920, "downsampling beats upsampling");
        // The two must agree: the frame `best_fitting` draws is the frame
        // `satisfies` is counting on, or the page never sharpens.
        assert!(cache.satisfies(RenderGeneration(1), 0, FrameKind::Slide, 1200));
    }

    #[test]
    fn pinned_overcommit_is_reported_not_hidden() {
        let mut cache = FrameCache::new(3_000_000);
        let first = key(1, 0);
        let second = key(1, 1);
        cache.insert(first, frame(2_000_000));
        cache.pin(vec![first, second]);
        cache.insert(second, frame(2_000_000));
        assert!(cache.stats().total_bytes() > cache.budget_bytes());
        assert_eq!(cache.stats().pinned_overcommit_bytes, 1_000_000);
        // Pressure that can be relieved clears the overcommit report.
        cache.pin(vec![second]);
        cache.insert(key(1, 2), frame(500_000));
        assert_eq!(cache.stats().pinned_overcommit_bytes, 0);
    }

    #[test]
    fn clearing_forgets_pins_and_reports_the_evicted_keys() {
        let mut cache = FrameCache::new(10_000_000);
        let pinned = key(1, 0);
        cache.insert(pinned, frame(1_000_000));
        cache.pin(vec![pinned]);
        cache.clear();
        assert_eq!(cache.take_evicted(), vec![pinned]);
        // A stale pin would exempt the next frame reusing this key from
        // eviction for ever.
        cache.insert(pinned, frame(8_000_000));
        cache.insert(key(1, 1), frame(8_000_000));
        assert!(cache.stats().total_bytes() <= cache.budget_bytes());
    }

    #[test]
    fn eviction_reports_the_keys_so_derived_handles_can_follow() {
        let mut cache = FrameCache::new(4_000_000);
        let old = key(1, 0);
        cache.insert(old, frame(3_000_000));
        cache.insert(key(1, 1), frame(3_000_000));
        assert_eq!(cache.take_evicted(), vec![old]);
        assert!(cache.take_evicted().is_empty(), "drained on read");
    }

    #[test]
    fn stale_generations_are_dropped_wholesale() {
        let mut cache = FrameCache::new(100_000_000);
        for slide in 0..5 {
            cache.insert(key(1, slide), frame(1_000_000));
            cache.insert(key(2, slide), frame(1_000_000));
        }
        assert_eq!(cache.evict_older_than(RenderGeneration(2)), 5);
        assert!(cache.get(&key(1, 0)).is_none());
        assert!(cache.get(&key(2, 0)).is_some());
        assert_eq!(cache.stats().cpu_bytes, 5_000_000);
    }

    #[test]
    fn the_best_available_frame_is_the_widest_there_is() {
        let mut cache = FrameCache::new(100_000_000);
        let small = FrameKey {
            width: 480,
            height: 270,
            ..key(3, 7)
        };
        cache.insert(small, Frame::new(480, 270, vec![0; 480 * 270 * 4]));
        let (found, _) = cache
            .best(RenderGeneration(3), 7, FrameKind::Slide)
            .unwrap();
        assert_eq!(
            found, small,
            "a narrow frame is shown until something better exists"
        );

        let wide = key(3, 7);
        cache.insert(wide, frame(1920 * 1080 * 4));
        let (found, _) = cache
            .best(RenderGeneration(3), 7, FrameKind::Slide)
            .unwrap();
        assert_eq!(found, wide);

        assert!(
            cache
                .best(RenderGeneration(4), 7, FrameKind::Slide)
                .is_none(),
            "frames never leak across generations"
        );
    }

    #[test]
    fn a_long_presentation_stays_within_budget() {
        // 300 slides at 4K, plus the panel-sized frame of each, on a 256 MiB
        // budget.
        let mut cache = FrameCache::new(DEFAULT_BUDGET_BYTES);
        for slide in 0..300 {
            let panel = FrameKey {
                width: 640,
                height: 360,
                ..key(1, slide)
            };
            cache.insert(panel, Frame::new(640, 360, vec![0; 640 * 360 * 4]));
            let audience = FrameKey {
                width: 3840,
                height: 2160,
                ..key(1, slide)
            };
            cache.insert(audience, Frame::new(3840, 2160, vec![0; 3840 * 2160 * 4]));
            assert!(cache.stats().total_bytes() <= DEFAULT_BUDGET_BYTES);
        }
        assert!(
            cache.stats().frames > 0,
            "the cache is still useful, just bounded"
        );
    }
}
