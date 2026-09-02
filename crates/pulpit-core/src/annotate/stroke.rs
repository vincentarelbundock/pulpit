//! Stroke sampling, simplification and validation.
//!
//! A pointer that is sampled at 1000 Hz produces thousands of points for a
//! gesture the eye reads as one curve, and every one of them would be written
//! into an `/InkList` and re-parsed by every viewer that opens the file. So a
//! stroke is thinned twice: while it is being drawn, by refusing points that
//! are on top of the last one, and once on release, by dropping the points
//! that lie on the line their neighbours already describe.
//!
//! Both bounds are in *page points* (A4), not pixels, so a stroke drawn on a
//! zoomed-in page is thinned by the same rule as one drawn zoomed out.

use serde::{Deserialize, Serialize};

use crate::page::PagePoint;

/// A sampled point of a stroke.
///
/// Just a position today. Pressure is deferred until the PDF mapping is
/// specified (§3.3), and this is the type that gains the field when it is —
/// which is why it is not simply `PagePoint`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct InkPoint {
    pub at: PagePoint,
}

impl InkPoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self {
            at: PagePoint::new(x, y),
        }
    }
}

impl From<PagePoint> for InkPoint {
    fn from(at: PagePoint) -> Self {
        Self { at }
    }
}

/// The most points one `/Ink` annotation may carry.
///
/// A stroke is a hand movement of a few seconds; at the sampling floor below
/// it takes a path several metres long across the page to reach this. Past it
/// the input is not a stroke, and A8 requires the bound to exist before the
/// allocation rather than after it.
pub const MAX_INK_POINTS: usize = 20_000;

/// How far the pointer must travel before a point is worth keeping, in page
/// points.
///
/// A quarter of a point is well below the width of the thinnest stroke pulpit
/// will draw, so nothing the eye can see is lost, and it removes the run of
/// identical samples a stationary pointer produces.
pub const MIN_SAMPLE_DISTANCE: f32 = 0.25;

/// How far a point may sit from the line its neighbours describe before it is
/// carrying information, in page points.
///
/// Half a point: at a normal reading zoom that is a fraction of a screen
/// pixel, so simplification is invisible, and on a typical hand-drawn stroke
/// it removes four points in five.
pub const SIMPLIFY_TOLERANCE: f32 = 0.5;

/// Should this sample be kept, given the last one that was?
///
/// The caller owns the buffer; this is the whole of the "sampling algorithm"
/// §5.3 asks be shared, so the presenter and document mode thin a stroke
/// identically and a mark drawn in one looks the same in the other.
pub fn accept_sample(last: Option<InkPoint>, candidate: InkPoint) -> bool {
    if !candidate.at.x.is_finite() || !candidate.at.y.is_finite() {
        return false;
    }
    match last {
        None => true,
        Some(last) => last.at.distance_to(candidate.at) >= MIN_SAMPLE_DISTANCE,
    }
}

