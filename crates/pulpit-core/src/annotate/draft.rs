//! What a finished gesture asks the document to do.
//!
//! A *draft* is a complete description of one annotation in canonical page
//! space, carrying no PDF handle and no object number, so it can be sent over
//! the worker protocol, written to the recovery journal and replayed against a
//! freshly opened document. A *command* is a draft plus what to do with it.
//!
//! Everything is validated before it is sent (A8): a draft that names a page
//! it cannot be on, carries more points than the limit, or resolves to no
//! geometry at all is rejected here rather than in the middle of a mutation.

use serde::{Deserialize, Serialize};

use crate::annotation::{InkColor, MarkupKind};
use crate::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect};

use super::id::AnnotationId;
use super::stroke::{InkPoint, MAX_INK_POINTS};

/// The most characters an annotation's text may carry.
///
/// Long enough for a paragraph of commentary or a page of selected text, short
/// enough that a document claiming a megabyte of `/Contents` is refused before
/// it is decoded.
pub const MAX_ANNOTATION_TEXT: usize = 16_384;

/// The most quadrilaterals one text-markup annotation may carry. A highlight
/// spanning several columns of a dense page runs to a few hundred runs; past
/// this the selection is the whole document.
pub const MAX_QUADS: usize = 4_096;

/// The user-facing kinds, and the PDF subtype each becomes (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationKind {
    /// `/Ink`
    Ink,
    /// `/Square`: a box drawn on the page, which is what PDF calls the
    /// rectangle rather than anything to do with equal sides.
    Square,
    /// `/Circle`: the ellipse inscribed in the annotation's rectangle, which
    /// is likewise not necessarily round.
    Circle,
    /// `/Highlight`, with `/QuadPoints` over extracted text.
    Highlight,
    /// `/Underline`, with the same `/QuadPoints`.
    Underline,
    /// `/StrikeOut`, likewise.
    StrikeOut,
    /// `/FreeText`
    FreeText,
    /// `/Text`
    Note,
    /// `/Stamp`
    Stamp,
    /// Anything else the document carries. Preserved, never rewritten (A5).
    Other,
}

impl AnnotationKind {
    pub fn label(self) -> &'static str {
        match self {
            AnnotationKind::Ink => "Ink",
            AnnotationKind::Square => "Rectangle",
            AnnotationKind::Circle => "Ellipse",
            AnnotationKind::Highlight => "Highlight",
            AnnotationKind::Underline => "Underline",
            AnnotationKind::StrikeOut => "Strikeout",
            AnnotationKind::FreeText => "Text",
            AnnotationKind::Note => "Note",
            AnnotationKind::Stamp => "Stamp",
            AnnotationKind::Other => "Annotation",
        }
    }

    /// The PDF `/Subtype` name.
    pub fn subtype(self) -> &'static str {
        match self {
            AnnotationKind::Ink => "Ink",
            AnnotationKind::Square => "Square",
            AnnotationKind::Circle => "Circle",
            AnnotationKind::Highlight => "Highlight",
            AnnotationKind::Underline => "Underline",
            AnnotationKind::StrikeOut => "StrikeOut",
            AnnotationKind::FreeText => "FreeText",
            AnnotationKind::Note => "Text",
            AnnotationKind::Stamp => "Stamp",
            AnnotationKind::Other => "Unknown",
        }
    }

    /// Can this kind be moved and resized freely?
    ///
    /// Text markup cannot: its `/QuadPoints` describe real text runs, and
    /// dragging the rectangle somewhere else would leave them describing text
    /// that is no longer under them (§8.4).
    pub fn is_freely_movable(self) -> bool {
        !self.is_text_markup() && !matches!(self, AnnotationKind::Other)
    }

    /// Is this one of the three marks the highlighter makes?
    ///
    /// The question almost every arm asking about `/Highlight` was really
    /// asking: the three share their geometry, their draft and every rule
    /// about what may be done to them, and differ only in what a viewer draws
    /// for them.
    pub fn is_text_markup(self) -> bool {
        self.markup().is_some()
    }

    /// Which mark this is, for the three the highlighter makes.
    pub fn markup(self) -> Option<MarkupKind> {
        match self {
            AnnotationKind::Highlight => Some(MarkupKind::Highlight),
            AnnotationKind::Underline => Some(MarkupKind::Underline),
            AnnotationKind::StrikeOut => Some(MarkupKind::StrikeOut),
            _ => None,
        }
    }
}

impl From<MarkupKind> for AnnotationKind {
    fn from(kind: MarkupKind) -> AnnotationKind {
        match kind {
            MarkupKind::Highlight => AnnotationKind::Highlight,
            MarkupKind::Underline => AnnotationKind::Underline,
            MarkupKind::StrikeOut => AnnotationKind::StrikeOut,
        }
    }
}

/// How a mark is painted.
///
/// Named `MarkStyle` rather than `AnnotationStyle` because
/// [`crate::annotation::AnnotationStyle`] already holds the presenter's
/// pointer and spotlight measures, which are a different thing that happens to
/// share the word. This one is about a durable annotation and is measured in
/// page points rather than fractions of a page.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarkStyle {
    pub color: InkColor,
    /// `/CA`, the constant opacity, `0.0`–`1.0`.
    pub opacity: f32,
    /// Border or stroke width in page points.
    pub width: f32,
    /// Text size in page points, for the kinds that set type.
    pub font_size: f32,
}

