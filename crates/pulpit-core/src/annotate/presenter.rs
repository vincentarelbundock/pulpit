//! Presenter marks as document annotations, and back again.
//!
//! A presenter draws on a *slide*: a rectangle of pixels the projector is
//! showing, in which every position is a fraction of the way across and down.
//! A document annotation lives on a *page*: PDF points from the crop box's
//! top-left corner (A4). Those are not the same space, and for a split-page
//! deck they are not even the same rectangle — the slide is half the physical
//! page, and a mark two thirds of the way across the slide is one third of the
//! way across the paper.
//!
//! This is the whole of that translation, in both directions, with nothing
//! else in it. It matters that it goes both ways: a mark made in presentation
//! is committed through here, and the same mark read back out of the file is
//! drawn on the slide through here, so a round trip that does not land where
//! it started is a test failure rather than a thing a presenter discovers
//! mid-talk.
//!
//! The reason this is one function rather than two conversions written at each
//! call site is invariant A1. There is one representation of a completed mark,
//! and every view of it is derived; a second place that knew how to do this
//! would be a second place that could be wrong.

use crate::annotate::draft::{AnnotationDraft, InkDraft, MarkStyle};
use crate::annotate::id::AnnotationId;
use crate::annotate::stroke::InkPoint;
use crate::annotation::{InkColor, InkStroke, StrokeKind, TextMark};
use crate::notes::Region;
use crate::page::{PageGeometry, PageIndex, PagePoint, PageRect};

/// Where a slide sits on a page, and how big that page is.
///
/// Everything needed to place a mark, and nothing about the mark itself. Built
/// from the notes mapping's `PageSource` and the geometry the document engine
/// reported for that page, which is what keeps a rotated or oddly cropped page
/// from needing a special case here: `PageGeometry` has already resolved that
/// into a plain width and height in canonical space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlidePlacement {
    /// The physical page the slide is a view of.
    pub page: PageIndex,
    /// The part of that page the slide shows, in fractions of the page.
    pub region: Region,
    /// The page's canonical size in points.
    pub width: f32,
    pub height: f32,
}

impl SlidePlacement {
    pub fn new(page: PageIndex, region: Region, geometry: &PageGeometry) -> SlidePlacement {
        SlidePlacement {
            page,
            region,
            width: geometry.width,
            height: geometry.height,
        }
    }

    /// Whether this placement can carry a mark at all.
    ///
    /// A region of zero width is not a slide, and a page of zero size is not a
    /// page. Both are refused rather than divided by: an empty region would
    /// send every mark to the same point, which reads as "annotation is
    /// broken" rather than as the mapping problem it is.
    pub fn is_usable(&self) -> bool {
        self.region.width > 0.0 && self.region.height > 0.0 && self.width > 0.0 && self.height > 0.0
    }

    /// A point on the slide, in fractions, as a point on the page, in points.
    pub fn to_page(&self, slide: (f32, f32)) -> PagePoint {
        PagePoint {
            x: (self.region.x + slide.0 * self.region.width) * self.width,
            y: (self.region.y + slide.1 * self.region.height) * self.height,
        }
    }

    /// A point on the page, in points, as a point on the slide, in fractions.
    ///
    /// The inverse of `to_page`, and deliberately unclamped: a mark that is
    /// off the slide is off the slide, and saying so is more useful than
    /// silently dragging it to the edge, where it would look like a mark
    /// someone made there.
    pub fn to_slide(&self, page: PagePoint) -> (f32, f32) {
        (
            (page.x / self.width - self.region.x) / self.region.width,
            (page.y / self.height - self.region.y) / self.region.height,
        )
    }

    /// A resolved text run on the page as a quad on the slide.
    ///
    /// The engine reports one quad per contiguous run of selected text, in
    /// page points. All four corners go through `to_slide` rather than a
    /// bounding box of two: a line of rotated text resolves to a quad that is
    /// not axis-aligned, and squaring it up here would highlight a rectangle
    /// the words do not sit in. Unclamped, like `to_slide` — a run on the half
    /// of a split page this slide is not showing comes back outside `0..=1`,
    /// which is the honest answer.
    pub fn quad_to_slide(&self, quad: &crate::page::PageQuad) -> [(f32, f32); 4] {
        [
            self.to_slide(quad.upper_left),
            self.to_slide(quad.upper_right),
            self.to_slide(quad.lower_right),
            self.to_slide(quad.lower_left),
        ]
    }

