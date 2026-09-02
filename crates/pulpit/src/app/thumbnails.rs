//! Deck thumbnails (§79.4): the warming plan ([`App::plan_thumbnails`]),
//! the trickle that submits render requests for it
//! ([`App::pump_thumbnails`]), and the pure helpers that decide a width
//! and an order ([`fitting_thumbnail_width`], [`warming_order`]).
//!
//! The eight thumbnail fields (`thumbnails`, `thumbnail_queue`,
//! `thumbnails_demanded`, `thumbnail_requests`, `thumbnail_plan`,
//! `thumbnail_plan_width`, `thumbnail_plan_inputs`, plus
//! `pending_auto_crop`) stay on `App` in app.rs, the same shape as
//! `app::print`, `app::overview` and `app::search`.

use iced::Task;

use pulpit_render::cache::{FrameKey, FrameKind};
use pulpit_render::protocol::{Priority, RenderJob};

use super::{App, Message};

/// How wide a page is rendered for the overview grid, the slider's preview
/// card, and the panels' stand-in while a real frame renders. One width, one
/// pass: a page is rendered once and its picture never changes for the life
/// of the document, so nothing downstream ever swaps a thumbnail texture.
/// Sharp enough for the preview card; a deck warms in a few seconds.
pub const THUMBNAIL_WIDTH: u32 = 480;

/// The narrowest a thumbnail is ever rendered.
///
/// Below this a page is a grey smudge rather than a picture of anything, so a
/// deck long enough to need it is one whose furthest pages the budget cannot
/// hold at any useful size. It gets the floor, and [`ThumbnailCache::trim`]
/// keeps the pages nearest the presenter.
pub const THUMBNAIL_MIN_WIDTH: u32 = 120;

/// What the whole deck's thumbnails may occupy — and, through
/// [`fitting_thumbnail_width`], the raw-pixel figure the warming width is
/// chosen against. The store now holds each page *encoded*, measured
/// seventeen times smaller than the pixels it was rendered from, so a warmed
/// deck actually occupies a few megabytes and the budget is a backstop for
/// pathological pages rather than a ceiling anyone reaches. The width
/// formula deliberately still reasons in raw pixels: it keeps today's widths
/// — the sizes every session has been judged at — rather than spending the
/// compression on sharper pictures nobody has measured the cost of.
/// Separate from the frame cache so the two can never evict each other.
pub(super) const THUMBNAIL_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

/// How many thumbnails may be outstanding (queued or rendering) at once.
///
/// This is the warming throughput throttle. Renderer events are drained once
/// per tick, so a warming pass completes at most this many pages per tick
/// however fast the workers are: 32 per 50 ms tick is ~640 pages a second —
/// a screenful of the overview refills in a single tick, and a 700-page deck
/// warms in about a second — while staying well short of saturating the
/// machine during a talk. Small enough, still, that a document swap does not
/// leave a long tail of stale requests to cancel.
const THUMBNAILS_OUTSTANDING: usize = 32;

/// Everything the thumbnail plan is a function of: the render generation, the
/// slide count, the presenter's page, the page warming is working outwards
/// from, how many pictures are held, and how many are still wanted. If none of
/// it moved, the plan cannot have moved either.
pub(super) type ThumbnailPlanInputs = (
    pulpit_core::RenderGeneration,
    usize,
    usize,
    usize,
    usize,
    usize,
);

