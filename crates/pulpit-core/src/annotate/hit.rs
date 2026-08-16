//! Hit-testing: which annotation is under the pointer, and which one the
//! eraser takes.
//!
//! Deliberately geometric rather than pixel-based. Asking the renderer which
//! annotation painted a pixel would tie selection to the frame currently on
//! screen, so a mark could not be selected until it had been drawn, and a mark
//! drawn at one zoom would be selectable at a different tolerance than the same
//! mark at another. Hit-testing against canonical geometry has neither
//! problem, and is a unit test rather than a rendering test.

use crate::page::{PagePoint, PageQuad, PageRect};

use super::draft::AnnotationKind;
use super::id::AnnotationId;

/// What a hit-test knows about one annotation.
///
/// The geometric shadow of `pulpit-render`'s `AnnotationSummary`, carrying no
/// style, no contents and no object reference — only enough to say whether the
/// pointer is on it.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationHit {
    pub id: AnnotationId,
    pub kind: AnnotationKind,
    pub bounds: PageRect,
    /// The stroke's centre line, for ink. A stroke's bounding box is mostly
    /// empty — a diagonal line's box is the whole square around it — so
    /// selecting ink by its box would make a mark half a page away answer to a
    /// click in the corner.
    pub path: Vec<PagePoint>,
    /// The marked text runs, for text markup. Same reasoning: a highlight
    /// spanning two columns has an enormous bounding box and marks very little
    /// of it.
    pub quads: Vec<PageQuad>,
    /// Whether this annotation may be edited at all (§10.1). An unsupported
    /// annotation is still hit-tested, so it can be reported, but it is never
    /// what the eraser takes.
    pub editable: bool,
    /// Painted width in page points, which is how far from the centre line a
    /// click still lands on the stroke.
    pub width: f32,
}

impl AnnotationHit {
    /// Is `point` on this annotation, allowing `tolerance` page points of
    /// slack for the size of a fingertip or a pointer?
    pub fn contains(&self, point: PagePoint, tolerance: f32) -> bool {
        let slack = tolerance.max(0.0) + self.width / 2.0;
        // The bounding box is the cheap rejection, not the answer.
        if !self.bounds.inflated(slack).contains(point) {
            return false;
        }
        if !self.path.is_empty() {
            return self
                .path
                .windows(2)
                .any(|pair| distance_to_segment(point, pair[0], pair[1]) <= slack)
                // A one-point stroke is a dot with no segment to measure.
                || self.path.iter().any(|p| p.distance_to(point) <= slack);
        }
        if !self.quads.is_empty() {
            return self
                .quads
                .iter()
                .any(|quad| quad.bounds().inflated(slack).contains(point));
        }
        true
    }

    /// Does this annotation meet the segment the pointer travelled between two
    /// samples?
    ///
    /// An eraser sweep is a series of jumps, not a continuous path: at speed
    /// the samples are tens of points apart, and testing only the sample
    /// positions leaves marks standing in the gaps between them.
    pub fn crossed_by(&self, from: PagePoint, to: PagePoint, tolerance: f32) -> bool {
        // Sampling along the sweep rather than solving segment-to-segment
        // intersection: the sweep is short, the step is bounded by the
        // tolerance, and the arithmetic stays obvious.
        let distance = from.distance_to(to);
        let step = (tolerance.max(1.0)) / 2.0;
        let steps = ((distance / step).ceil() as usize).clamp(1, 512);
        (0..=steps).any(|i| {
            let t = i as f32 / steps as f32;
            let at = PagePoint::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.contains(at, tolerance)
        })
    }
}

/// What a click landed on.
#[derive(Debug, Clone, PartialEq)]
pub enum HitTarget {
    Annotation(AnnotationId),
    /// The page itself, at a point on it.
    Page(PagePoint),
}

/// The topmost annotation under `point`.
///
/// `candidates` are in the page's `/Annots` order, which is paint order: later
/// entries are drawn over earlier ones, so the *last* match is the one the
/// user sees on top and therefore the one they mean.
pub fn topmost(
    candidates: &[AnnotationHit],
    point: PagePoint,
    tolerance: f32,
) -> Option<&AnnotationHit> {
    candidates
        .iter()
        .rev()
        .find(|candidate| candidate.contains(point, tolerance))
}

/// The annotations one eraser step takes: the topmost editable one crossed by
/// the pointer's travel (§8.3).
///
/// One per step, not all of them: an eraser that took the whole stack under
/// one flick would remove marks the user could not see.
pub fn erasable(
    candidates: &[AnnotationHit],
    from: PagePoint,
    to: PagePoint,
    tolerance: f32,
) -> Option<&AnnotationHit> {
    candidates
        .iter()
        .rev()
        .filter(|candidate| candidate.editable)
        .find(|candidate| candidate.crossed_by(from, to, tolerance))
}

