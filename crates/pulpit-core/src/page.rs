//! Canonical page space: the one coordinate system persistent document
//! geometry is expressed in.
//!
//! Invariant A4 of `SPEC-document.md`. Canonical page space has
//!
//! - its origin at the displayed crop box's top-left;
//! - positive x to the right and positive y *downward*;
//! - dimensions in PDF points, measured after page rotation;
//! - finite coordinates, bounded to [`PAGE_MARGIN`] points outside the page.
//!
//! It is deliberately *not* PDF user space, whose origin is bottom-left and
//! whose axes ignore `/Rotate`, and deliberately not the normalised
//! `0.0..=1.0` space the presenter draws transient marks in. Converting to PDF
//! user space is `pulpit-render`'s job and nobody else's; converting from
//! normalised presenter coordinates happens once, at the UI boundary, through
//! [`PageGeometry::from_normalised`].
//!
//! Everything here is pure arithmetic on `f32` and carries no PDF library
//! type, which is what lets rotation and crop-box handling be ordinary unit
//! tests.

use serde::{Deserialize, Serialize};

/// A zero-based physical page index.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct PageIndex(pub usize);

impl PageIndex {
    pub fn get(self) -> usize {
        self.0
    }
}

impl std::fmt::Display for PageIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Pages are counted from one everywhere a human reads them.
        write!(f, "{}", self.0 + 1)
    }
}

/// How far outside the page a canonical coordinate may still be.
///
/// Not zero, because a stroke's painted width, a resize handle dragged past
/// the edge and a `/Rect` that a producer inflated slightly all legitimately
/// land just outside the crop box, and clamping them to the edge would move a
/// mark the user made. Not unbounded, because A8 requires geometry to be
/// bounded before it reaches an allocator or a rasteriser. A US Letter page is
/// 612 × 792 points, so this is roughly a page-and-a-half of slack in each
/// direction.
pub const PAGE_MARGIN: f32 = 1_000.0;

/// The largest page pulpit will treat as real, in points.
///
/// The PDF specification's own limit is 14 400 points (200 inches) a side for
/// most producers; anything past that is a malformed or hostile document
/// rather than a poster.
pub const MAX_PAGE_POINTS: f32 = 14_400.0;

/// A point in canonical page space.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PagePoint {
    pub x: f32,
    pub y: f32,
}

impl PagePoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Is this a coordinate that could have come from a real gesture on a page
    /// of this size?
    pub fn is_valid_on(&self, page: &PageGeometry) -> bool {
        finite(self.x)
            && finite(self.y)
            && self.x >= -PAGE_MARGIN
            && self.y >= -PAGE_MARGIN
            && self.x <= page.width + PAGE_MARGIN
            && self.y <= page.height + PAGE_MARGIN
    }

    pub fn distance_to(self, other: PagePoint) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// An axis-aligned rectangle in canonical page space, with `top < bottom`