impl Default for MarkStyle {
    fn default() -> Self {
        Self {
            // Black, not red: a pen's resting colour is the one a document is
            // written in. Red is a choice, and choices are made in the
            // toolbar's options.
            color: InkColor::Black,
            opacity: 1.0,
            // Two points is a fine-liner on a letter page: visible at reading
            // zoom without swallowing the text it is drawn over.
            width: 2.0,
            font_size: 12.0,
        }
    }
}

/// Widths and sizes outside these are not a style choice, they are a mistake
/// or a hostile file.
pub const MARK_WIDTH_RANGE: (f32, f32) = (0.25, 72.0);
pub const FONT_SIZE_RANGE: (f32, f32) = (4.0, 288.0);

impl MarkStyle {
    /// The style a highlighter is born with: broad, translucent, yellow.
    pub fn highlighter() -> MarkStyle {
        MarkStyle::text_markup(MarkupKind::Highlight)
    }

    /// The style one of the highlighter's three marks is born with.
    ///
    /// The colour is the tool's, whichever mark it makes: it is one pen with
    /// three nibs, not three pens. Only the opacity parts company, because a
    /// wash that let the words through and a rule that did are not the same
    /// request (see [`MarkupKind::opacity`]).
    pub fn text_markup(kind: MarkupKind) -> MarkStyle {
        MarkStyle {
            color: InkColor::Yellow,
            opacity: kind.opacity(),
            ..MarkStyle::default()
        }
    }

    /// The style the select-text tool sweeps in: a cyan wash, fixed.
    ///
    /// Not the highlighter's yellow, so a sweep that will leave no mark never
    /// looks like one that will; cyan at a wash's translucency is what a text
    /// selection looks like on the rest of the desktop. Fixed because the
    /// tool has no options: a selection is chrome, not a mark anybody styles.
    pub fn selection() -> MarkStyle {
        MarkStyle {
            color: InkColor::Cyan,
            opacity: 0.35,
            ..MarkStyle::default()
        }
    }

    /// The style a sticky note is born with: opaque yellow.
    ///
    /// A note is an icon on the page rather than a stroke over it, and `/C` is
    /// what every viewer paints that icon with. The pen's black would put a
    /// black tab on the page in one viewer and pulpit's own drawing in
    /// another; yellow is the colour a sticky note is everywhere else.
    pub fn note() -> MarkStyle {
        MarkStyle {
            color: InkColor::Yellow,
            ..MarkStyle::default()
        }
    }

    pub fn sanitised(mut self) -> MarkStyle {
        let repair = |value: f32, range: (f32, f32), fallback: f32| {
            if value.is_finite() {
                value.clamp(range.0, range.1)
            } else {
                fallback
            }
        };
        self.opacity = if self.opacity.is_finite() {
            self.opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.width = repair(self.width, MARK_WIDTH_RANGE, MarkStyle::default().width);
        self.font_size = repair(
            self.font_size,
            FONT_SIZE_RANGE,
            MarkStyle::default().font_size,
        );
        self
    }
}

/// What a stamp shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StampMark {
    Check,
    Cross,
    /// A visible signature or other picture, already decoded to RGBA.
    ///
    /// Never a path: an annotation that pointed at a file on disk would either
    /// break when the file moved or read a file the document chose (§12).
    Image {
        pixel_width: u32,
        pixel_height: u32,
        rgba: Vec<u8>,
    },
}

impl From<crate::annotation::StampChoice> for StampMark {
    fn from(choice: crate::annotation::StampChoice) -> StampMark {
        match choice {
            crate::annotation::StampChoice::Check => StampMark::Check,
            crate::annotation::StampChoice::Cross => StampMark::Cross,
        }
    }
}

impl StampMark {
    /// The largest picture a stamp may carry. Four megapixels is far past what
    /// a signature or a mark on a page is rendered at, and bounds the decode.
    pub const MAX_PIXELS: u64 = 2048 * 2048;

    pub fn label(&self) -> &'static str {
        match self {
            StampMark::Check => "Check",
            StampMark::Cross => "Cross",
            // Never "signature": a picture of a signature is not a
            // cryptographic one, and the UI must not suggest it is (§1).
            StampMark::Image { .. } => "Mark",
        }
    }

    fn is_consistent(&self) -> bool {
        match self {
            StampMark::Check | StampMark::Cross => true,
            StampMark::Image {
                pixel_width,
                pixel_height,
                rgba,
            } => {
                *pixel_width > 0
                    && *pixel_height > 0
                    && u64::from(*pixel_width) * u64::from(*pixel_height) <= Self::MAX_PIXELS
                    && rgba.len() == *pixel_width as usize * *pixel_height as usize * 4
            }
        }
    }
}

/// Where a free-text annotation's content came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextSource {
    /// Plain text, written straight into `/Contents` (§7.3).
    Plain,
    /// Typst markup, compiled to an appearance with the source kept in
    /// pulpit's namespaced metadata (§7.4).
    Typst,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkDraft {
    pub page: PageIndex,
    pub points: Vec<InkPoint>,
    pub style: MarkStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HighlightDraft {
    pub page: PageIndex,
    /// Which of the three marks this is. Defaulted when absent so a journal
    /// written before underlines existed replays as the highlights it meant.
    #[serde(default)]
    pub kind: MarkupKind,
    /// One quadrilateral per contiguous run of selected text, in reading
    /// order. Normative geometry, not a hint (§7.2).
    pub quads: Vec<PageQuad>,
    /// The selected text, carried into `/Contents` so the mark is recoverable
    /// by a reader that re-extracts the page.
    pub text: String,
    pub style: MarkStyle,
}

/// The two shapes PDF has an annotation subtype of its own for.
///
/// [`crate::annotation::ShapeKind`] is what the *tool* is set to and has four
/// values; this is what a draft can be, and has two. A line and an arrow are
/// not here because they are drafted as `/Ink` — see `ShapeKind`'s own note
/// for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShapeOutline {
    /// `/Square`: the rectangle itself.
    Box,
    /// `/Circle`: the ellipse inscribed in it.
    Ellipse,
}