/// Drop the points that lie within `tolerance` of the line their neighbours
/// describe — Ramer–Douglas–Peucker, iteratively so a pathological stroke
/// cannot overflow the stack.
///
/// The first and last points are always kept: they are where the gesture
/// started and stopped, which is the part of a stroke a user is most sure
/// about.
pub fn simplify(points: &[InkPoint], tolerance: f32) -> Vec<InkPoint> {
    if points.len() <= 2 || tolerance <= 0.0 || !tolerance.is_finite() {
        return points.to_vec();
    }

    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;

    // An explicit stack, because a stroke of 20 000 points that zig-zags
    // recurses 20 000 deep.
    let mut segments = vec![(0usize, points.len() - 1)];
    while let Some((first, last)) = segments.pop() {
        if last <= first + 1 {
            continue;
        }
        let mut worst = 0.0f32;
        let mut worst_at = first;
        for (index, point) in points.iter().enumerate().take(last).skip(first + 1) {
            let distance = point
                .at
                .distance_to_segment(points[first].at, points[last].at);
            if distance > worst {
                worst = distance;
                worst_at = index;
            }
        }
        if worst > tolerance {
            keep[worst_at] = true;
            segments.push((first, worst_at));
            segments.push((worst_at, last));
        }
    }

    points
        .iter()
        .zip(keep)
        .filter_map(|(point, keep)| keep.then_some(*point))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::PageGeometry;

    fn line(count: usize) -> Vec<InkPoint> {
        (0..count).map(|i| InkPoint::new(i as f32, 100.0)).collect()
    }

    #[test]
    fn a_straight_line_simplifies_to_its_endpoints() {
        let simplified = simplify(&line(500), SIMPLIFY_TOLERANCE);
        assert_eq!(simplified.len(), 2);
        assert_eq!(simplified[0], InkPoint::new(0.0, 100.0));
        assert_eq!(simplified[1], InkPoint::new(499.0, 100.0));
    }

    #[test]
    fn a_corner_survives_simplification() {
        let points = vec![
            InkPoint::new(0.0, 0.0),
            InkPoint::new(50.0, 0.0),
            InkPoint::new(100.0, 0.0),
            InkPoint::new(100.0, 50.0),
            InkPoint::new(100.0, 100.0),
        ];
        let simplified = simplify(&points, SIMPLIFY_TOLERANCE);
        assert_eq!(simplified.len(), 3, "{simplified:?}");
        assert_eq!(simplified[1], InkPoint::new(100.0, 0.0));
    }

    #[test]
    fn a_stroke_that_doubles_back_keeps_its_fold() {
        // Out and back along the same line: the infinite line through the
        // endpoints contains every point, so only a segment-distance measure
        // keeps the far end.
        let points = vec![
            InkPoint::new(0.0, 0.0),
            InkPoint::new(100.0, 0.0),
            InkPoint::new(0.0, 0.0),
        ];
        assert_eq!(simplify(&points, SIMPLIFY_TOLERANCE).len(), 3);
    }

    #[test]
    fn simplification_never_moves_the_ends_and_never_grows_a_stroke() {
        for count in [0usize, 1, 2, 3, 17, 1_000] {
            let points = line(count);
            let simplified = simplify(&points, SIMPLIFY_TOLERANCE);
            assert!(simplified.len() <= points.len().max(1) || points.is_empty());
            if let (Some(first), Some(last)) = (points.first(), points.last()) {
                assert_eq!(simplified.first(), Some(first));
                assert_eq!(simplified.last(), Some(last));
            }
        }
    }

    #[test]
    fn a_nonsense_tolerance_leaves_the_stroke_alone() {
        let points = line(50);
        assert_eq!(simplify(&points, 0.0).len(), 50);
        assert_eq!(simplify(&points, -1.0).len(), 50);
        assert_eq!(simplify(&points, f32::NAN).len(), 50);
    }

    #[test]
    fn a_zigzag_deep_enough_to_have_overflowed_a_recursive_pass_is_handled() {
        // Every point is a local extreme, so every point is kept and the
        // subdivision is maximally deep.
        let points: Vec<InkPoint> = (0..20_000)
            .map(|i| InkPoint::new(i as f32, if i % 2 == 0 { 0.0 } else { 40.0 }))
            .collect();
        assert_eq!(simplify(&points, SIMPLIFY_TOLERANCE).len(), points.len());
    }

    #[test]
    fn samples_on_top_of_each_other_are_refused() {
        let first = InkPoint::new(10.0, 10.0);
        assert!(accept_sample(None, first));
        assert!(!accept_sample(Some(first), first));
        assert!(!accept_sample(
            Some(first),
            InkPoint::new(10.0 + MIN_SAMPLE_DISTANCE / 2.0, 10.0)
        ));
        assert!(accept_sample(
            Some(first),
            InkPoint::new(10.0 + MIN_SAMPLE_DISTANCE, 10.0)
        ));
    }

    #[test]
    fn a_sample_that_is_not_a_number_is_refused_before_it_reaches_a_buffer() {
        assert!(!accept_sample(None, InkPoint::new(f32::NAN, 0.0)));
        assert!(!accept_sample(None, InkPoint::new(0.0, f32::INFINITY)));
    }

    #[test]
    fn the_bounds_are_the_ones_the_spec_names() {
        let page = PageGeometry::upright(612.0, 792.0);
        // Sampling at the floor across a letter page's diagonal stays well
        // inside the point limit, which is what makes the limit a hostility
        // bound rather than something a real stroke can hit.
        let diagonal = page.width.hypot(page.height);
        assert!((diagonal / MIN_SAMPLE_DISTANCE) < MAX_INK_POINTS as f32);
    }
}