    /// Whether a slide-space point is on the slide at all.
    pub fn contains(&self, slide: (f32, f32)) -> bool {
        (0.0..=1.0).contains(&slide.0) && (0.0..=1.0).contains(&slide.1)
    }

    /// A stroke width in fractions of the slide's width, as points on the page.
    ///
    /// Width follows the horizontal scale alone. A pen has one width, and a
    /// slide stretched more one way than the other would otherwise give it
    /// two — which PDF ink has no way to express and no viewer would draw.
    pub fn to_page_width(&self, slide_width: f32) -> f32 {
        slide_width * self.region.width * self.width
    }

    /// The inverse: a page-space width back into fractions of the slide.
    pub fn to_slide_width(&self, page_width: f32) -> f32 {
        let across = self.region.width * self.width;
        if across > 0.0 {
            page_width / across
        } else {
            0.0
        }
    }
}

/// A completed presenter stroke as a draft annotation, ready to commit.
///
/// `None` when the placement cannot carry it, or when the stroke has nothing
/// in it — an empty `/Ink` is a thing other viewers draw as a stray dot, and a
/// tap that drew nothing should leave nothing behind.
pub fn stroke_to_draft(stroke: &InkStroke, placement: &SlidePlacement) -> Option<AnnotationDraft> {
    if !placement.is_usable() || stroke.points.is_empty() {
        return None;
    }
    let points: Vec<InkPoint> = stroke
        .points
        .iter()
        .map(|point| {
            let at = placement.to_page(*point);
            InkPoint::new(at.x, at.y)
        })
        .collect();
    Some(AnnotationDraft::Ink(InkDraft {
        page: placement.page,
        points,
        style: MarkStyle {
            color: stroke.color,
            width: placement.to_page_width(stroke.width),
            // Opacity is what carries "this was a highlighter" into the file,
            // because it is what a highlighter *is* in PDF: `/CA` below one,
            // over the same `/Ink` geometry. There is no second field saying
            // which tool made it, and there should not be — a viewer that
            // never heard of pulpit draws the translucency and gets the right
            // answer, which is the whole point of a native annotation.
            ..match stroke.kind {
                StrokeKind::Ink => MarkStyle::default(),
                StrokeKind::Highlight => MarkStyle::highlighter(),
            }
        },
    }))
}

/// Where a presenter's text label goes on the page, and how big it is there.
///
/// The label is Typst markup, so how much room it needs is not something this
/// crate can answer: that is the compiler's, and the compiler lives in the
/// application. What is resolved here is everything that depends on the
/// slide-to-page mapping, which is this module's whole job — where the
/// top-left corner lands, how big a point of type is on the paper, and how
/// much room there is to the right of the mark before the slide's edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextPlacement {
    /// The label's top-left corner, in page points.
    pub at: PagePoint,
    /// The type size, in page points.
    pub size_pt: f32,
    /// The room from `at` to the right edge of the slide, in page points,
    /// which is the width the markup is set to.
    pub width_pt: f32,
}

/// A presenter's text label as page geometry, ready to be compiled.
///
/// `None` when the placement cannot carry it, when the label is off the slide,
/// or when it says nothing — an empty label is a box other viewers draw and
/// nobody meant to make (§8.5).
pub fn text_placement(mark: &TextMark, placement: &SlidePlacement) -> Option<TextPlacement> {
    if !placement.is_usable() || mark.text.trim().is_empty() || !placement.contains(mark.position) {
        return None;
    }
    let width_pt = placement.to_page_width(1.0 - mark.position.0);
    let size_pt = placement.to_page_width(mark.size);
    if width_pt <= 0.0 || size_pt <= 0.0 {
        return None;
    }
    Some(TextPlacement {
        at: placement.to_page(mark.position),
        size_pt,
        width_pt,
    })
}