/// because y grows downward.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PageRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl PageRect {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// The rectangle enclosing every point given, or `None` for no points.
    pub fn enclosing(points: impl IntoIterator<Item = PagePoint>) -> Option<PageRect> {
        let mut rect: Option<PageRect> = None;
        for point in points {
            rect = Some(match rect {
                None => PageRect::new(point.x, point.y, point.x, point.y),
                Some(r) => PageRect::new(
                    r.left.min(point.x),
                    r.top.min(point.y),
                    r.right.max(point.x),
                    r.bottom.max(point.y),
                ),
            });
        }
        rect
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    pub fn contains(&self, point: PagePoint) -> bool {
        point.x >= self.left
            && point.x <= self.right
            && point.y >= self.top
            && point.y <= self.bottom
    }

    pub fn intersects(&self, other: &PageRect) -> bool {
        self.left <= other.right
            && other.left <= self.right
            && self.top <= other.bottom
            && other.top <= self.bottom
    }

    /// Grow by `amount` on every side. Used to turn a stroke's centre-line
    /// bounds into the `/Rect` that encloses its painted width.
    pub fn inflated(&self, amount: f32) -> PageRect {
        PageRect::new(
            self.left - amount,
            self.top - amount,
            self.right + amount,
            self.bottom + amount,
        )
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, other: &PageRect) -> PageRect {
        PageRect::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    pub fn is_finite(&self) -> bool {
        finite(self.left) && finite(self.top) && finite(self.right) && finite(self.bottom)
    }

    /// Well-formed and inside the documented margin around `page`.
    pub fn is_valid_on(&self, page: &PageGeometry) -> bool {
        self.is_finite()
            && self.left <= self.right
            && self.top <= self.bottom
            && PagePoint::new(self.left, self.top).is_valid_on(page)
            && PagePoint::new(self.right, self.bottom).is_valid_on(page)
    }
}

/// One quadrilateral of a text-markup annotation.
///
/// Four corners rather than a rectangle because `/QuadPoints` describes a run
/// of text, and a run of text on a rotated or sheared page is not axis
/// aligned. The corner order here is upper-left, upper-right, lower-left,
/// lower-right — the order the PDF specification's `/QuadPoints` entry uses,
/// so the conversion is a coordinate change and not also a permutation.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PageQuad {
    pub upper_left: PagePoint,
    pub upper_right: PagePoint,
    pub lower_left: PagePoint,
    pub lower_right: PagePoint,
}

impl PageQuad {
    /// The quad of an upright rectangle, which is what an unrotated line of
    /// text resolves to.
    pub fn from_rect(rect: PageRect) -> Self {
        Self {
            upper_left: PagePoint::new(rect.left, rect.top),
            upper_right: PagePoint::new(rect.right, rect.top),
            lower_left: PagePoint::new(rect.left, rect.bottom),
            lower_right: PagePoint::new(rect.right, rect.bottom),
        }
    }

    pub fn corners(&self) -> [PagePoint; 4] {
        [
            self.upper_left,
            self.upper_right,
            self.lower_left,
            self.lower_right,
        ]
    }

    pub fn bounds(&self) -> PageRect {
        PageRect::enclosing(self.corners()).expect("a quad has four corners")
    }

    pub fn is_valid_on(&self, page: &PageGeometry) -> bool {
        self.corners().iter().all(|c| c.is_valid_on(page))
    }

    /// A quad with no area marks nothing, and a zero-area `/QuadPoints` entry
    /// is what several viewers draw as a stray line across the page.
    pub fn is_degenerate(&self) -> bool {
        let bounds = self.bounds();
        bounds.width() < f32::EPSILON || bounds.height() < f32::EPSILON
    }
}

/// Page rotation, as the `/Rotate` entry expresses it: clockwise degrees, a
/// multiple of 90.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PageRotation {
    #[default]
    None,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}

impl PageRotation {
    pub const ALL: [PageRotation; 4] = [
        PageRotation::None,
        PageRotation::Clockwise90,
        PageRotation::Clockwise180,
        PageRotation::Clockwise270,
    ];

    /// From a `/Rotate` value. Out-of-range and non-multiple-of-90 values are
    /// what a malformed document carries; the specification says to treat them
    /// as absent rather than to refuse the page.
    pub fn from_degrees(degrees: i32) -> PageRotation {
        match degrees.rem_euclid(360) {
            90 => PageRotation::Clockwise90,
            180 => PageRotation::Clockwise180,
            270 => PageRotation::Clockwise270,
            _ => PageRotation::None,
        }
    }

    pub fn degrees(self) -> i32 {
        match self {
            PageRotation::None => 0,
            PageRotation::Clockwise90 => 90,
            PageRotation::Clockwise180 => 180,
            PageRotation::Clockwise270 => 270,
        }
    }

    /// Does this rotation exchange the page's width and height?
    pub fn swaps_axes(self) -> bool {
        matches!(self, PageRotation::Clockwise90 | PageRotation::Clockwise270)
    }
}