impl ShapeOutline {
    pub fn kind(self) -> AnnotationKind {
        match self {
            ShapeOutline::Box => AnnotationKind::Square,
            ShapeOutline::Ellipse => AnnotationKind::Circle,
        }
    }
}

/// A box or an ellipse, bounded by the rectangle the hand pulled.
///
/// The rectangle is the annotation's `/Rect`, and the shape is drawn inside
/// it inset by half the border width, which is where PDF 12.5.6.8 puts a
/// square's border and what keeps a thick outline from being clipped by the
/// annotation's own bounds in viewers that honour them strictly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeDraft {
    pub page: PageIndex,
    pub outline: ShapeOutline,
    pub rect: PageRect,
    pub style: MarkStyle,
}

/// How many segments an ellipse is drawn as when it has to be a polyline.
///
/// Only the preview needs one — the mark itself is written as four Bézier
/// curves — and at any zoom a screen can show, sixty-four segments is a
/// smooth curve. Even, so the polyline is symmetric about both axes.
pub const ELLIPSE_SEGMENTS: usize = 64;

/// The polyline a shape is previewed as while the hand is still drawing it.
///
/// One function for all four kinds because the preview draws one thing — a
/// stroked path — and because two of the four commit exactly these points as
/// their `/InkList`. A preview built from different arithmetic than the mark
/// is a preview that can disagree with what lands on the page.
///
/// `from` and `to` are the corners the drag visited, in order: an arrow's
/// head goes on `to`.
pub fn shape_outline(
    kind: crate::annotation::ShapeKind,
    from: PagePoint,
    to: PagePoint,
    width: f32,
) -> Vec<PagePoint> {
    use crate::annotation::ShapeKind;

    match kind {
        ShapeKind::Rectangle => {
            let rect = PageRect::enclosing([from, to])
                .unwrap_or(PageRect::new(from.x, from.y, from.x, from.y));
            vec![
                PagePoint::new(rect.left, rect.top),
                PagePoint::new(rect.right, rect.top),
                PagePoint::new(rect.right, rect.bottom),
                PagePoint::new(rect.left, rect.bottom),
                // Back to the start: the painter strokes an open polyline, so
                // the closing side is a segment like any other.
                PagePoint::new(rect.left, rect.top),
            ]
        }
        ShapeKind::Ellipse => {
            let rect = PageRect::enclosing([from, to])
                .unwrap_or(PageRect::new(from.x, from.y, from.x, from.y));
            let (cx, cy) = (
                (rect.left + rect.right) / 2.0,
                (rect.top + rect.bottom) / 2.0,
            );
            let (rx, ry) = (rect.width() / 2.0, rect.height() / 2.0);
            (0..=ELLIPSE_SEGMENTS)
                .map(|step| {
                    let angle = step as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
                    PagePoint::new(cx + rx * angle.cos(), cy + ry * angle.sin())
                })
                .collect()
        }
        ShapeKind::Line => vec![from, to],
        ShapeKind::Arrow => arrow_outline(from, to, width),
    }
}

/// How long an arrowhead's barbs are, as a multiple of the line's width.
const ARROWHEAD_WIDTHS: f32 = 5.0;
/// …and never shorter than this, in page points, so a hairline arrow still
/// has a head somebody can see.
const ARROWHEAD_MIN: f32 = 7.0;
/// …nor longer than this fraction of the shaft, so a short arrow is a short
/// arrow rather than two crossed barbs.
const ARROWHEAD_MAX_SHARE: f32 = 0.4;
/// Half the angle between the barbs and the shaft.
const ARROWHEAD_HALF_ANGLE: f32 = 0.42;