/// The widest a page can be rendered and still leave room for every other
/// page in the deck.
///
/// The budget holds `count` pictures of `width × width/aspect × 4` bytes, so
/// the width that exactly spends it is the square root of
/// `budget × aspect / (4 × count)`. Rounded down to a multiple of eight,
/// because a texture width that is a round number of pixels is kinder to
/// every stage below this one, and clamped: never sharper than
/// [`THUMBNAIL_WIDTH`], which is all the grid can show, and never narrower
/// than [`THUMBNAIL_MIN_WIDTH`], below which there is nothing to look at.
///
/// A deck long enough to hit the floor is one the budget genuinely cannot
/// hold, and it is the only case where a thumbnail is evicted at all.
fn fitting_thumbnail_width(count: usize, aspect: f32, budget: u64) -> u32 {
    if count == 0 {
        return THUMBNAIL_WIDTH;
    }
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let exact = (budget as f64 * aspect as f64 / (4.0 * count as f64)).sqrt();
    if !exact.is_finite() {
        return THUMBNAIL_WIDTH;
    }
    let rounded = ((exact as u32) / 8) * 8;
    rounded.clamp(THUMBNAIL_MIN_WIDTH, THUMBNAIL_WIDTH)
}

fn warming_order(
    queue: &std::collections::VecDeque<(usize, u32)>,
    count: usize,
    here: usize,
    have: &crate::thumbnails::ThumbnailCache,
) -> std::collections::VecDeque<(usize, u32)> {
    let mut order: Vec<(usize, u32)> = queue
        .iter()
        .copied()
        .filter(|(slide, width)| *slide < count && !have.has_at_least(*slide, *width))
        .collect();
    order.sort_by_key(|(slide, _)| slide.abs_diff(here));
    order.into()
}

impl App {
    // ------------------------------------------------------------ thumbnails

    /// The page warming should work outwards from.
    ///
    /// Normally that is the presenter's own position: the pages they are
    /// about to reach are the pages they are about to want. But while the
    /// overview is open the grid *is* the presenter's screen, and they can
    /// scroll it a long way from where they are standing — open the grid on
    /// page twelve, scroll to page two hundred, and warming from `preview()`
    /// would spend the renderer on pages thirteen and fourteen while the
    /// pages under the eye stay blank. So when the grid is open and has been
    /// laid out, the centre is the middle of what is on screen.
    pub(super) fn warming_centre(&self) -> usize {
        let count = self.state.slide_count();
        if !self.overview || count == 0 {
            return self.state.preview();
        }
        super::overview::visible_centre(self.overview_scroll, self.overview_grid.get(), count)
            .unwrap_or_else(|| self.state.preview())
    }

    /// Decide which pages still want a thumbnail, nearest first.
    ///
    /// Rebuilt when the document changes, and re-ordered as the presenter
    /// moves, so what arrives next is what they are most likely to look at.
    /// A whole deck's worth of `usize` is nothing; it is the *rendering* that
    /// is expensive, and that is what the ordering is protecting.
    /// Whether warming should run at all.
    ///
    /// Asked of the session: a layout with slide panels draws thumbnails as
    /// the panels' stand-ins, and a running presentation must have the grid
    /// ready mid-sentence, so both warm eagerly. A plain reader warms only
    /// once somebody navigates by picture — the grid fills from where they
    /// are at a screenful in a couple of hundred milliseconds, which is the
    /// same trade the rest of this file makes: pay on demand, not on spec.
    fn thumbnails_wanted(&self) -> bool {
        if self.thumbnails_demanded || self.audience_started {
            return true;
        }
        let demand = crate::layout::panels::demand(&self.active_layout);
        demand.current || demand.neighbour
    }

    /// Apply the pending automatic margin crop once the measurements are in.
    ///
    /// "In" means the warming queue has drained with nothing outstanding:
    /// every page that will get a picture has one, so the union taken is of
    /// the whole deck — or of the bounded sample the budget allows on a very
    /// long one — rather than of whichever pages happened to land first. A
    /// crop that tightened page by page would move under the reader.
    ///
    /// Called from the press itself, for the deck whose pictures are already
    /// warm, and from the tick, which is what carries a press made while
    /// warming was still running.
    pub(super) fn try_apply_auto_crop(&mut self) -> Task<Message> {
        let Some(generation) = self.pending_auto_crop else {
            return Task::none();
        };
        if generation != self.state.generation() {
            // The press was about a document that is gone.
            self.pending_auto_crop = None;
            return Task::none();
        }
        if !self.thumbnail_queue.is_empty() || !self.thumbnail_requests.is_empty() {
            // Still measuring; the tick asks again.
            return Task::none();
        }
        self.pending_auto_crop = None;
        let Some(region) = crate::autocrop::combined_crop(&self.thumbnails.margins()) else {
            // Honest rather than silent: the latch did not light and the
            // reader should hear why — the pages are already tight, a page
            // is full-bleed and may not lose ink, or nothing could be
            // measured at all.
            self.notify("No margins worth cropping were found.".to_string());
            return Task::none();
        };
        if !self.reader.auto_crop(region) {
            return Task::none();
        }
        self.request_reader_renders();
        self.scroll_surface_to_reader()
    }

