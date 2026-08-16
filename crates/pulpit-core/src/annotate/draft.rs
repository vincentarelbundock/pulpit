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

use crate::annotation::InkColor;
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
    /// `/Highlight`, with `/QuadPoints` over extracted text.
    Highlight,
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
            AnnotationKind::Highlight => "Highlight",
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
            AnnotationKind::Highlight => "Highlight",
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
        !matches!(self, AnnotationKind::Highlight | AnnotationKind::Other)
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
            color: InkColor::Red,
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
        MarkStyle {
            color: InkColor::Yellow,
            opacity: 0.4,
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
    /// One quadrilateral per contiguous run of selected text, in reading
    /// order. Normative geometry, not a hint (§7.2).
    pub quads: Vec<PageQuad>,
    /// The selected text, carried into `/Contents` so the mark is recoverable
    /// by a reader that re-extracts the page.
    pub text: String,
    pub style: MarkStyle,
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
    pub open: bool,
    pub style: MarkStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StampDraft {
    pub page: PageIndex,
    pub rect: PageRect,
    pub mark: StampMark,
    pub style: MarkStyle,
}

/// One annotation, fully described, in canonical page space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationDraft {
    Ink(InkDraft),
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
            AnnotationDraft::Highlight(_) => AnnotationKind::Highlight,
            AnnotationDraft::FreeText(_) => AnnotationKind::FreeText,
            AnnotationDraft::Note(_) => AnnotationKind::Note,
            AnnotationDraft::Stamp(_) => AnnotationKind::Stamp,
        }
    }

    pub fn page(&self) -> PageIndex {
        match self {
            AnnotationDraft::Ink(d) => d.page,
            AnnotationDraft::Highlight(d) => d.page,
            AnnotationDraft::FreeText(d) => d.page,
            AnnotationDraft::Note(d) => d.page,
            AnnotationDraft::Stamp(d) => d.page,
        }
    }

    pub fn style(&self) -> MarkStyle {
        match self {
            AnnotationDraft::Ink(d) => d.style,
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
            open: false,
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