/// The shaft and the head, as one stroke that doubles back.
///
/// One path rather than three, because `/Ink` draws each path in its list
/// with the same pen and a head made of separate paths would be three marks
/// to erase, to move and to select. Retracing the shaft's last stretch costs
/// nothing on a stroked path.
fn arrow_outline(from: PagePoint, to: PagePoint, width: f32) -> Vec<PagePoint> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let length = (dx * dx + dy * dy).sqrt();
    if !length.is_finite() || length <= f32::EPSILON {
        return vec![from, to];
    }
    let head = (width * ARROWHEAD_WIDTHS)
        .max(ARROWHEAD_MIN)
        .min(length * ARROWHEAD_MAX_SHARE);
    // The direction the shaft came *from*, which is where the barbs point.
    let angle = dy.atan2(dx) + std::f32::consts::PI;
    let barb = |offset: f32| {
        let angle = angle + offset;
        PagePoint::new(to.x + head * angle.cos(), to.y + head * angle.sin())
    };
    vec![
        from,
        to,
        barb(ARROWHEAD_HALF_ANGLE),
        to,
        barb(-ARROWHEAD_HALF_ANGLE),
    ]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreeTextDraft {
    pub page: PageIndex,
    pub rect: PageRect,
    pub text: String,
    pub source: TextSource,
    pub style: MarkStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteDraft {
    pub page: PageIndex,
    /// The note's anchor: the corner of the icon, not a rectangle, because a
    /// `/Text` annotation is drawn at a fixed size whatever its `/Rect` says.
    pub at: PagePoint,
    pub text: String,
    pub style: MarkStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StampDraft {
    pub page: PageIndex,
    pub rect: PageRect,
    pub mark: StampMark,
    pub style: MarkStyle,
    /// The Typst markup this mark was generated from, when it was (§7.4).
    ///
    /// Kept in pulpit's own namespaced entry so other viewers show the
    /// appearance and are not asked to understand Typst, while pulpit can
    /// reopen the source for editing. Absent for a check, a cross or a
    /// picture somebody dropped in.
    #[serde(default)]
    pub source: Option<String>,
}

/// One annotation, fully described, in canonical page space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationDraft {
    Ink(InkDraft),
    Shape(ShapeDraft),
    Highlight(HighlightDraft),
    FreeText(FreeTextDraft),
    Note(NoteDraft),
    Stamp(StampDraft),
}

/// Why a draft is not something that can be written to a page.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DraftError {
    #[error("the mark is empty")]
    Empty,
    #[error("{what} exceeds the limit of {limit}")]
    TooLarge { what: &'static str, limit: usize },
    #[error("the mark is not on the page")]
    OffPage,
    #[error("the mark's geometry is not a number")]
    NotFinite,
    #[error("the picture is malformed")]
    MalformedImage,
    #[error("page {0} is not in this document")]
    NoSuchPage(usize),
}

impl AnnotationDraft {
    pub fn kind(&self) -> AnnotationKind {
        match self {
            AnnotationDraft::Ink(_) => AnnotationKind::Ink,
            AnnotationDraft::Shape(d) => d.outline.kind(),
            AnnotationDraft::Highlight(d) => d.kind.into(),
            AnnotationDraft::FreeText(_) => AnnotationKind::FreeText,
            AnnotationDraft::Note(_) => AnnotationKind::Note,
            AnnotationDraft::Stamp(_) => AnnotationKind::Stamp,
        }
    }

    pub fn page(&self) -> PageIndex {
        match self {
            AnnotationDraft::Ink(d) => d.page,
            AnnotationDraft::Shape(d) => d.page,
            AnnotationDraft::Highlight(d) => d.page,
            AnnotationDraft::FreeText(d) => d.page,
            AnnotationDraft::Note(d) => d.page,
            AnnotationDraft::Stamp(d) => d.page,
        }
    }

    pub fn style(&self) -> MarkStyle {
        match self {
            AnnotationDraft::Ink(d) => d.style,
            AnnotationDraft::Shape(d) => d.style,
            AnnotationDraft::Highlight(d) => d.style,
            AnnotationDraft::FreeText(d) => d.style,
            AnnotationDraft::Note(d) => d.style,
            AnnotationDraft::Stamp(d) => d.style,
        }
    }

    /// Bring every measure back into range. Applied on the way in from a UI,
    /// a protocol message and the recovery journal alike.
    pub fn sanitise(&mut self) {
        let style = self.style().sanitised();
        match self {
            AnnotationDraft::Ink(d) => d.style = style,
            AnnotationDraft::Shape(d) => d.style = style,
            AnnotationDraft::Highlight(d) => d.style = style,
            AnnotationDraft::FreeText(d) => d.style = style,
            AnnotationDraft::Note(d) => d.style = style,
            AnnotationDraft::Stamp(d) => d.style = style,
        }
    }

    /// The rectangle the annotation occupies, which becomes its `/Rect`.
    ///
    /// For ink this is the stroke's bounds grown by half its painted width,
    /// because a `/Rect` that clipped the stroke it encloses is exactly the
    /// bug that makes a mark disappear at its edges in another viewer.
    pub fn bounds(&self) -> Option<PageRect> {
        match self {
            AnnotationDraft::Ink(d) => PageRect::enclosing(d.points.iter().map(|p| p.at))
                .map(|rect| rect.inflated(d.style.width / 2.0 + 1.0)),
            // The rectangle *is* the mark: PDF 12.5.6.8 draws a square's
            // border inside its `/Rect`, so the border needs no room made for
            // it the way a stroke's does.
            AnnotationDraft::Shape(d) => Some(d.rect),
            AnnotationDraft::Highlight(d) => d
                .quads
                .iter()
                .map(|quad| quad.bounds())
                .reduce(|a, b| a.union(&b)),
            AnnotationDraft::FreeText(d) => Some(d.rect),
            // A `/Text` icon is drawn at a viewer-chosen size; 20 points
            // square is what every common viewer uses, and the `/Rect` has to
            // say something.
            AnnotationDraft::Note(d) => Some(PageRect::new(
                d.at.x,
                d.at.y,
                d.at.x + NOTE_ICON_POINTS,
                d.at.y + NOTE_ICON_POINTS,
            )),
            AnnotationDraft::Stamp(d) => Some(d.rect),
        }
    }

    /// The same mark, moved by `dx` and `dy` page points.
    ///
    /// `None` for a kind that cannot be moved freely: `/Highlight`'s
    /// `/QuadPoints` describe real text runs, and dragging them elsewhere
    /// would leave them describing text that is no longer under them (§8.4).
    pub fn translated(&self, dx: f32, dy: f32) -> Option<AnnotationDraft> {
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        let shift = |rect: PageRect| {
            PageRect::new(
                rect.left + dx,
                rect.top + dy,
                rect.right + dx,
                rect.bottom + dy,
            )
        };
        match self {
            AnnotationDraft::Ink(ink) => {
                let mut moved = ink.clone();
                for point in &mut moved.points {
                    point.at = PagePoint::new(point.at.x + dx, point.at.y + dy);
                }
                Some(AnnotationDraft::Ink(moved))
            }
            AnnotationDraft::FreeText(free) => Some(AnnotationDraft::FreeText(FreeTextDraft {
                rect: shift(free.rect),
                ..free.clone()
            })),
            AnnotationDraft::Note(note) => Some(AnnotationDraft::Note(NoteDraft {
                at: PagePoint::new(note.at.x + dx, note.at.y + dy),
                ..note.clone()
            })),
            AnnotationDraft::Stamp(stamp) => Some(AnnotationDraft::Stamp(StampDraft {
                rect: shift(stamp.rect),
                ..stamp.clone()
            })),
            AnnotationDraft::Shape(shape) => Some(AnnotationDraft::Shape(ShapeDraft {
                rect: shift(shape.rect),
                ..shape.clone()
            })),
            AnnotationDraft::Highlight(_) => None,
        }
    }

    /// The same mark scaled out of `from` and into `to`.
    ///
    /// The companion to [`AnnotationDraft::translated`], for the corner drags
    /// rather than the middle one. A highlight refuses for the same reason it
    /// refuses a move: its quads describe real text runs, and stretching them
    /// would claim the mark covers words it does not (§8.4).
    ///
    /// A note refuses too, but for its own reason: a `/Text` annotation is
    /// drawn at a fixed size whatever its `/Rect` says, so a resized note would
    /// look exactly like an unresized one and the handles would be a lie.
    pub fn resized(&self, from: PageRect, to: PageRect) -> Option<AnnotationDraft> {
        let (from_width, from_height) = (from.width(), from.height());
        if !to.left.is_finite()
            || !to.top.is_finite()
            || !to.right.is_finite()
            || !to.bottom.is_finite()
            || from_width <= f32::EPSILON
            || from_height <= f32::EPSILON
        {
            return None;
        }
        match self {
            AnnotationDraft::FreeText(free) => Some(AnnotationDraft::FreeText(FreeTextDraft {
                rect: to,
                ..free.clone()
            })),
            AnnotationDraft::Stamp(stamp) => Some(AnnotationDraft::Stamp(StampDraft {
                rect: to,
                ..stamp.clone()
            })),
            // A box or an ellipse is its rectangle, so resizing one is
            // exactly the new rectangle and nothing else has to follow.
            AnnotationDraft::Shape(shape) => Some(AnnotationDraft::Shape(ShapeDraft {
                rect: to,
                ..shape.clone()
            })),
            AnnotationDraft::Ink(ink) => {
                // The stroke's points follow the box that held them, so a
                // scribble stretched wider is the same scribble drawn larger
                // and not a new one somewhere else.
                let (scale_x, scale_y) = (to.width() / from_width, to.height() / from_height);
                let mut moved = ink.clone();
                for point in &mut moved.points {
                    point.at = PagePoint::new(
                        to.left + (point.at.x - from.left) * scale_x,
                        to.top + (point.at.y - from.top) * scale_y,
                    );
                }
                Some(AnnotationDraft::Ink(moved))
            }
            AnnotationDraft::Highlight(_) | AnnotationDraft::Note(_) => None,
        }
    }

    /// Can this kind of mark be dragged by a corner at all?
    ///
    /// What the view asks before it draws handles: offering a grip that does
    /// nothing is worse than offering none.
    pub fn is_resizable(&self) -> bool {
        matches!(
            self,
            AnnotationDraft::FreeText(_)
                | AnnotationDraft::Stamp(_)
                | AnnotationDraft::Ink(_)
                | AnnotationDraft::Shape(_)
        )
    }

    /// What this mark says, for the kinds that say anything.
    pub fn text(&self) -> Option<&str> {
        match self {
            AnnotationDraft::FreeText(free) => Some(&free.text),
            AnnotationDraft::Note(note) => Some(&note.text),
            AnnotationDraft::Highlight(highlight) => Some(&highlight.text),
            AnnotationDraft::Ink(_) | AnnotationDraft::Stamp(_) | AnnotationDraft::Shape(_) => None,
        }
    }

    /// The same mark, saying something else.
    ///
    /// Geometry is untouched: re-writing a note is not moving it, and a
    /// highlight keeps the `/QuadPoints` that say which words it is about even
    /// when the comment attached to them changes (§8.5).
    pub fn with_text(&self, text: String) -> Option<AnnotationDraft> {
        match self {
            AnnotationDraft::FreeText(free) => Some(AnnotationDraft::FreeText(FreeTextDraft {
                text,
                ..free.clone()
            })),
            AnnotationDraft::Note(note) => Some(AnnotationDraft::Note(NoteDraft {
                text,
                ..note.clone()
            })),
            AnnotationDraft::Highlight(highlight) => {
                Some(AnnotationDraft::Highlight(HighlightDraft {
                    text,
                    ..highlight.clone()
                }))
            }
            AnnotationDraft::Ink(_) | AnnotationDraft::Stamp(_) | AnnotationDraft::Shape(_) => None,
        }
    }

    /// Is this something that can be written to `page`?
    pub fn validate(&self, page: &PageGeometry) -> Result<(), DraftError> {
        match self {
            AnnotationDraft::Ink(d) => {
                if d.points.is_empty() {
                    return Err(DraftError::Empty);
                }
                if d.points.len() > MAX_INK_POINTS {
                    return Err(DraftError::TooLarge {
                        what: "ink points",
                        limit: MAX_INK_POINTS,
                    });
                }
                if d.points
                    .iter()
                    .any(|p| !p.at.x.is_finite() || !p.at.y.is_finite())
                {
                    return Err(DraftError::NotFinite);
                }
                // *Every* point must be placeable, not merely the bounds: a
                // stroke that wanders a mile off the sheet and comes back
                // would otherwise pass on its bounding box alone.
                if !d.points.iter().all(|p| p.at.is_valid_on(page)) {
                    return Err(DraftError::OffPage);
                }
            }
            AnnotationDraft::Highlight(d) => {
                if d.quads.is_empty() {
                    return Err(DraftError::Empty);
                }
                if d.quads.len() > MAX_QUADS {
                    return Err(DraftError::TooLarge {
                        what: "quadrilaterals",
                        limit: MAX_QUADS,
                    });
                }
                if d.quads.iter().all(PageQuad::is_degenerate) {
                    return Err(DraftError::Empty);
                }
                if !d.quads.iter().all(|quad| quad.is_valid_on(page)) {
                    return Err(DraftError::OffPage);
                }
                check_text(&d.text)?;
            }
            AnnotationDraft::FreeText(d) => {
                check_text(&d.text)?;
                if d.text.trim().is_empty() {
                    return Err(DraftError::Empty);
                }
                check_rect(d.rect, page)?;
            }
            AnnotationDraft::Note(d) => {
                check_text(&d.text)?;
                // An empty note is an icon on a page with nothing behind it:
                // the reader who clicks it learns nothing, and the reader who
                // sees it wonders what they missed. Same rule as free text.
                if d.text.trim().is_empty() {
                    return Err(DraftError::Empty);
                }
                if !d.at.is_valid_on(page) {
                    return Err(DraftError::OffPage);
                }
            }
            AnnotationDraft::Stamp(d) => {
                if !d.mark.is_consistent() {
                    return Err(DraftError::MalformedImage);
                }
                check_rect(d.rect, page)?;
            }
            // A shape is its rectangle, so `check_rect` is the whole of it:
            // finite, on the page, and not dragged down to nothing.
            AnnotationDraft::Shape(d) => check_rect(d.rect, page)?,
        }
        Ok(())
    }
}

/// The side of the icon a `/Text` annotation is conventionally drawn at.
pub const NOTE_ICON_POINTS: f32 = 20.0;

fn check_text(text: &str) -> Result<(), DraftError> {
    if text.len() > MAX_ANNOTATION_TEXT {
        return Err(DraftError::TooLarge {
            what: "text",
            limit: MAX_ANNOTATION_TEXT,
        });
    }
    Ok(())
}

fn check_rect(rect: PageRect, page: &PageGeometry) -> Result<(), DraftError> {
    if !rect.is_finite() {
        return Err(DraftError::NotFinite);
    }
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(DraftError::Empty);
    }
    if !rect.is_valid_on(page) {
        return Err(DraftError::OffPage);
    }
    Ok(())
}

