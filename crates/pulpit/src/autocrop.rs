//! Measuring where the ink is, so the margins can be cropped without a hand
//! drawing the rectangle.
//!
//! Two pure functions: [`ink_bounds`] reads one rendered page and answers
//! where its content sits, and [`combined_crop`] turns a deck's worth of
//! answers into the one window every page can be read through. Both speak in
//! fractions of the page, for the same reason the marquee crop does: one
//! window is right for a letter page and the A4 appendix behind it.
//!
//! The measurement is taken from pixels rather than from the PDF's object
//! bounds, because pixels cannot lie about what is visible: a full-page
//! white rectangle, a text layer set invisible and a watermark clipped to
//! nothing all have bounds and no ink. The pixels used are the deck's
//! thumbnails — rendered anyway, small enough that scanning one costs
//! microseconds, and exactly what the viewer sees.

use pulpit_core::notes::Region;

/// How different from the background a pixel must be to count as ink,
/// summed over the three colour channels.
///
/// High enough that a slow background gradient does not read as content,
/// low enough that a grey caption or a pale figure does. Anti-aliased text
/// clears it by an order of magnitude.
const INK_TOLERANCE: u32 = 48;

/// A margin kept on every side after the crop, as a fraction of the page.
///
/// Text set hard against the edge of a window reads as cut off even when it
/// is not, so a sliver of each measured margin is given back.
const BREATHING_MARGIN: f32 = 0.01;

/// The least a crop must win, summed across one axis, to be worth applying.
///
/// Below this the pages are already tight, and what the "crop" would buy is
/// a re-render of every page that changes nothing anyone can see.
const MIN_GAIN: f32 = 0.05;

/// How many inked pixels a row or column needs before it counts as content
/// rather than noise, given how long the line is.
///
/// A fleck of dust on a scan is one or two pixels; a line of text is
/// hundreds. Two consecutive qualifying lines are also required — see
/// [`first_content`] — so a stray horizontal scratch does not hold a margin
/// open either.
fn noise_floor(extent: usize) -> u32 {
    (extent / 128).max(2) as u32
}

/// Where the content of one rendered page sits, as fractions of the page.
///
/// `None` means the page had nothing to measure: it is blank, too small to
/// judge, or the buffer is not the RGBA frame it claims to be. A blank page
/// says nothing about the deck's margins rather than holding them open.
pub fn ink_bounds(width: u32, height: u32, rgba: &[u8]) -> Option<Region> {
    let (w, h) = (width as usize, height as usize);
    if w < 4 || h < 4 || rgba.len() != w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let background = background_colour(w, h, rgba);

    // One pass over the pixels: how much ink each row and each column holds.
    let mut rows = vec![0u32; h];
    let mut cols = vec![0u32; w];
    for y in 0..h {
        let row = &rgba[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let px = &row[x * 4..x * 4 + 3];
            let distance = px[0].abs_diff(background[0]) as u32
                + px[1].abs_diff(background[1]) as u32
                + px[2].abs_diff(background[2]) as u32;
            if distance > INK_TOLERANCE {
                rows[y] += 1;
                cols[x] += 1;
            }
        }
    }

    let top = first_content(&rows, noise_floor(w))?;
    let bottom = last_content(&rows, noise_floor(w))?;
    let left = first_content(&cols, noise_floor(h))?;
    let right = last_content(&cols, noise_floor(h))?;
    Some(Region::new(
        left as f32 / w as f32,
        top as f32 / h as f32,
        (right - left + 1) as f32 / w as f32,
        (bottom - top + 1) as f32 / h as f32,
    ))
}