    pub(super) fn plan_thumbnails(&mut self) {
        if !self.thumbnails_wanted() {
            return;
        }

        let generation = self.state.generation();
        let count = self.state.slide_count();
        // This runs on every 50 ms tick, but the plan only changes when one
        // of its inputs does: the document, the presenter's position, or a
        // thumbnail landing. On the vast majority of ticks nothing moved and
        // rebuilding, re-filtering and re-sorting the queue — hundreds of
        // lookups and a sort on a long deck — produced the identical result.
        let centre = self.warming_centre();
        let inputs = (
            generation,
            count,
            self.state.preview(),
            centre,
            self.thumbnails.len(),
            self.thumbnail_queue.len(),
        );
        if self.thumbnail_plan_inputs == Some(inputs) {
            return;
        }
        self.thumbnail_plan_inputs = Some(inputs);
        // Both halves matter. The generation changes when the document is
        // replaced or re-read, and the count when a document finishes opening
        // — which for the first document of the session happens *without* a
        // generation change, so planning on the generation alone would leave
        // the very first deck unwarmed.
        if self.thumbnail_plan != Some((generation, count)) {
            if self.thumbnails.generation() != generation {
                self.thumbnails.reset(generation);
            }
            self.thumbnail_plan = Some((generation, count));
            // One pass at one width, decided up front and chosen so the whole
            // deck fits the budget: the upgrade pass this replaces re-rendered
            // pages the grid was already showing, and every upgrade was a
            // texture swap — a visible blink — in whatever panel was standing
            // in on that thumbnail at that moment.
            //
            // The width has to be *computed* rather than picked from a pair of
            // constants. A six-hundred-page book of portrait pages overflows
            // the budget at any fixed coarse width too, and what overflow
            // means here is not a coarser grid: it is eviction, and an evicted
            // page is one nothing ever asks for again, so its cell in the grid
            // stays empty for the life of the session.
            let aspect = self
                .state
                .first_page_size()
                .map(|size| size.aspect_ratio())
                .unwrap_or(16.0 / 9.0);
            let width = fitting_thumbnail_width(count, aspect, self.thumbnails.budget_bytes());
            self.thumbnail_plan_width = width;
            self.thumbnail_queue = (0..count)
                .filter(|s| !self.thumbnails.contains(*s))
                .map(|s| (s, width))
                .collect();
        }
        // A page can go missing after the one pass has been through it: a
        // render that failed or was cancelled frees its slot without leaving
        // a picture, and a deck too long for the budget even at the floor
        // width has its furthest pages evicted. Either way nothing above
        // would ever ask again, and the grid keeps an empty cell for the rest
        // of the session.
        //
        // So the pages around the presenter are swept once the pass has
        // drained. Bounded to a window the budget can certainly hold, which
        // is what stops a deck that overflows from chasing its own tail:
        // re-requesting the far end would only evict the near end that the
        // presenter is looking at, and then re-request that.
        if self.thumbnail_queue.is_empty() && self.thumbnail_requests.is_empty() {
            let width = self.thumbnail_plan_width;
            // The same height the render request will ask for, so the
            // estimate is of the pictures actually being made.
            let aspect = self
                .state
                .first_page_size()
                .map(|size| size.aspect_ratio())
                .unwrap_or(16.0 / 9.0);
            let height = (width as f32 / aspect).max(1.0) as u32;
            let reach = self.thumbnails.capacity_at(width, height, count).max(1) / 2;
            let first = centre.saturating_sub(reach);
            let last = centre.saturating_add(reach).min(count.saturating_sub(1));
            self.thumbnail_queue = (first..=last)
                .filter(|s| !self.thumbnails.has_at_least(*s, width))
                .map(|s| (s, width))
                .collect();
        }
        if self.thumbnail_queue.is_empty() {
            return;
        }
        self.thumbnail_queue =
            warming_order(&self.thumbnail_queue, count, centre, &self.thumbnails);
    }