/// One thing to do to the document's annotations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationCommand {
    Create(AnnotationDraft),
    Replace {
        id: AnnotationId,
        replacement: AnnotationDraft,
    },
    Delete {
        id: AnnotationId,
    },
}

impl AnnotationCommand {
    pub fn draft(&self) -> Option<&AnnotationDraft> {
        match self {
            AnnotationCommand::Create(draft) => Some(draft),
            AnnotationCommand::Replace { replacement, .. } => Some(replacement),
            AnnotationCommand::Delete { .. } => None,
        }
    }

    pub fn id(&self) -> Option<&AnnotationId> {
        match self {
            AnnotationCommand::Create(_) => None,
            AnnotationCommand::Replace { id, .. } | AnnotationCommand::Delete { id } => Some(id),
        }
    }

    /// What the user would call this in an undo menu.
    pub fn label(&self) -> String {
        match self {
            AnnotationCommand::Create(draft) => format!("Add {}", draft.kind().label()),
            AnnotationCommand::Replace { replacement, .. } => {
                format!("Edit {}", replacement.kind().label())
            }
            AnnotationCommand::Delete { .. } => "Erase".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PageGeometry {
        PageGeometry::upright(612.0, 792.0)
    }

    fn ink(points: Vec<InkPoint>) -> AnnotationDraft {
        AnnotationDraft::Ink(InkDraft {
            page: PageIndex(0),
            points,
            style: MarkStyle::default(),
        })
    }

    fn free_text(text: &str) -> AnnotationDraft {
        AnnotationDraft::FreeText(FreeTextDraft {
            page: PageIndex(0),
            rect: PageRect::new(100.0, 100.0, 300.0, 140.0),
            text: text.into(),
            source: TextSource::Plain,
            style: MarkStyle::default(),
        })
    }

    #[test]
    fn resizing_a_stroke_carries_its_points_with_the_box() {
        // Otherwise a stretched scribble is a new scribble somewhere else.
        let stroke = ink(vec![InkPoint::new(10.0, 10.0), InkPoint::new(20.0, 30.0)]);
        let from = PageRect::new(10.0, 10.0, 20.0, 30.0);
        let AnnotationDraft::Ink(scaled) = stroke
            .resized(from, PageRect::new(10.0, 10.0, 30.0, 50.0))
            .expect("a stroke is resizable")
        else {
            panic!("a stroke resizes to a stroke")
        };
        assert_eq!(scaled.points[0].at, PagePoint::new(10.0, 10.0));
        assert_eq!(scaled.points[1].at, PagePoint::new(30.0, 50.0));
    }

    #[test]
    fn the_kinds_that_cannot_be_reshaped_refuse_rather_than_lie() {
        // A highlight's quads describe real text runs; a note is drawn at a
        // fixed size whatever its rect says. Stretching either would produce a
        // mark claiming something untrue about the page (§8.4).
        let from = PageRect::new(0.0, 0.0, 10.0, 10.0);
        let to = PageRect::new(0.0, 0.0, 40.0, 40.0);
        let highlight = AnnotationDraft::Highlight(HighlightDraft {
            kind: MarkupKind::Highlight,
            page: PageIndex(0),
            quads: vec![PageQuad::from_rect(from)],
            text: "words".into(),
            style: MarkStyle::highlighter(),
        });
        assert!(highlight.resized(from, to).is_none());
        assert!(!highlight.is_resizable());

        let note = AnnotationDraft::Note(NoteDraft {
            page: PageIndex(0),
            at: PagePoint::new(0.0, 0.0),
            text: "a note".into(),
            style: MarkStyle::default(),
        });
        assert!(note.resized(from, to).is_none());
        assert!(!note.is_resizable());
    }

    #[test]
    fn a_resize_out_of_a_box_with_no_area_is_refused_rather_than_dividing_by_it() {
        // Refused for every kind rather than only for the one that divides by
        // it: a drag that started from a mark with no area is a drag from a
        // measurement nobody can have aimed, whatever the mark turns out to be.
        let flat = PageRect::new(10.0, 10.0, 10.0, 10.0);
        let to = PageRect::new(0.0, 0.0, 40.0, 40.0);
        assert!(free_text("something").resized(flat, to).is_none());
        assert!(ink(vec![InkPoint::new(10.0, 10.0)])
            .resized(flat, to)
            .is_none());
        // …and a resize to something that is not a number, likewise.
        let real = PageRect::new(0.0, 0.0, 10.0, 10.0);
        let nonsense = PageRect::new(0.0, 0.0, f32::NAN, 10.0);
        assert!(free_text("something").resized(real, nonsense).is_none());
    }

    #[test]
    fn rewriting_a_mark_changes_what_it_says_and_nothing_else() {
        let AnnotationDraft::FreeText(rewritten) = free_text("a first thought")
            .with_text("a second thought".into())
            .expect("free text says something")
        else {
            panic!("free text rewrites to free text")
        };
        assert_eq!(rewritten.text, "a second thought");
        assert_eq!(
            rewritten.rect,
            PageRect::new(100.0, 100.0, 300.0, 140.0),
            "rewriting a mark is not moving it"
        );
        assert_eq!(free_text("x").text(), Some("x"));

        // A stroke and a stamp say nothing, and guessing at text for them
        // would replace a mark with a different kind of mark.
        let stroke = ink(vec![InkPoint::new(1.0, 1.0)]);
        assert!(stroke.text().is_none());
        assert!(stroke.with_text("hello".into()).is_none());
    }

    #[test]
    fn an_empty_stroke_is_not_an_annotation_but_a_single_point_is() {
        assert_eq!(ink(vec![]).validate(&page()), Err(DraftError::Empty));
        // A one-point stroke is a dot, which is a mark somebody meant to make
        // (§7.1); it is the *empty* gesture that is residue.
        assert!(ink(vec![InkPoint::new(10.0, 10.0)])
            .validate(&page())
            .is_ok());
    }

    #[test]
    fn a_stroke_that_wanders_off_the_page_is_rejected_even_if_it_comes_back() {
        let draft = ink(vec![
            InkPoint::new(10.0, 10.0),
            InkPoint::new(90_000.0, 10.0),
            InkPoint::new(20.0, 10.0),
        ]);
        assert_eq!(draft.validate(&page()), Err(DraftError::OffPage));
    }

    #[test]
    fn a_stroke_with_more_points_than_the_limit_is_refused_before_anything_is_written() {
        let points = vec![InkPoint::new(1.0, 1.0); MAX_INK_POINTS + 1];
        assert!(matches!(
            ink(points).validate(&page()),
            Err(DraftError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_stroke_carrying_a_non_number_is_refused() {
        let draft = ink(vec![
            InkPoint::new(10.0, 10.0),
            InkPoint::new(f32::NAN, 1.0),
        ]);
        assert_eq!(draft.validate(&page()), Err(DraftError::NotFinite));
    }

    #[test]
    fn an_ink_rect_encloses_the_painted_width_rather_than_the_centre_line() {
        let draft = ink(vec![
            InkPoint::new(100.0, 100.0),
            InkPoint::new(200.0, 100.0),
        ]);
        let bounds = draft.bounds().unwrap();
        assert!(bounds.left < 100.0 && bounds.right > 200.0);
        assert!(
            bounds.height() > 0.0,
            "a horizontal stroke still has height"
        );
    }

    #[test]
    fn a_highlight_needs_quads_that_mark_something() {
        let mut draft = HighlightDraft {
            kind: MarkupKind::Highlight,
            page: PageIndex(0),
            quads: Vec::new(),
            text: "hello".into(),
            style: MarkStyle::highlighter(),
        };
        assert_eq!(
            AnnotationDraft::Highlight(draft.clone()).validate(&page()),
            Err(DraftError::Empty)
        );
        // A selection that resolved to nothing but flat quads marks nothing.
        draft.quads = vec![PageQuad::from_rect(PageRect::new(10.0, 20.0, 100.0, 20.0))];
        assert_eq!(
            AnnotationDraft::Highlight(draft.clone()).validate(&page()),
            Err(DraftError::Empty)
        );
        draft.quads = vec![PageQuad::from_rect(PageRect::new(10.0, 20.0, 100.0, 32.0))];
        assert!(AnnotationDraft::Highlight(draft).validate(&page()).is_ok());
    }

    #[test]
    fn text_beyond_the_limit_is_refused() {
        let draft = AnnotationDraft::FreeText(FreeTextDraft {
            page: PageIndex(0),
            rect: PageRect::new(10.0, 10.0, 200.0, 60.0),
            text: "x".repeat(MAX_ANNOTATION_TEXT + 1),
            source: TextSource::Plain,
            style: MarkStyle::default(),
        });
        assert!(matches!(
            draft.validate(&page()),
            Err(DraftError::TooLarge { .. })
        ));
    }

    #[test]
    fn empty_free_text_is_not_committed() {
        let draft = AnnotationDraft::FreeText(FreeTextDraft {
            page: PageIndex(0),
            rect: PageRect::new(10.0, 10.0, 200.0, 60.0),
            text: "   \n ".into(),
            source: TextSource::Plain,
            style: MarkStyle::default(),
        });
        assert_eq!(draft.validate(&page()), Err(DraftError::Empty));
    }

    #[test]
    fn an_empty_note_is_not_committed_either() {
        let mut draft = NoteDraft {
            page: PageIndex(0),
            at: PagePoint::new(100.0, 100.0),
            text: String::new(),
            style: MarkStyle::default(),
        };
        assert_eq!(
            AnnotationDraft::Note(draft.clone()).validate(&page()),
            Err(DraftError::Empty)
        );
        draft.text = "  \n ".into();
        assert_eq!(
            AnnotationDraft::Note(draft.clone()).validate(&page()),
            Err(DraftError::Empty)
        );
        draft.text = "remember this".into();
        assert!(AnnotationDraft::Note(draft).validate(&page()).is_ok());
    }

    #[test]
    fn a_stamp_with_a_picture_that_does_not_match_its_dimensions_is_malformed() {
        let draft = AnnotationDraft::Stamp(StampDraft {
            page: PageIndex(0),
            rect: PageRect::new(10.0, 10.0, 100.0, 60.0),
            mark: StampMark::Image {
                pixel_width: 4,
                pixel_height: 4,
                rgba: vec![0; 8],
            },
            style: MarkStyle::default(),
            source: None,
        });
        assert_eq!(draft.validate(&page()), Err(DraftError::MalformedImage));

        let huge = AnnotationDraft::Stamp(StampDraft {
            page: PageIndex(0),
            rect: PageRect::new(10.0, 10.0, 100.0, 60.0),
            mark: StampMark::Image {
                pixel_width: 40_000,
                pixel_height: 40_000,
                rgba: Vec::new(),
            },
            style: MarkStyle::default(),
            source: None,
        });
        assert_eq!(huge.validate(&page()), Err(DraftError::MalformedImage));
    }

    #[test]
    fn a_zero_area_rectangle_is_not_an_annotation() {
        let draft = AnnotationDraft::Stamp(StampDraft {
            page: PageIndex(0),
            rect: PageRect::new(10.0, 10.0, 10.0, 60.0),
            mark: StampMark::Check,
            style: MarkStyle::default(),
            source: None,
        });
        assert_eq!(draft.validate(&page()), Err(DraftError::Empty));
    }

    #[test]
    fn style_repairs_bring_every_measure_back_into_range() {
        let style = MarkStyle {
            color: InkColor::Red,
            opacity: 4.0,
            width: -3.0,
            font_size: f32::NAN,
        }
        .sanitised();
        assert_eq!(style.opacity, 1.0);
        assert_eq!(style.width, MARK_WIDTH_RANGE.0);
        assert_eq!(style.font_size, MarkStyle::default().font_size);

        let mut draft = ink(vec![InkPoint::new(1.0, 1.0)]);
        if let AnnotationDraft::Ink(d) = &mut draft {
            d.style.opacity = f32::INFINITY;
        }
        draft.sanitise();
        assert_eq!(draft.style().opacity, 1.0);
    }

    #[test]
    fn text_markup_is_not_freely_movable_and_the_rest_is() {
        assert!(!AnnotationKind::Highlight.is_freely_movable());
        assert!(!AnnotationKind::Other.is_freely_movable());
        for kind in [
            AnnotationKind::Ink,
            AnnotationKind::FreeText,
            AnnotationKind::Note,
            AnnotationKind::Stamp,
        ] {
            assert!(kind.is_freely_movable(), "{kind:?}");
            assert!(!kind.subtype().is_empty());
        }
    }

    #[test]
    fn commands_name_what_they_do_and_what_they_touch() {
        let create = AnnotationCommand::Create(ink(vec![InkPoint::new(1.0, 1.0)]));
        assert_eq!(create.label(), "Add Ink");
        assert!(create.id().is_none());
        assert!(create.draft().is_some());

        let id = super::super::id::IdGenerator::new(0).next_id();
        let delete = AnnotationCommand::Delete { id: id.clone() };
        assert_eq!(delete.label(), "Erase");
        assert_eq!(delete.id(), Some(&id));
        assert!(delete.draft().is_none());

        let replace = AnnotationCommand::Replace {
            id,
            replacement: ink(vec![InkPoint::new(1.0, 1.0)]),
        };
        assert_eq!(replace.label(), "Edit Ink");
    }

    #[test]
    fn a_picture_of_a_signature_is_never_called_a_signature() {
        let mark = StampMark::Image {
            pixel_width: 1,
            pixel_height: 1,
            rgba: vec![0; 4],
        };
        assert!(!mark.label().to_lowercase().contains("signature"));
    }
}