/// The rectangle a compiled label occupies on the page.
///
/// The compiled picture's width and height are in the same points the type
/// size was given in, so both go on the page unscaled. They are deliberately
/// *not* re-derived through the slide's vertical scale: a split-page slide is
/// squeezed more one way than the other, and a picture squeezed with it would
/// be type taller than it is wide. A label has one size for the same reason a
/// pen has one width (`to_page_width`).
pub fn text_rect(at: PagePoint, width_pt: f32, height_pt: f32) -> PageRect {
    PageRect::new(at.x, at.y, at.x + width_pt, at.y + height_pt)
}

/// The inverse: a committed Typst mark as the label a slide draws.
///
/// This is how presentation shows a text mark that is in the document —
/// including one made in document mode, and one that was in the file before
/// pulpit opened it. Like [`ink_to_stroke`], it is a *view* of the annotation
/// (A1) rather than a second copy of it.
///
/// The label comes back with a `fit`: the box the annotation occupies, in
/// slide fractions. What a PDF records is the box, not the type size the
/// markup was set at, so putting the compiled picture back in the box it
/// claims is the only reading that cannot drift.
///
/// `id` is the local drawing identity the caller assigns; the annotation's own
/// name goes in `annotation`.
pub fn stamp_to_text(
    id: u64,
    annotation: AnnotationId,
    page: PageIndex,
    rect: PageRect,
    source: &str,
    color: InkColor,
    placement: &SlidePlacement,
) -> Option<TextMark> {
    if !placement.is_usable() || placement.page != page || source.trim().is_empty() {
        return None;
    }
    let corner = placement.to_slide(PagePoint::new(rect.left, rect.top));
    let far = placement.to_slide(PagePoint::new(rect.right, rect.bottom));
    // A mark on the other half of a split page belongs to the other slide.
    if !placement.contains(corner) {
        return None;
    }
    let fit = (far.0 - corner.0, far.1 - corner.1);
    if fit.0 <= 0.0 || fit.1 <= 0.0 {
        return None;
    }
    Some(TextMark {
        id,
        position: corner,
        text: source.to_string(),
        // The size the markup is *set* at when it is compiled again. The
        // picture is drawn into `fit` whatever this comes out as, so it only
        // decides where lines break — but a size wildly unlike the one the
        // mark was made at would break them somewhere else and be stretched
        // into the box, so it is estimated from the box rather than guessed.
        // A one-line mark's box is its type size plus a line's leading and the
        // compiler's gutter, which is where the fraction comes from; a mark of
        // several lines compiles smaller than its box and is scaled up to it.
        size: placement.to_slide_width((rect.bottom - rect.top) * 0.625),
        color,
        // A note is drawn as its icon, never as a label, so nothing that comes
        // back through here is one.
        note: false,
        annotation: Some(annotation),
        fit: Some(fit),
    })
}

/// A committed highlight as the runs a slide draws.
///
/// The quads are the engine's answer about where the *text* is, in page
/// points; the slide draws them as fractions. Every corner goes through
/// `to_slide` rather than a bounding box of two, for the reason
/// `quad_to_slide` gives.
pub fn highlight_to_runs(
    page: PageIndex,
    quads: &[crate::page::PageQuad],
    placement: &SlidePlacement,
) -> Option<Vec<[(f32, f32); 4]>> {
    if !placement.is_usable() || placement.page != page || quads.is_empty() {
        return None;
    }
    let runs: Vec<_> = quads
        .iter()
        .map(|quad| placement.quad_to_slide(quad))
        .filter(|run| run.iter().any(|corner| placement.contains(*corner)))
        .collect();
    (!runs.is_empty()).then_some(runs)
}

/// Which presenter tool a committed style reads as.
///
/// The inverse of the choice above, and the only place that inverse is made.
pub fn kind_of(style: &MarkStyle) -> StrokeKind {
    if style.opacity < 1.0 {
        StrokeKind::Highlight
    } else {
        StrokeKind::Ink
    }
}