    /// Submit the next thumbnail or two, if the renderer has room.
    ///
    /// Warming happens from the moment a document opens rather than when the
    /// overview is asked for, because the whole point is that pressing the
    /// key shows a finished grid. It is deliberately a trickle: the queue is
    /// only fed when nothing more important is waiting, so a deck warming in
    /// the background cannot delay a page turn.
    pub(super) fn pump_thumbnails(&mut self) {
        let Some(document) = self.state.document().map(|d| d.id.0) else {
            return;
        };
        if self.thumbnail_queue.is_empty() {
            return;
        }
        let generation = self.state.generation();
        let aspect = self
            .state
            .first_page_size()
            .map(|size| size.aspect_ratio())
            .unwrap_or(16.0 / 9.0);
        // Keeping several outstanding is safe, and is what lets a long deck
        // warm at the renderer's pace rather than one page per tick. It does
        // not cost the presenter anything, because the renderer dispatches by
        // priority: an audience frame submitted a moment later is picked
        // before any of these, and the only wait it can suffer is for a
        // thumbnail already *in* a worker — one small page, tens of
        // milliseconds, behind a window that is still showing its last frame.
        if self.thumbnail_requests.len() >= THUMBNAILS_OUTSTANDING {
            return;
        }
        let mut room = THUMBNAILS_OUTSTANDING - self.thumbnail_requests.len();
        // Warming is ancillary work — until the grid is open, at which point
        // these thumbnails are not warming for later, they are the thing the
        // presenter is looking at right now. They stay below the audience and
        // the presenter's own page, which must never wait behind a grid.
        let priority = if self.overview {
            Priority::Adjacent
        } else {
            Priority::Ancillary
        };

        while room > 0 {
            let Some((slide, width)) = self.thumbnail_queue.pop_front() else {
                return;
            };
            if self.thumbnails.has_at_least(slide, width) {
                continue;
            }
            let height = (width as f32 / aspect).max(1.0) as u32;
            let key = FrameKey {
                generation,
                slide,
                kind: FrameKind::Slide,
                width,
                height,
            };
            if self.pending.iter().any(|(_, pending)| *pending == key) {
                continue;
            }
            let Some(source) = self
                .state
                .mapping()
                .audience_source(slide, self.state.pdf_pages())
            else {
                continue;
            };
            let Some(supervisor) = self.supervisor.as_mut() else {
                return;
            };
            let id = super::submit_render(
                supervisor,
                &mut self.pending,
                &mut self.submitted_at,
                Some(key),
                |id| RenderJob {
                    id,
                    generation,
                    document,
                    page: source.pdf_page,
                    region: source.region,
                    width,
                    height,
                    priority,
                    with_annotations: false,
                    region_name: String::new(),
                },
            );
            self.thumbnail_requests.insert(id);
            room -= 1;
        }
    }
}