/// Everything needed to convert between canonical page space and PDF user
/// space for one page.
///
/// `width` and `height` are the *displayed* dimensions: the crop box's size
/// with rotation applied, which is what a reader sees and therefore what a
/// canonical coordinate is measured against. The crop-box origin and the
/// rotation are kept so the conversion is reversible.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PageGeometry {
    /// Displayed width in points, after rotation.
    pub width: f32,
    /// Displayed height in points, after rotation.
    pub height: f32,
    /// The crop box's lower-left corner in unrotated PDF user space.
    pub crop_left: f32,
    pub crop_bottom: f32,
    /// The crop box's size in unrotated PDF user space.
    pub crop_width: f32,
    pub crop_height: f32,
    pub rotation: PageRotation,
    /// The page's `/UserUnit`, which scales points for oversized drawings.
    /// Almost always 1.0; carried so a document that sets it round-trips.
    pub user_unit: f32,
}

impl Default for PageGeometry {
    fn default() -> Self {
        // US Letter, unrotated, crop box at the origin.
        PageGeometry::upright(612.0, 792.0)
    }
}

impl PageGeometry {
    /// The common case: an unrotated page whose crop box starts at the origin.
    pub fn upright(width: f32, height: f32) -> PageGeometry {
        PageGeometry {
            width,
            height,
            crop_left: 0.0,
            crop_bottom: 0.0,
            crop_width: width,
            crop_height: height,
            rotation: PageRotation::None,
            user_unit: 1.0,
        }
    }

    /// A page from its crop box and rotation, with the displayed dimensions
    /// derived rather than supplied — the two cannot then disagree.
    pub fn new(
        crop_left: f32,
        crop_bottom: f32,
        crop_width: f32,
        crop_height: f32,
        rotation: PageRotation,
        user_unit: f32,
    ) -> PageGeometry {
        let (width, height) = if rotation.swaps_axes() {
            (crop_height, crop_width)
        } else {
            (crop_width, crop_height)
        };
        PageGeometry {
            width,
            height,
            crop_left,
            crop_bottom,
            crop_width,
            crop_height,
            rotation,
            user_unit: if finite(user_unit) && user_unit > 0.0 {
                user_unit
            } else {
                1.0
            },
        }
    }

    /// Is this a page pulpit will work on at all? A zero-sized or absurd page
    /// is refused before anything is allocated for it (A8).
    pub fn is_valid(&self) -> bool {
        finite(self.width)
            && finite(self.height)
            && self.width > 0.0
            && self.height > 0.0
            && self.width <= MAX_PAGE_POINTS
            && self.height <= MAX_PAGE_POINTS
            && finite(self.crop_left)
            && finite(self.crop_bottom)
            && self.crop_width > 0.0
            && self.crop_height > 0.0
    }

    /// The whole page as a canonical rectangle.
    pub fn bounds(&self) -> PageRect {
        PageRect::new(0.0, 0.0, self.width, self.height)
    }

    /// Canonical page space → PDF user space.
    ///
    /// This is the *only* direction that matters for writing, and its inverse
    /// [`PageGeometry::from_user_space`] is what reads an imported annotation
    /// back. The two are tested for round-trip agreement at every rotation.
    pub fn to_user_space(&self, point: PagePoint) -> (f32, f32) {
        // First undo the rotation, giving a position within the *unrotated*
        // crop box, still with a top-left origin.
        let (cx, cy) = match self.rotation {
            PageRotation::None => (point.x, point.y),
            // The displayed page is the crop box turned 90° clockwise, which
            // puts the sheet's *bottom*-left corner at the top left of what
            // the reader sees: a displayed x is a distance back up the
            // unrotated page, and a displayed y is a distance across it.
            PageRotation::Clockwise90 => (point.y, self.crop_height - point.x),
            PageRotation::Clockwise180 => (self.crop_width - point.x, self.crop_height - point.y),
            PageRotation::Clockwise270 => (self.crop_width - point.y, point.x),
        };
        // Then flip to a bottom-left origin and offset by the crop box.
        (
            self.crop_left + cx,
            self.crop_bottom + (self.crop_height - cy),
        )
    }