/// The inverse: a committed ink annotation as the stroke a slide draws.
///
/// This is how presentation shows a mark that is in the document — including
/// one that was already there when the file was opened, and one made in
/// document mode. There is one representation and this is a view of it (A1).
///
/// `None` when the annotation is not on this slide: another page, another half
/// of a split page, or ink with nothing in it.
pub fn ink_to_stroke(
    id: AnnotationId,
    page: PageIndex,
    points: &[PagePoint],
    color: InkColor,
    width: f32,
    kind: StrokeKind,
    placement: &SlidePlacement,
) -> Option<InkStroke> {
    if !placement.is_usable() || placement.page != page || points.is_empty() {
        return None;
    }
    let slide: Vec<(f32, f32)> = points
        .iter()
        .map(|point| placement.to_slide(*point))
        .collect();
    // A stroke drawn on the other half of a split page belongs to the other
    // slide, and drawing it here would put someone's notes-side scribble
    // across the projector. One point on the slide is enough to claim it: a
    // stroke that runs off the edge was still made here.
    if !slide.iter().any(|point| placement.contains(*point)) {
        return None;
    }
    Some(InkStroke {
        points: slide,
        width: placement.to_slide_width(width),
        color,
        kind,
        // Named, because it came out of the document: this is the annotation
        // it shows, and erasing it erases that.
        id: Some(id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::PageRotation;

    fn letter() -> PageGeometry {
        PageGeometry::upright(612.0, 792.0)
    }

    fn full(geometry: &PageGeometry) -> SlidePlacement {
        SlidePlacement::new(PageIndex(3), Region::FULL, geometry)
    }

    fn named(name: &str) -> AnnotationId {
        AnnotationId::imported(name).expect("a name")
    }

    fn stroke(points: Vec<(f32, f32)>) -> InkStroke {
        InkStroke {
            points,
            width: 0.004,
            color: InkColor::Red,
            kind: StrokeKind::Ink,
            id: None,
        }
    }

    #[test]
    fn a_mark_on_a_whole_page_lands_where_it_was_drawn() {
        let geometry = letter();
        let placement = full(&geometry);
        // The middle of the slide is the middle of the page.
        let middle = placement.to_page((0.5, 0.5));
        assert!((middle.x - 306.0).abs() < 1e-3, "{}", middle.x);
        assert!((middle.y - 396.0).abs() < 1e-3, "{}", middle.y);
        // The top-left corner is the crop box's origin, +y down (A4).
        let origin = placement.to_page((0.0, 0.0));
        assert_eq!((origin.x, origin.y), (0.0, 0.0));
    }

    #[test]
    fn a_mark_on_the_left_half_of_a_split_page_lands_on_that_half() {
        let geometry = letter();
        let placement = SlidePlacement::new(PageIndex(0), Region::left_half(), &geometry);
        // Two thirds across the *slide* is one third across the paper.
        let at = placement.to_page((2.0 / 3.0, 0.5));
        assert!((at.x - 204.0).abs() < 1e-2, "{}", at.x);
        // Down is unaffected: the split is horizontal.
        assert!((at.y - 396.0).abs() < 1e-3, "{}", at.y);
    }

    #[test]
    fn every_placement_round_trips_a_point_it_could_carry() {
        // The property the whole module exists for. If this ever fails, a
        // mark made in presentation and read back in document mode is in a
        // different place, which is exactly the bug one representation is
        // meant to make impossible.
        let geometries = [
            PageGeometry::upright(612.0, 792.0),
            PageGeometry::upright(1024.0, 768.0),
            PageGeometry::new(20.0, 30.0, 400.0, 300.0, PageRotation::Clockwise90, 1.0),
            PageGeometry::new(0.0, 0.0, 595.0, 842.0, PageRotation::Clockwise180, 1.0),
            PageGeometry::new(5.0, 7.0, 300.0, 500.0, PageRotation::Clockwise270, 1.0),
        ];
        let regions = [
            Region::FULL,
            Region::left_half(),
            Region::new(0.5, 0.0, 0.5, 1.0),
            Region::new(0.25, 0.1, 0.5, 0.4),
        ];
        for geometry in &geometries {
            for region in &regions {
                let placement = SlidePlacement::new(PageIndex(1), *region, geometry);
                assert!(placement.is_usable());
                for slide in [
                    (0.0, 0.0),
                    (1.0, 1.0),
                    (0.5, 0.5),
                    (0.125, 0.875),
                    (0.99, 0.01),
                ] {
                    let back = placement.to_slide(placement.to_page(slide));
                    assert!(
                        (back.0 - slide.0).abs() < 1e-3 && (back.1 - slide.1).abs() < 1e-3,
                        "{slide:?} came back as {back:?} on {region:?} of {}x{}",
                        geometry.width,
                        geometry.height
                    );
                }
                // Width round-trips too, or a stroke thickens every time it
                // is read back and rewritten.
                let width = 0.004;
                let back = placement.to_slide_width(placement.to_page_width(width));
                assert!((back - width).abs() < 1e-6, "{back}");
            }
        }
    }

    #[test]
    fn a_stroke_becomes_a_draft_on_the_page_it_was_drawn_over() {
        let geometry = letter();
        let placement = SlidePlacement::new(PageIndex(3), Region::FULL, &geometry);
        let draft = stroke_to_draft(&stroke(vec![(0.0, 0.0), (0.5, 0.5)]), &placement)
            .expect("a drawn stroke is a draft");
        let AnnotationDraft::Ink(ink) = draft else {
            panic!("ink is ink")
        };
        assert_eq!(ink.page, PageIndex(3));
        assert_eq!(ink.points.len(), 2);
        assert!((ink.points[1].at.x - 306.0).abs() < 1e-3);
        // 0.004 of 612 points, because width follows the horizontal scale.
        assert!(
            (ink.style.width - 2.448).abs() < 1e-3,
            "{}",
            ink.style.width
        );
        assert_eq!(kind_of(&ink.style), StrokeKind::Ink);
    }

    #[test]
    fn a_highlighter_stroke_keeps_its_translucency() {
        let geometry = letter();
        let mut highlight = stroke(vec![(0.1, 0.1), (0.9, 0.1)]);
        highlight.kind = StrokeKind::Highlight;
        let draft =
            stroke_to_draft(&highlight, &full(&geometry)).expect("a highlighter stroke commits");
        let AnnotationDraft::Ink(ink) = draft else {
            panic!("ink is ink")
        };
        assert!(ink.style.opacity < 1.0, "{}", ink.style.opacity);
        assert_eq!(kind_of(&ink.style), StrokeKind::Highlight);
    }

    #[test]
    fn a_stroke_with_nothing_in_it_commits_nothing() {
        let geometry = letter();
        assert!(stroke_to_draft(&stroke(Vec::new()), &full(&geometry)).is_none());
    }

    #[test]
    fn an_unusable_placement_refuses_rather_than_dividing_by_it() {
        let geometry = letter();
        let empty = SlidePlacement::new(PageIndex(0), Region::new(0.0, 0.0, 0.0, 1.0), &geometry);
        assert!(!empty.is_usable());
        assert!(stroke_to_draft(&stroke(vec![(0.5, 0.5)]), &empty).is_none());

        let nothing =
            SlidePlacement::new(PageIndex(0), Region::FULL, &PageGeometry::upright(0.0, 0.0));
        assert!(!nothing.is_usable());
        assert!(stroke_to_draft(&stroke(vec![(0.5, 0.5)]), &nothing).is_none());
    }

    #[test]
    fn a_committed_mark_comes_back_as_the_stroke_that_made_it() {
        // The round trip that matters to a presenter: draw it, commit it,
        // read it out of the document, draw it again.
        let geometry = letter();
        let placement = SlidePlacement::new(PageIndex(3), Region::left_half(), &geometry);
        let original = stroke(vec![(0.1, 0.2), (0.4, 0.6), (0.9, 0.3)]);
        let draft = stroke_to_draft(&original, &placement).expect("a draft");
        let AnnotationDraft::Ink(ink) = draft else {
            panic!("ink is ink")
        };
        let points: Vec<PagePoint> = ink.points.iter().map(|point| point.at).collect();
        let back = ink_to_stroke(
            named("committed"),
            ink.page,
            &points,
            ink.style.color,
            ink.style.width,
            kind_of(&ink.style),
            &placement,
        )
        .expect("the mark is on this slide");

        assert_eq!(back.points.len(), original.points.len());
        for (drawn, read) in original.points.iter().zip(&back.points) {
            assert!(
                (drawn.0 - read.0).abs() < 1e-3 && (drawn.1 - read.1).abs() < 1e-3,
                "{drawn:?} came back as {read:?}"
            );
        }
        assert!((back.width - original.width).abs() < 1e-5);
        assert_eq!(back.color, original.color);
        assert_eq!(back.kind, original.kind);
    }

    #[test]
    fn a_mark_on_another_page_is_not_drawn_on_this_slide() {
        let geometry = letter();
        let placement = full(&geometry);
        assert!(ink_to_stroke(
            named("elsewhere"),
            PageIndex(9),
            &[PagePoint { x: 10.0, y: 10.0 }],
            InkColor::Red,
            2.0,
            StrokeKind::Ink,
            &placement,
        )
        .is_none());
    }

    #[test]
    fn a_mark_on_the_notes_half_stays_off_the_slide_half() {
        // The case a split-page deck makes real: someone annotated the notes
        // beside the slide, and that must not appear on the projector.
        let geometry = letter();
        let slide_half = SlidePlacement::new(PageIndex(0), Region::left_half(), &geometry);
        // A mark three quarters of the way across the paper: the notes side.
        let on_the_notes = [
            PagePoint { x: 459.0, y: 100.0 },
            PagePoint { x: 500.0, y: 150.0 },
        ];
        assert!(ink_to_stroke(
            named("notes"),
            PageIndex(0),
            &on_the_notes,
            InkColor::Red,
            2.0,
            StrokeKind::Ink,
            &slide_half,
        )
        .is_none());

        // …and one that starts on the slide and runs off it is still the
        // slide's, because that is where it was made.
        let running_off = [
            PagePoint { x: 200.0, y: 100.0 },
            PagePoint { x: 500.0, y: 100.0 },
        ];
        assert!(ink_to_stroke(
            named("running-off"),
            PageIndex(0),
            &running_off,
            InkColor::Red,
            2.0,
            StrokeKind::Ink,
            &slide_half,
        )
        .is_some());
    }

    #[test]
    fn a_resolved_text_run_lands_on_the_slide_it_was_swept_on() {
        let geometry = letter();
        let placement = full(&geometry);
        // A line of text a quarter of the way down, spanning the middle half
        // of the page.
        let quad = crate::page::PageQuad::from_rect(crate::page::PageRect {
            left: 153.0,
            top: 198.0,
            right: 459.0,
            bottom: 218.0,
        });
        let run = placement.quad_to_slide(&quad);
        // Clockwise from upper-left, and still a rectangle on an upright page.
        assert!((run[0].0 - 0.25).abs() < 1e-4, "{run:?}");
        assert!((run[0].1 - 0.25).abs() < 1e-4, "{run:?}");
        assert!((run[1].0 - 0.75).abs() < 1e-4, "{run:?}");
        assert!((run[2].1 - run[1].1).abs() > 0.0, "{run:?}");
        assert!((run[3].0 - run[0].0).abs() < 1e-6, "{run:?}");
    }

    #[test]
    fn a_run_on_the_other_half_of_a_split_page_is_not_on_this_slide() {
        let geometry = letter();
        // The slide shows the left half; the text is on the right half.
        let left = SlidePlacement::new(PageIndex(0), Region::left_half(), &geometry);
        let quad = crate::page::PageQuad::from_rect(crate::page::PageRect {
            left: 400.0,
            top: 100.0,
            right: 500.0,
            bottom: 120.0,
        });
        let run = left.quad_to_slide(&quad);
        // Unclamped, so it reads as off the slide rather than as text jammed
        // against the right edge.
        assert!(run.iter().all(|corner| corner.0 > 1.0), "{run:?}");
    }

    fn label(position: (f32, f32), text: &str) -> TextMark {
        TextMark {
            id: 1,
            position,
            text: text.to_string(),
            size: 0.025,
            color: InkColor::Black,
            note: false,
            annotation: None,
            fit: None,
        }
    }

    #[test]
    fn a_label_is_placed_and_sized_in_page_points() {
        let geometry = letter();
        let placement = full(&geometry);
        let placed = text_placement(&label((0.25, 0.5), "a thought"), &placement)
            .expect("a label on the slide");
        assert!((placed.at.x - 153.0).abs() < 1e-3, "{:?}", placed.at);
        assert!((placed.at.y - 396.0).abs() < 1e-3, "{:?}", placed.at);
        // Type size follows the page's width, like a pen's width does.
        assert!((placed.size_pt - 0.025 * 612.0).abs() < 1e-3, "{placed:?}");
        // And the markup is set to the room that is left to the right of it.
        assert!((placed.width_pt - 0.75 * 612.0).abs() < 1e-3, "{placed:?}");
    }

    #[test]
    fn a_label_on_half_a_page_is_set_to_half_the_type_size() {
        let geometry = letter();
        let left = SlidePlacement::new(PageIndex(0), Region::left_half(), &geometry);
        let placed = text_placement(&label((0.5, 0.5), "a thought"), &left).expect("a label");
        // The slide is half the paper across, so a mark sized against the
        // slide is half as many points on the paper as it would be on a whole
        // page. Anything else and a split deck's labels come out double size.
        assert!((placed.size_pt - 0.025 * 306.0).abs() < 1e-3, "{placed:?}");
        assert!((placed.at.x - 153.0).abs() < 1e-3, "{:?}", placed.at);
    }

    #[test]
    fn an_empty_label_is_not_an_annotation() {
        let geometry = letter();
        let placement = full(&geometry);
        assert!(text_placement(&label((0.5, 0.5), "   \n "), &placement).is_none());
        assert!(text_placement(&label((1.4, 0.5), "off the slide"), &placement).is_none());
    }

    #[test]
    fn a_committed_label_comes_back_in_the_box_it_claims() {
        let geometry = letter();
        let placement = full(&geometry);
        let placed = text_placement(&label((0.25, 0.5), "a thought"), &placement).expect("a label");
        let rect = text_rect(placed.at, 90.0, 24.0);
        let back = stamp_to_text(
            7,
            named("the-label"),
            PageIndex(3),
            rect,
            "a thought",
            InkColor::Black,
            &placement,
        )
        .expect("a mark on this slide");
        assert_eq!(back.id, 7);
        assert_eq!(back.annotation, Some(named("the-label")));
        assert!((back.position.0 - 0.25).abs() < 1e-4, "{back:?}");
        assert!((back.position.1 - 0.5).abs() < 1e-4, "{back:?}");
        let fit = back.fit.expect("a box");
        assert!((fit.0 - 90.0 / 612.0).abs() < 1e-4, "{fit:?}");
        assert!((fit.1 - 24.0 / 792.0).abs() < 1e-4, "{fit:?}");
    }

    #[test]
    fn a_label_on_the_other_half_of_a_split_page_is_not_adopted() {
        let geometry = letter();
        let left = SlidePlacement::new(PageIndex(0), Region::left_half(), &geometry);
        let rect = PageRect::new(400.0, 100.0, 500.0, 124.0);
        assert!(stamp_to_text(
            1,
            named("elsewhere"),
            PageIndex(0),
            rect,
            "not here",
            InkColor::Black,
            &left,
        )
        .is_none());
    }

    #[test]
    fn a_highlight_comes_back_as_the_runs_it_covers() {
        let geometry = letter();
        let placement = full(&geometry);
        let quad = crate::page::PageQuad::from_rect(PageRect {
            left: 153.0,
            top: 198.0,
            right: 459.0,
            bottom: 218.0,
        });
        let runs = highlight_to_runs(PageIndex(3), &[quad], &placement).expect("a run");
        assert_eq!(runs.len(), 1);
        assert!((runs[0][0].0 - 0.25).abs() < 1e-4, "{runs:?}");
        // Another page's highlight is not this slide's.
        assert!(highlight_to_runs(PageIndex(4), &[quad], &placement).is_none());
        assert!(highlight_to_runs(PageIndex(3), &[], &placement).is_none());
    }
}