/// The cache's pixel allocation, wrapped so an iced image handle can share
/// it. `Handle::from_rgba` holds `bytes::Bytes`; built from an owner, the
/// handle and the frame cache reference the *same* allocation, where a
/// `Vec` clone both copied the frame and doubled its residency for as long
/// as the handle lived.
/// A thumbnail's handle, holding encoded bytes rather than pixels, and what
/// those bytes weigh for the cache's accounting. A page that will not encode
/// — it cannot happen for an opaque rendered page, but the encoder's error
/// type says it can — is kept raw rather than lost.
pub(super) fn encoded_thumbnail(
    frame: &pulpit_render::cache::Frame,
) -> (iced::widget::image::Handle, u64) {
    let encoded = image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels.to_vec())
        .map(|img| image::DynamicImage::ImageRgba8(img).to_rgb8())
        .and_then(|rgb| {
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
                .encode_image(&rgb)
                .ok()
                .map(|()| out)
        });
    match encoded {
        Some(bytes) => {
            let held = bytes.len() as u64;
            (iced::widget::image::Handle::from_bytes(bytes), held)
        }
        None => (
            iced::widget::image::Handle::from_rgba(
                frame.width,
                frame.height,
                super::shared_pixels(&frame.pixels),
            ),
            frame.pixels.len() as u64,
        ),
    }
}

#[cfg(test)]
mod warming_tests {
    use super::warming_order;
    use crate::app::overview::visible_centre;
    use crate::app::OverviewGrid;
    use crate::thumbnails::ThumbnailCache;
    use std::collections::VecDeque;

    fn grid(columns: usize, rows_on_screen: f32) -> OverviewGrid {
        OverviewGrid {
            columns,
            row_height: 100.0,
            viewport_height: rows_on_screen * 100.0,
        }
    }

    #[test]
    fn an_unlaid_grid_has_no_centre() {
        assert_eq!(visible_centre(0.0, OverviewGrid::default(), 100), None);
        assert_eq!(visible_centre(0.0, grid(4, 3.0), 0), None);
    }

    #[test]
    fn the_centre_is_the_middle_of_what_is_on_screen() {
        // Four columns, three rows on screen, scrolled to row 50: rows 50-52
        // are showing, so the middle row is 51 and the middle of it is 51*4+2.
        assert_eq!(visible_centre(5000.0, grid(4, 3.0), 400), Some(51 * 4 + 2));
        // Unscrolled, the centre is in the first screenful, not at page zero.
        assert_eq!(visible_centre(0.0, grid(4, 3.0), 400), Some(4 + 2));
    }

    #[test]
    fn the_centre_stays_inside_a_short_deck() {
        // A grid scrolled past a deck that does not fill it must still name a
        // page that exists.
        assert_eq!(visible_centre(9000.0, grid(4, 3.0), 10), Some(9));
    }

    #[test]
    fn warming_follows_the_grid_rather_than_the_presenter() {
        // The presenter is on page 12 and has scrolled the grid to page 200:
        // what fills first is what they are looking at.
        let centre = visible_centre(5000.0, grid(4, 3.0), 400).unwrap();
        let order = warming_order(&coarse(0..400), 400, centre, &cache());
        for (slide, _) in order.iter().take(4) {
            assert!(
                slide.abs_diff(centre) <= 2,
                "{slide} is not near the rows on screen"
            );
        }
    }

    fn cache() -> ThumbnailCache {
        ThumbnailCache::new(1024 * 1024)
    }

    fn coarse(range: std::ops::Range<usize>) -> VecDeque<(usize, u32)> {
        range.map(|slide| (slide, super::THUMBNAIL_WIDTH)).collect()
    }

    #[test]
    fn the_nearest_pages_are_warmed_first() {
        let order = warming_order(&coarse(0..100), 100, 50, &cache());

        assert_eq!(
            order.front().map(|(slide, _)| *slide),
            Some(50),
            "the page in hand"
        );
        for (slide, _) in order.iter().take(5) {
            assert!(
                slide.abs_diff(50) <= 2,
                "{slide} is further than the first five should reach"
            );
        }
        assert_eq!(order.len(), 100, "and every page is still wanted");
    }