    /// PDF user space → canonical page space. The exact inverse of
    /// [`PageGeometry::to_user_space`].
    pub fn from_user_space(&self, x: f32, y: f32) -> PagePoint {
        let cx = x - self.crop_left;
        let cy = self.crop_height - (y - self.crop_bottom);
        match self.rotation {
            PageRotation::None => PagePoint::new(cx, cy),
            PageRotation::Clockwise90 => PagePoint::new(self.crop_height - cy, cx),
            PageRotation::Clockwise180 => {
                PagePoint::new(self.crop_width - cx, self.crop_height - cy)
            }
            PageRotation::Clockwise270 => PagePoint::new(cy, self.crop_width - cx),
        }
    }

    /// A rectangle in canonical space as a PDF `/Rect`: left, bottom, right,
    /// top in user space, normalised so the first pair is the lower-left
    /// corner however the page is rotated.
    pub fn rect_to_user_space(&self, rect: PageRect) -> [f32; 4] {
        let a = self.to_user_space(PagePoint::new(rect.left, rect.top));
        let b = self.to_user_space(PagePoint::new(rect.right, rect.bottom));
        [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]
    }

    /// A PDF `/Rect` as a canonical rectangle.
    pub fn rect_from_user_space(&self, rect: [f32; 4]) -> PageRect {
        let a = self.from_user_space(rect[0], rect[1]);
        let b = self.from_user_space(rect[2], rect[3]);
        PageRect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
    }

    /// The eight numbers of one `/QuadPoints` entry, in the specification's
    /// order.
    pub fn quad_to_user_space(&self, quad: PageQuad) -> [f32; 8] {
        let ul = self.to_user_space(quad.upper_left);
        let ur = self.to_user_space(quad.upper_right);
        let ll = self.to_user_space(quad.lower_left);
        let lr = self.to_user_space(quad.lower_right);
        [ul.0, ul.1, ur.0, ur.1, ll.0, ll.1, lr.0, lr.1]
    }

    pub fn quad_from_user_space(&self, values: [f32; 8]) -> PageQuad {
        PageQuad {
            upper_left: self.from_user_space(values[0], values[1]),
            upper_right: self.from_user_space(values[2], values[3]),
            lower_left: self.from_user_space(values[4], values[5]),
            lower_right: self.from_user_space(values[6], values[7]),
        }
    }

    /// The presenter's normalised point, converted at the UI boundary.
    ///
    /// Normalised coordinates are fractions of the displayed page, top-left
    /// origin, so this is a scale and nothing else — and it is the only place
    /// the two spaces are allowed to meet (A4).
    pub fn from_normalised(&self, x: f32, y: f32) -> PagePoint {
        PagePoint::new(x * self.width, y * self.height)
    }

    /// The inverse, for drawing a committed annotation through the presenter's
    /// existing normalised painting path.
    pub fn to_normalised(&self, point: PagePoint) -> (f32, f32) {
        (point.x / self.width, point.y / self.height)
    }

    /// A length expressed as a fraction of the page width — which is how the
    /// presenter stores stroke widths — in points.
    pub fn width_fraction_to_points(&self, fraction: f32) -> f32 {
        fraction * self.width
    }
}