fn distance_to_segment(point: PagePoint, start: PagePoint, end: PagePoint) -> f32 {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let length_squared = dx * dx + dy * dy;
    if length_squared <= f32::EPSILON {
        return point.distance_to(start);
    }
    let t =
        (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared).clamp(0.0, 1.0);
    point.distance_to(PagePoint::new(start.x + t * dx, start.y + t * dy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::id::IdGenerator;

    fn stroke(id: AnnotationId, points: Vec<PagePoint>) -> AnnotationHit {
        AnnotationHit {
            id,
            kind: AnnotationKind::Ink,
            bounds: PageRect::enclosing(points.iter().copied()).unwrap(),
            path: points,
            quads: Vec::new(),
            editable: true,
            width: 2.0,
        }
    }

    #[test]
    fn a_diagonal_stroke_is_not_selected_from_the_corner_of_its_bounding_box() {
        let mut generator = IdGenerator::new(1);
        let diagonal = stroke(
            generator.next_id(),
            vec![PagePoint::new(0.0, 0.0), PagePoint::new(200.0, 200.0)],
        );
        assert!(diagonal.contains(PagePoint::new(100.0, 100.0), 2.0));
        assert!(
            !diagonal.contains(PagePoint::new(190.0, 10.0), 2.0),
            "the empty corner of the box is not the stroke"
        );
    }

    #[test]
    fn a_single_point_stroke_is_a_dot_that_can_be_hit() {
        let mut generator = IdGenerator::new(2);
        let dot = stroke(generator.next_id(), vec![PagePoint::new(50.0, 50.0)]);
        assert!(dot.contains(PagePoint::new(50.5, 50.5), 2.0));
        assert!(!dot.contains(PagePoint::new(80.0, 50.0), 2.0));
    }

    #[test]
    fn the_topmost_annotation_is_the_last_one_painted() {
        let mut generator = IdGenerator::new(3);
        let under = stroke(
            generator.next_id(),
            vec![PagePoint::new(0.0, 50.0), PagePoint::new(200.0, 50.0)],
        );
        let over = stroke(
            generator.next_id(),
            vec![PagePoint::new(0.0, 51.0), PagePoint::new(200.0, 51.0)],
        );
        let expected = over.id.clone();
        let stack = vec![under, over];
        assert_eq!(
            topmost(&stack, PagePoint::new(100.0, 50.5), 2.0).map(|hit| hit.id.clone()),
            Some(expected)
        );
        assert!(topmost(&stack, PagePoint::new(100.0, 400.0), 2.0).is_none());
    }

    #[test]
    fn the_eraser_never_takes_an_annotation_it_may_not_edit() {
        let mut generator = IdGenerator::new(4);
        let mut unsupported = stroke(
            generator.next_id(),
            vec![PagePoint::new(0.0, 50.0), PagePoint::new(200.0, 50.0)],
        );
        unsupported.editable = false;
        let editable = stroke(
            generator.next_id(),
            vec![PagePoint::new(0.0, 50.0), PagePoint::new(200.0, 50.0)],
        );
        let expected = editable.id.clone();
        let stack = vec![editable, unsupported];
        let taken = erasable(
            &stack,
            PagePoint::new(100.0, 40.0),
            PagePoint::new(100.0, 60.0),
            3.0,
        );
        assert_eq!(taken.map(|hit| hit.id.clone()), Some(expected));
    }

    #[test]
    fn a_fast_sweep_does_not_jump_over_a_stroke() {
        let mut generator = IdGenerator::new(5);
        let vertical = stroke(
            generator.next_id(),
            vec![PagePoint::new(100.0, 0.0), PagePoint::new(100.0, 200.0)],
        );
        let stack = vec![vertical];
        // Two samples 120 points apart, one on each side of the stroke: only a
        // swept test finds it.
        assert!(erasable(
            &stack,
            PagePoint::new(40.0, 100.0),
            PagePoint::new(160.0, 100.0),
            3.0
        )
        .is_some());
        assert!(erasable(
            &stack,
            PagePoint::new(40.0, 400.0),
            PagePoint::new(160.0, 400.0),
            3.0
        )
        .is_none());
    }

    #[test]
    fn a_highlight_is_hit_on_its_runs_rather_than_across_its_columns() {
        let mut generator = IdGenerator::new(6);
        let quads = vec![
            PageQuad::from_rect(PageRect::new(50.0, 100.0, 250.0, 114.0)),
            PageQuad::from_rect(PageRect::new(350.0, 300.0, 550.0, 314.0)),
        ];
        let highlight = AnnotationHit {
            id: generator.next_id(),
            kind: AnnotationKind::Highlight,
            bounds: PageRect::new(50.0, 100.0, 550.0, 314.0),
            path: Vec::new(),
            quads,
            editable: true,
            width: 0.0,
        };
        assert!(highlight.contains(PagePoint::new(100.0, 107.0), 1.0));
        assert!(highlight.contains(PagePoint::new(400.0, 307.0), 1.0));
        assert!(
            !highlight.contains(PagePoint::new(300.0, 200.0), 1.0),
            "the gap between two runs is not highlighted"
        );
    }

    #[test]
    fn an_annotation_with_no_finer_geometry_falls_back_to_its_rectangle() {
        let mut generator = IdGenerator::new(7);
        let stamp = AnnotationHit {
            id: generator.next_id(),
            kind: AnnotationKind::Stamp,
            bounds: PageRect::new(10.0, 10.0, 60.0, 60.0),
            path: Vec::new(),
            quads: Vec::new(),
            editable: true,
            width: 0.0,
        };
        assert!(stamp.contains(PagePoint::new(30.0, 30.0), 0.0));
        assert!(!stamp.contains(PagePoint::new(300.0, 30.0), 0.0));
    }

    #[test]
    fn a_wider_stroke_is_easier_to_hit() {
        let mut generator = IdGenerator::new(8);
        let mut thin = stroke(
            generator.next_id(),
            vec![PagePoint::new(0.0, 50.0), PagePoint::new(200.0, 50.0)],
        );
        thin.width = 1.0;
        let mut fat = thin.clone();
        fat.width = 40.0;
        assert!(!thin.contains(PagePoint::new(100.0, 65.0), 0.0));
        assert!(fat.contains(PagePoint::new(100.0, 65.0), 0.0));
    }
}