    #[test]
    fn pages_already_held_are_not_asked_for_again() {
        let mut have = cache();
        have.insert(
            3,
            iced::widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
            10,
            super::THUMBNAIL_WIDTH,
            3,
        );

        let order = warming_order(&coarse(0..10), 10, 0, &have);

        assert!(!order.iter().any(|(slide, _)| *slide == 3));
        assert_eq!(order.len(), 9);
    }

    #[test]
    fn a_coarse_picture_does_not_satisfy_a_wider_request() {
        // Warming is one pass at one width now, but a reload can lower a
        // giant deck to a narrower width and a later reload restore it:
        // a narrower picture must still count as missing.
        let mut have = cache();
        have.insert(
            3,
            iced::widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
            10,
            super::THUMBNAIL_MIN_WIDTH,
            3,
        );
        let queue: VecDeque<(usize, u32)> = [(3, super::THUMBNAIL_WIDTH)].into();

        let order = warming_order(&queue, 10, 0, &have);

        assert_eq!(
            order.len(),
            1,
            "the page is still wanted, at the wider width"
        );
    }

    /// The whole deck must fit the budget at the width warming chooses.
    /// Anything that does not fit is evicted, and nothing ever asks for an
    /// evicted page again — which is a grid with permanent holes in it.
    #[test]
    fn every_page_of_a_long_book_fits_the_budget_at_the_chosen_width() {
        use super::{fitting_thumbnail_width, THUMBNAIL_BUDGET_BYTES, THUMBNAIL_MIN_WIDTH};

        // A real one: 655 portrait pages, 439.42 × 683.15 points. At the two
        // fixed widths this replaced — 480 and 240 — this deck needed 938 MB
        // and 234 MB against a 128 MB budget, so roughly half of it was
        // evicted as fast as it was rendered and the grid never filled in.
        for (count, aspect) in [
            (655usize, 439.42f32 / 683.15),
            (120, 16.0 / 9.0),
            (1, 16.0 / 9.0),
            (2_000, 0.7),
        ] {
            let width = fitting_thumbnail_width(count, aspect, THUMBNAIL_BUDGET_BYTES);
            if width == THUMBNAIL_MIN_WIDTH {
                // The floor: a deck this long is one the budget cannot hold
                // at any width worth looking at, and eviction is the answer.
                continue;
            }
            let height = (width as f64 / aspect as f64).max(1.0) as u64;
            let total = width as u64 * height * 4 * count as u64;
            assert!(
                total <= THUMBNAIL_BUDGET_BYTES,
                "{count} pages at {width}px need {} MiB of a {} MiB budget",
                total / (1024 * 1024),
                THUMBNAIL_BUDGET_BYTES / (1024 * 1024),
            );
        }
    }

    /// A short deck is not punished for the long ones: it still gets the
    /// sharp width the grid is designed around.
    #[test]
    fn an_ordinary_deck_still_warms_at_the_sharp_width() {
        use super::{fitting_thumbnail_width, THUMBNAIL_BUDGET_BYTES, THUMBNAIL_WIDTH};

        assert_eq!(
            fitting_thumbnail_width(120, 16.0 / 9.0, THUMBNAIL_BUDGET_BYTES),
            THUMBNAIL_WIDTH
        );
        // A degenerate document cannot produce a nonsense width.
        assert_eq!(
            fitting_thumbnail_width(0, 16.0 / 9.0, THUMBNAIL_BUDGET_BYTES),
            THUMBNAIL_WIDTH
        );
        assert_eq!(
            fitting_thumbnail_width(10, f32::NAN, THUMBNAIL_BUDGET_BYTES),
            THUMBNAIL_WIDTH
        );
    }

    #[test]
    fn a_shorter_document_drops_the_pages_it_no_longer_has() {
        // A reload of a deck that lost its last twenty pages must not leave
        // requests for pages that cannot be rendered.
        let order = warming_order(&coarse(0..30), 10, 0, &cache());

        assert_eq!(order.len(), 10);
        assert!(order.iter().all(|(slide, _)| *slide < 10));
    }
}