/// The page's background, read as the commonest colour on its border.
///
/// Read rather than assumed, because "margins are white" is only true of
/// papers: a dark-themed deck has black margins, and a sepia scan has
/// neither. Quantised to 4 bits per channel so anti-aliasing at the border
/// does not split one background into many, then averaged within the
/// winning bucket so the comparison colour is the background itself rather
/// than the bucket's corner.
fn background_colour(w: usize, h: usize, rgba: &[u8]) -> [u8; 3] {
    let mut buckets: std::collections::HashMap<u16, (u32, [u64; 3])> =
        std::collections::HashMap::new();
    let mut tally = |x: usize, y: usize| {
        let px = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
        let key = ((px[0] as u16 >> 4) << 8) | ((px[1] as u16 >> 4) << 4) | (px[2] as u16 >> 4);
        let (count, sums) = buckets.entry(key).or_default();
        *count += 1;
        sums[0] += px[0] as u64;
        sums[1] += px[1] as u64;
        sums[2] += px[2] as u64;
    };
    for x in 0..w {
        tally(x, 0);
        tally(x, h - 1);
    }
    for y in 0..h {
        tally(0, y);
        tally(w - 1, y);
    }
    let (count, sums) = buckets
        .into_values()
        .max_by_key(|(count, _)| *count)
        .unwrap_or((1, [255, 255, 255]));
    [
        (sums[0] / count as u64) as u8,
        (sums[1] / count as u64) as u8,
        (sums[2] / count as u64) as u8,
    ]
}

/// The first line that holds content, where "holds content" takes two
/// consecutive lines at or above the floor: a lone speck row is noise, two
/// in a row is the top of something.
fn first_content(profile: &[u32], floor: u32) -> Option<usize> {
    profile
        .windows(2)
        .position(|pair| pair[0] >= floor && pair[1] >= floor)
}

/// The last such line, by the same two-line rule from the other end.
fn last_content(profile: &[u32], floor: u32) -> Option<usize> {
    profile
        .windows(2)
        .rposition(|pair| pair[0] >= floor && pair[1] >= floor)
        .map(|index| index + 1)
}

/// A side's margin below this, on every side at once, is a full-bleed page:
/// artwork to its edges, the shape of a book's cover.
const FULL_BLEED_MARGIN: f32 = 0.02;

/// How many measured pages must remain after the covers are set aside for
/// "the rest of the deck" to mean anything. A two-page flyer has no
/// interior; its "cover" keeps its veto.
const MIN_INTERIOR: usize = 3;

/// The one window a deck of measured pages can all be read through.
///
/// A union first: each side's margin is the *smallest* measured on any
/// page, so no page loses ink. When that union comes to nothing, the first
/// and last measured pages are reconsidered — books wear full-bleed covers,
/// and a cover that vetoes the crop defeats the feature on exactly the
/// documents that want it most. A cover is set aside only when it measures
/// full-bleed on every side, and only when at least [`MIN_INTERIOR`] other
/// pages remain to agree on real margins; a full-bleed page *inside* the
/// document is a figure, not a cover, and keeps its veto.
///
/// The trade is made knowingly: the window applies to every page, covers
/// included, so a set-aside cover is trimmed at its edges while the crop is
/// on — the same thing the hand-drawn crop-every-page does to it. No
/// *interior* page ever loses ink.
///
/// `bounds` must be in page order — the first and last elements are what
/// "cover" and "back page" mean here. [`BREATHING_MARGIN`] is given back on
/// every side of the result, and a crop that wins less than [`MIN_GAIN`] on
/// both axes is declined as not worth re-rendering the deck for.
///
/// `None` means there is nothing worth cropping: no measurements, margins
/// already tight, or full-bleed pages the rule above may not set aside.
pub fn combined_crop(bounds: &[Region]) -> Option<Region> {
    if let Some(crop) = union_crop(bounds) {
        return Some(crop);
    }
    let mut interior = bounds;
    if interior.first().is_some_and(is_full_bleed) {
        interior = &interior[1..];
    }
    if interior.last().is_some_and(is_full_bleed) {
        interior = &interior[..interior.len() - 1];
    }
    if interior.len() == bounds.len() || interior.len() < MIN_INTERIOR {
        return None;
    }
    union_crop(interior)
}

/// Does this page's ink run to every edge — the measured shape of a cover?
fn is_full_bleed(region: &Region) -> bool {
    region.x < FULL_BLEED_MARGIN
        && region.y < FULL_BLEED_MARGIN
        && 1.0 - region.x - region.width < FULL_BLEED_MARGIN
        && 1.0 - region.y - region.height < FULL_BLEED_MARGIN
}