fn finite(value: f32) -> bool {
    value.is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometries() -> Vec<PageGeometry> {
        let mut all = Vec::new();
        for rotation in PageRotation::ALL {
            // An ordinary page at the origin…
            all.push(PageGeometry::new(0.0, 0.0, 612.0, 792.0, rotation, 1.0));
            // …and one whose crop box is offset inside a larger media box,
            // which is what a document trimmed for print looks like.
            all.push(PageGeometry::new(36.0, 54.0, 540.0, 684.0, rotation, 1.0));
        }
        all
    }

    #[test]
    fn canonical_and_user_space_round_trip_at_every_rotation_and_crop() {
        for geometry in geometries() {
            for point in [
                PagePoint::new(0.0, 0.0),
                PagePoint::new(geometry.width, geometry.height),
                PagePoint::new(geometry.width * 0.25, geometry.height * 0.75),
                // Outside the page but inside the documented margin.
                PagePoint::new(-12.0, geometry.height + 8.0),
            ] {
                let (x, y) = geometry.to_user_space(point);
                let back = geometry.from_user_space(x, y);
                assert!(
                    (back.x - point.x).abs() < 1e-3 && (back.y - point.y).abs() < 1e-3,
                    "{geometry:?} lost {point:?}, got {back:?}"
                );
            }
        }
    }

    #[test]
    fn the_canonical_origin_is_the_crop_boxs_top_left_corner() {
        let geometry = PageGeometry::new(36.0, 54.0, 540.0, 684.0, PageRotation::None, 1.0);
        let (x, y) = geometry.to_user_space(PagePoint::new(0.0, 0.0));
        assert!((x - 36.0).abs() < 1e-4, "x was {x}");
        // Top-left in canonical space is the *top* of the crop box in user
        // space: its bottom plus its height.
        assert!((y - (54.0 + 684.0)).abs() < 1e-4, "y was {y}");
    }

    #[test]
    fn y_grows_downward_in_canonical_space_and_upward_in_user_space() {
        let geometry = PageGeometry::upright(612.0, 792.0);
        let top = geometry.to_user_space(PagePoint::new(0.0, 10.0));
        let below = geometry.to_user_space(PagePoint::new(0.0, 20.0));
        assert!(below.1 < top.1, "a larger canonical y is lower on the page");
    }

    #[test]
    fn rotation_exchanges_the_displayed_dimensions() {
        let upright = PageGeometry::new(0.0, 0.0, 612.0, 792.0, PageRotation::None, 1.0);
        assert_eq!((upright.width, upright.height), (612.0, 792.0));
        let turned = PageGeometry::new(0.0, 0.0, 612.0, 792.0, PageRotation::Clockwise90, 1.0);
        assert_eq!((turned.width, turned.height), (792.0, 612.0));
        assert!(PageRotation::Clockwise270.swaps_axes());
        assert!(!PageRotation::Clockwise180.swaps_axes());
    }

    #[test]
    fn a_rotated_pages_top_left_is_a_corner_of_the_unrotated_crop_box() {
        let geometry = PageGeometry::new(0.0, 0.0, 612.0, 792.0, PageRotation::Clockwise90, 1.0);
        // Turning the sheet 90° clockwise puts its lower-left corner at the
        // top left of what the reader sees.
        let (x, y) = geometry.to_user_space(PagePoint::new(0.0, 0.0));
        assert!((x - 0.0).abs() < 1e-4 && (y - 0.0).abs() < 1e-4, "{x},{y}");
    }

    #[test]
    fn a_rect_stays_a_well_formed_pdf_rect_at_every_rotation() {
        for geometry in geometries() {
            let rect = PageRect::new(10.0, 20.0, 110.0, 220.0);
            let pdf = geometry.rect_to_user_space(rect);
            assert!(pdf[0] < pdf[2] && pdf[1] < pdf[3], "{pdf:?} is not ordered");
            let back = geometry.rect_from_user_space(pdf);
            assert!((back.width() - rect.width()).abs() < 1e-2);
            assert!((back.height() - rect.height()).abs() < 1e-2);
        }
    }

    #[test]
    fn quads_round_trip_corner_by_corner() {
        for geometry in geometries() {
            let quad = PageQuad::from_rect(PageRect::new(30.0, 40.0, 130.0, 60.0));
            let values = geometry.quad_to_user_space(quad);
            let back = geometry.quad_from_user_space(values);
            for (a, b) in back.corners().iter().zip(quad.corners().iter()) {
                assert!((a.x - b.x).abs() < 1e-2 && (a.y - b.y).abs() < 1e-2);
            }
        }
    }

    #[test]
    fn normalised_coordinates_convert_only_through_the_page() {
        let geometry = PageGeometry::upright(600.0, 800.0);
        let point = geometry.from_normalised(0.5, 0.25);
        assert_eq!(point, PagePoint::new(300.0, 200.0));
        let (nx, ny) = geometry.to_normalised(point);
        assert!((nx - 0.5).abs() < 1e-6 && (ny - 0.25).abs() < 1e-6);
        assert_eq!(geometry.width_fraction_to_points(0.004), 2.4);
    }

    #[test]
    fn geometry_outside_the_documented_margin_is_rejected() {
        let geometry = PageGeometry::upright(612.0, 792.0);
        assert!(PagePoint::new(-999.0, 0.0).is_valid_on(&geometry));
        assert!(!PagePoint::new(-1_001.0, 0.0).is_valid_on(&geometry));
        assert!(!PagePoint::new(f32::NAN, 0.0).is_valid_on(&geometry));
        assert!(!PagePoint::new(0.0, f32::INFINITY).is_valid_on(&geometry));
        assert!(geometry.bounds().is_valid_on(&geometry));
        // Inverted rectangles are not "empty", they are malformed.
        assert!(!PageRect::new(10.0, 10.0, 5.0, 20.0).is_valid_on(&geometry));
    }

    #[test]
    fn absurd_and_zero_pages_are_not_pages() {
        assert!(PageGeometry::upright(612.0, 792.0).is_valid());
        assert!(!PageGeometry::upright(0.0, 792.0).is_valid());
        assert!(!PageGeometry::upright(612.0, MAX_PAGE_POINTS + 1.0).is_valid());
        assert!(!PageGeometry::upright(f32::NAN, 792.0).is_valid());
    }

    #[test]
    fn a_user_unit_that_is_not_a_scale_falls_back_to_one() {
        let geometry = PageGeometry::new(0.0, 0.0, 612.0, 792.0, PageRotation::None, 0.0);
        assert_eq!(geometry.user_unit, 1.0);
        let geometry = PageGeometry::new(0.0, 0.0, 612.0, 792.0, PageRotation::None, 2.5);
        assert_eq!(geometry.user_unit, 2.5);
    }

    #[test]
    fn rotate_entries_that_are_not_multiples_of_ninety_are_treated_as_absent() {
        assert_eq!(PageRotation::from_degrees(90), PageRotation::Clockwise90);
        assert_eq!(PageRotation::from_degrees(-90), PageRotation::Clockwise270);
        assert_eq!(PageRotation::from_degrees(450), PageRotation::Clockwise90);
        assert_eq!(PageRotation::from_degrees(45), PageRotation::None);
        assert_eq!(PageRotation::from_degrees(360), PageRotation::None);
    }

    #[test]
    fn rectangles_enclose_grow_and_meet() {
        let rect = PageRect::enclosing([
            PagePoint::new(10.0, 20.0),
            PagePoint::new(5.0, 40.0),
            PagePoint::new(30.0, 15.0),
        ])
        .unwrap();
        assert_eq!(rect, PageRect::new(5.0, 15.0, 30.0, 40.0));
        assert_eq!(PageRect::enclosing([]), None);
        assert_eq!(rect.inflated(5.0), PageRect::new(0.0, 10.0, 35.0, 45.0));
        assert!(rect.contains(PagePoint::new(10.0, 20.0)));
        assert!(!rect.contains(PagePoint::new(10.0, 200.0)));
        assert!(rect.intersects(&PageRect::new(25.0, 35.0, 100.0, 100.0)));
        assert!(!rect.intersects(&PageRect::new(31.0, 35.0, 100.0, 100.0)));
        assert_eq!(
            rect.union(&PageRect::new(0.0, 0.0, 6.0, 6.0)),
            PageRect::new(0.0, 0.0, 30.0, 40.0)
        );
    }

    #[test]
    fn a_quad_with_no_area_is_degenerate() {
        let flat = PageQuad::from_rect(PageRect::new(10.0, 20.0, 110.0, 20.0));
        assert!(flat.is_degenerate());
        assert!(!PageQuad::from_rect(PageRect::new(10.0, 20.0, 110.0, 30.0)).is_degenerate());
    }

    #[test]
    fn pages_are_counted_from_one_when_a_human_reads_them() {
        assert_eq!(PageIndex(0).to_string(), "1");
        assert_eq!(PageIndex(11).get(), 11);
    }
}