/// The strict union of `bounds`, padded and gain-checked as
/// [`combined_crop`] describes, with no page set aside.
fn union_crop(bounds: &[Region]) -> Option<Region> {
    if bounds.is_empty() {
        return None;
    }
    let (mut left, mut top, mut right, mut bottom) = (1.0f32, 1.0f32, 1.0f32, 1.0f32);
    for region in bounds {
        left = left.min(region.x.max(0.0));
        top = top.min(region.y.max(0.0));
        right = right.min((1.0 - region.x - region.width).max(0.0));
        bottom = bottom.min((1.0 - region.y - region.height).max(0.0));
    }
    let trim = |margin: f32| (margin - BREATHING_MARGIN).max(0.0);
    let (left, top, right, bottom) = (trim(left), trim(top), trim(right), trim(bottom));
    if left + right < MIN_GAIN && top + bottom < MIN_GAIN {
        return None;
    }
    Some(Region::new(
        left,
        top,
        1.0 - left - right,
        1.0 - top - bottom,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of `background`, with solid rectangles of `ink` painted on it.
    /// Rectangles are (x, y, width, height) in pixels.
    fn page(
        w: usize,
        h: usize,
        background: [u8; 3],
        ink: [u8; 3],
        marks: &[(usize, usize, usize, usize)],
    ) -> Vec<u8> {
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let at = (y * w + x) * 4;
                let inked = marks
                    .iter()
                    .any(|&(mx, my, mw, mh)| x >= mx && x < mx + mw && y >= my && y < my + mh);
                let colour = if inked { ink } else { background };
                rgba[at..at + 3].copy_from_slice(&colour);
                rgba[at + 3] = 255;
            }
        }
        rgba
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.03
    }

    #[test]
    fn a_block_of_ink_is_found_where_it_is() {
        let rgba = page(200, 100, [255; 3], [0; 3], &[(50, 20, 100, 40)]);
        let bounds = ink_bounds(200, 100, &rgba).expect("a page with ink has bounds");
        assert!(close(bounds.x, 0.25), "left at {}", bounds.x);
        assert!(close(bounds.y, 0.20), "top at {}", bounds.y);
        assert!(close(bounds.width, 0.50), "width {}", bounds.width);
        assert!(close(bounds.height, 0.40), "height {}", bounds.height);
    }

    #[test]
    fn a_dark_deck_measures_the_same_as_a_paper() {
        // The background is read off the border, not assumed white.
        let rgba = page(200, 100, [10, 10, 20], [240; 3], &[(50, 20, 100, 40)]);
        let bounds = ink_bounds(200, 100, &rgba).expect("light ink on dark is still ink");
        assert!(close(bounds.x, 0.25));
        assert!(close(bounds.height, 0.40));
    }

    #[test]
    fn a_blank_page_says_nothing() {
        let rgba = page(200, 100, [255; 3], [0; 3], &[]);
        assert_eq!(ink_bounds(200, 100, &rgba), None);
    }

    #[test]
    fn a_speck_is_not_content() {
        // One dark pixel — dust on a scan — must not hold the margins open,
        // and with nothing else on the page there is nothing to measure.
        let rgba = page(200, 100, [255; 3], [0; 3], &[(190, 5, 1, 1)]);
        assert_eq!(ink_bounds(200, 100, &rgba), None);
    }

    #[test]
    fn a_speck_beside_real_content_is_ignored() {
        let rgba = page(
            200,
            100,
            [255; 3],
            [0; 3],
            &[(50, 40, 100, 20), (2, 2, 1, 1)],
        );
        let bounds = ink_bounds(200, 100, &rgba).expect("the block is content");
        assert!(
            close(bounds.x, 0.25),
            "the speck did not pull the left edge"
        );
        assert!(close(bounds.y, 0.40), "nor the top");
    }

    #[test]
    fn a_torn_buffer_is_refused() {
        assert_eq!(ink_bounds(200, 100, &[0u8; 16]), None);
        assert_eq!(ink_bounds(2, 2, &[255u8; 16]), None);
    }

    #[test]
    fn the_combined_crop_is_the_union_no_page_loses_ink() {
        // One page's content sits left, the other's sits right: the window
        // must hold both.
        let pages = [
            Region::new(0.10, 0.20, 0.40, 0.60),
            Region::new(0.40, 0.10, 0.45, 0.60),
        ];
        let crop = combined_crop(&pages).expect("wide margins are worth cropping");
        assert!(crop.x <= 0.10, "left edge holds the leftmost page");
        assert!(
            crop.x + crop.width >= 0.85,
            "right edge holds the rightmost"
        );
        assert!(crop.y <= 0.10);
        assert!(crop.y + crop.height >= 0.80);
    }

    #[test]
    fn breathing_room_is_kept() {
        let pages = [Region::new(0.20, 0.20, 0.60, 0.60)];
        let crop = combined_crop(&pages).expect("a centred block leaves margins");
        assert!(crop.x < 0.20, "the crop stands off the ink");
        assert!(crop.x + crop.width > 0.80);
    }

    #[test]
    fn tight_pages_are_left_alone() {
        // Nearly full-bleed on every side: not worth re-rendering the deck.
        let pages = [Region::new(0.01, 0.01, 0.98, 0.98)];
        assert_eq!(combined_crop(&pages), None);
    }

    /// The page bounds of a book: a full-bleed sheet.
    fn cover() -> Region {
        Region::new(0.0, 0.0, 1.0, 1.0)
    }

    /// The page bounds of an ordinary text page, with real margins.
    fn text_page() -> Region {
        Region::new(0.15, 0.10, 0.70, 0.80)
    }

    #[test]
    fn a_full_bleed_page_inside_the_document_holds_the_window_open() {
        // A figure printed to the edges is content, not a cover: no
        // interior page may lose ink, however many neighbours disagree.
        let pages = [text_page(), text_page(), cover(), text_page(), text_page()];
        assert_eq!(combined_crop(&pages), None);
    }

    #[test]
    fn a_cover_is_set_aside() {
        let pages = [cover(), text_page(), text_page(), text_page()];
        let crop = combined_crop(&pages).expect("the interior agrees on its margins");
        assert!(crop.x > 0.10, "the text pages' margins came off");
        assert!(crop.x + crop.width < 0.90);
    }

    #[test]
    fn a_back_page_is_set_aside_too() {
        let pages = [text_page(), text_page(), text_page(), cover()];
        assert!(combined_crop(&pages).is_some());

        let both = [cover(), text_page(), text_page(), text_page(), cover()];
        assert!(
            combined_crop(&both).is_some(),
            "a cover at each end still leaves an interior to read"
        );
    }

    #[test]
    fn a_two_page_flyer_keeps_its_cover() {
        // With no interior to speak for the document, the full-bleed page's
        // veto stands.
        let pages = [cover(), text_page()];
        assert_eq!(combined_crop(&pages), None);
        let three = [cover(), text_page(), text_page()];
        assert_eq!(
            combined_crop(&three),
            None,
            "two interior pages are one short of enough"
        );
    }

    #[test]
    fn an_all_full_bleed_deck_yields_nothing() {
        let pages = [cover(), cover(), cover(), cover(), cover()];
        assert_eq!(combined_crop(&pages), None);
    }

    #[test]
    fn a_tight_first_page_is_not_a_cover() {
        // Near-full-bleed on one axis only: a wide banner page. It is not
        // full-bleed on every side, so it is content and keeps its veto.
        let pages = [
            Region::new(0.0, 0.40, 1.0, 0.20),
            text_page(),
            text_page(),
            text_page(),
        ];
        let crop = combined_crop(&pages).expect("its open axis still crops");
        assert!(
            close(crop.x, 0.0) && close(crop.width, 1.0),
            "the banner's full width is kept: {} + {}",
            crop.x,
            crop.width
        );
        assert!(crop.y > 0.05, "the shared vertical margins came off");
    }

    #[test]
    fn nothing_measured_means_no_crop() {
        assert_eq!(combined_crop(&[]), None);
    }

    #[test]
    fn one_axis_worth_cropping_is_enough() {
        // Wide side margins, no headroom: the horizontal gain carries it.
        let pages = [Region::new(0.25, 0.0, 0.50, 1.0)];
        let crop = combined_crop(&pages).expect("the side margins are worth it");
        assert!(close(crop.y, 0.0));
        assert!(close(crop.height, 1.0));
        assert!(crop.x > 0.15);
    }
}
