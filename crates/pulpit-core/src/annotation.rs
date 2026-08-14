//! Presenter annotations: temporary ink, highlighting, text and erasing.
//!
//! Everything here is in the same normalised top-left page coordinates as
//! [`crate::notes::Region`], so an annotation drawn on the presenter screen
//! lands on exactly the same part of the page on the audience screen, through
//! whatever letterboxing or `/FitR` crop each window happens to be applying.
//! Storing pixels instead would tie a mark to the panel it was drawn in, and
//! the two panels are never the same size.
//!
//! Annotations are *temporary*. They are not part of the document, they are
//! never written back to it, and they do not survive navigation: see
//! [`Annotations::RETAIN_ACROSS_NAVIGATION`].

use serde::{Deserialize, Serialize};

/// What the presenter's pointer is currently doing to the slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationTool {
    /// A dot that follows the pointer: "this line here".
    Pointer,
    /// A lit circle with the rest of the page dimmed around it.
    ///
    /// Not a palette button of its own: it is the other thing the pointer
    /// control can be, chosen in that control's options. Pointing at a line
    /// and lighting up the paragraph around it are the same gesture with the
    /// dimming turned up, so they share one slot rather than competing for
    /// two.
    Spotlight,
    /// Freehand strokes that stay until the slide changes.
    Ink,
    /// A broad translucent stroke that leaves slide content readable.
    Highlighter,
    /// Removes a stroke touched by the eraser gesture.
    Eraser,
    /// Places a text label, then sends keyboard input to it until committed.
    Text,
}

impl AnnotationTool {
    /// The tools the palette offers a control for, in the order it draws
    /// them. [`AnnotationTool::Spotlight`] is absent because it is armed from
    /// the pointer control's options rather than from a button of its own.
    pub const ALL: [AnnotationTool; 5] = [
        AnnotationTool::Pointer,
        AnnotationTool::Ink,
        AnnotationTool::Highlighter,
        AnnotationTool::Eraser,
        AnnotationTool::Text,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AnnotationTool::Pointer => "Pointer",
            AnnotationTool::Spotlight => "Spotlight",
            AnnotationTool::Ink => "Ink",
            AnnotationTool::Highlighter => "Highlighter",
            AnnotationTool::Eraser => "Eraser",
            AnnotationTool::Text => "Text",
        }
    }
}

/// How a freehand stroke is composited over the page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StrokeKind {
    #[default]
    Ink,
    Highlight,
}

impl StrokeKind {
    pub fn opacity(self) -> f32 {
        match self {
            StrokeKind::Ink => 1.0,
            StrokeKind::Highlight => 0.38,
        }
    }
}

/// The ink colours on offer.
///
/// Six named ones and a mixed one, in that order of prominence. The colours are
/// chosen to stay legible over both a white slide and a dark one, which an
/// arbitrary colour is not — so they are what the palette shows, and mixing
/// is a deliberate step past them rather than the first thing on offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InkColor {
    #[default]
    Black,
    /// Legible on a white slide, which most slides are.
    Red,
    Yellow,
    Cyan,
    Green,
    White,
    /// A colour the presenter mixed, in sRGB bytes.
    ///
    /// The named colours above are the ones that survive being projected onto somebody
    /// else's slide, and they stay the palette. This is the escape hatch for
    /// the deck that is entirely one of them already — a red diagram wants
    /// anything but red ink — and for matching a house colour.
    Custom {
        red: u8,
        green: u8,
        blue: u8,
    },
}

impl InkColor {
    pub const ALL: [InkColor; 6] = [
        InkColor::Black,
        InkColor::Red,
        InkColor::Yellow,
        InkColor::Cyan,
        InkColor::Green,
        InkColor::White,
    ];

    /// Straight sRGB components, so no UI toolkit type appears in the model.
    pub fn rgb(self) -> (f32, f32, f32) {
        match self {
            InkColor::Black => (0.0, 0.0, 0.0),
            InkColor::Red => (0.90, 0.16, 0.22),
            InkColor::Yellow => (0.98, 0.80, 0.16),
            InkColor::Cyan => (0.16, 0.78, 0.94),
            InkColor::Green => (0.22, 0.78, 0.36),
            InkColor::White => (1.0, 1.0, 1.0),
            InkColor::Custom { red, green, blue } => (
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
            ),
        }
    }

    /// A mixed colour from sRGB components in `0.0..=1.0`.
    ///
    /// Stored as bytes rather than floats so two colours that look the same
    /// *are* the same: a palette that compares equal is what lets the swatch
    /// show which one is armed.
    pub fn from_rgb(red: f32, green: f32, blue: f32) -> Self {
        let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        InkColor::Custom {
            red: byte(red),
            green: byte(green),
            blue: byte(blue),
        }
    }

    /// Is this one the presenter mixed rather than one of the five?
    pub fn is_custom(self) -> bool {
        matches!(self, InkColor::Custom { .. })
    }

    pub fn label(self) -> &'static str {
        match self {
            InkColor::Black => "Black",
            InkColor::Red => "Red",
            InkColor::Yellow => "Yellow",
            InkColor::Cyan => "Cyan",
            InkColor::Green => "Green",
            InkColor::White => "White",
            InkColor::Custom { .. } => "Custom",
        }
    }
}

/// How big the marks are, as fractions of the page's width.
///
/// Fractions rather than pixels for the same reason the coordinates are: the
/// presenter panel and the projector are different sizes, and a five-pixel
/// stroke would be a scratch on one and a smear on the other.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnotationStyle {
    /// Radius of the pointer dot.
    pub pointer_radius: f32,
    /// The colour of the pointer dot. A choice rather than the interface's
    /// accent: the accent is picked to sit against the application's own
    /// chrome, and the dot has to be found on somebody else's slide.
    pub pointer_color: InkColor,
    /// Radius of the lit circle.
    pub spotlight_radius: f32,
    /// Width of an ink stroke.
    pub ink_width: f32,
    /// How dark the page goes outside the spotlight, `0.0`–`1.0`.
    pub dim: f32,
}

impl Default for AnnotationStyle {
    fn default() -> Self {
        Self {
            pointer_radius: 0.012,
            pointer_color: InkColor::Red,
            spotlight_radius: 0.08,
            ink_width: 0.004,
            dim: 0.6,
        }
    }
}

/// The widest and narrowest each measure may be. Outside these a pointer is
/// invisible or a spotlight is the whole page, and neither is an annotation.
pub const POINTER_RADIUS_RANGE: (f32, f32) = (0.003, 0.06);
/// A *radius*, as a fraction of the page width. A quarter of the width is
/// already a lit circle half the page across — wider than that is not a
/// spotlight, it is the lights back on, which the tool being armed at all is
/// the way to say.
pub const SPOTLIGHT_RADIUS_RANGE: (f32, f32) = (0.02, 0.25);
pub const INK_WIDTH_RANGE: (f32, f32) = (0.001, 0.03);
pub const HIGHLIGHT_WIDTH_RANGE: (f32, f32) = (0.006, 0.08);
pub const ERASER_RADIUS_RANGE: (f32, f32) = (0.004, 0.10);

impl AnnotationStyle {
    /// Bring every measure back inside its range, replacing anything that is
    /// not a number with the default.
    pub fn sanitise(&mut self) {
        let default = AnnotationStyle::default();
        let bound = |value: f32, fallback: f32, range: (f32, f32)| {
            if value.is_finite() {
                value.clamp(range.0, range.1)
            } else {
                fallback
            }
        };
        self.pointer_radius = bound(
            self.pointer_radius,
            default.pointer_radius,
            POINTER_RADIUS_RANGE,
        );
        self.spotlight_radius = bound(
            self.spotlight_radius,
            default.spotlight_radius,
            SPOTLIGHT_RADIUS_RANGE,
        );
        self.ink_width = bound(self.ink_width, default.ink_width, INK_WIDTH_RANGE);
        self.dim = if self.dim.is_finite() {
            self.dim.clamp(0.0, 0.95)
        } else {
            default.dim
        };
    }
}

/// One freehand mark, in normalised page coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InkStroke {
    pub points: Vec<(f32, f32)>,
    /// Stroke width as a fraction of the page width.
    pub width: f32,
    pub color: InkColor,
    #[serde(default)]
    pub kind: StrokeKind,
}

/// A temporary text label anchored in normalised page coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMark {
    /// Stable within this process, so an asynchronous renderer can reject a
    /// result for a label that has since been edited or erased.
    #[serde(default)]
    pub id: u64,
    pub position: (f32, f32),
    pub text: String,
    /// Cap height as a fraction of the page width.
    pub size: f32,
    pub color: InkColor,
}

impl InkStroke {
    /// A stroke of one point is a dot, which is a mark a presenter means to
    /// make; a stroke of none is the residue of a press that went nowhere.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// Every annotation on the current slide, plus what the pointer is armed to
/// do and who can see the result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotations {
    pub strokes: Vec<InkStroke>,
    #[serde(default)]
    pub texts: Vec<TextMark>,
    /// Where the pointer dot is, when the pointer tool is armed and the
    /// pointer is over the page.
    pub pointer: Option<(f32, f32)>,
    /// Where the spotlight is centred.
    pub spotlight: Option<(f32, f32)>,
    /// The armed tool, or `None` when the pointer belongs to links and media
    /// overlays as it normally does.
    pub tool: Option<AnnotationTool>,
    /// Whether the audience screen shows these too. On by default — a mark is
    /// drawn to be seen. Off means the presenter is marking up their own copy.
    pub audience_visible: bool,
    /// Whether a stroke is open, i.e. the button is down and points are
    /// still arriving. Not serialised state anybody depends on, but it is
    /// what makes a stray move event after a release harmless.
    #[serde(skip)]
    drawing: bool,
    /// Whether the open gesture is an eraser sweep, so the strokes one sweep
    /// took are taken back together rather than one press-worth at a time.
    #[serde(skip)]
    erasing: bool,
    /// Index of the label currently receiving keyboard input.
    #[serde(skip)]
    typing: Option<usize>,
    #[serde(skip)]
    next_text_id: u64,
    /// What has been done to the marks, oldest first, and what has been taken
    /// back off the end of it. Neither is serialised: an edit history is
    /// about a session at a lectern, not about a document.
    #[serde(skip)]
    history: Vec<Edit>,
    #[serde(skip)]
    future: Vec<Edit>,
    /// Bumped by every visible change, so a consumer holding a rendered copy
    /// can tell "changed" from "same" without a deep comparison.
    #[serde(skip)]
    revision: Revision,
}

impl Default for Annotations {
    /// Everything empty and unarmed, and the marks shown to the audience:
    /// hiding them is the deliberate choice, not showing them.
    fn default() -> Self {
        Self {
            strokes: Vec::new(),
            texts: Vec::new(),
            pointer: None,
            spotlight: None,
            tool: None,
            audience_visible: true,
            drawing: false,
            erasing: false,
            typing: None,
            next_text_id: 1,
            history: Vec::new(),
            future: Vec::new(),
            revision: Revision::default(),
        }
    }
}

/// One reversible change to the marks on a slide.
///
/// Recorded as *what happened* rather than as a copy of the whole slide:
/// snapshots of a thousand-point stroke, a hundred edits deep, is a
/// presentation's worth of memory for a feature used twice a talk.
#[derive(Debug, Clone, PartialEq)]
enum Edit {
    /// A stroke was drawn, and sits at the end of the list.
    Added(InkStroke),
    /// Marks erased in one sweep, with their original positions.
    Removed(RemovedMarks),
    AddedText(TextMark),
}

#[derive(Debug, Clone, Default, PartialEq)]
struct RemovedMarks {
    strokes: Vec<(usize, InkStroke)>,
    texts: Vec<(usize, TextMark)>,
}

impl RemovedMarks {
    fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.texts.is_empty()
    }
}

/// A monotonic change counter that deliberately compares equal to any other,
/// so deriving `PartialEq` on the model keeps meaning "same marks", not
/// "same edit history".
#[derive(Debug, Clone, Copy, Default)]
pub struct Revision(u64);

impl PartialEq for Revision {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Annotations {
    /// Ink is a gesture about *this* slide, so it goes when the slide does.
    ///
    /// Keeping marks across navigation was considered and rejected: strokes
    /// are positioned in page coordinates with no idea what is underneath
    /// them, so a mark that survives a page turn lands on unrelated content
    /// and — worse, when the audience can see it — does so on the projector.
    /// The constant exists so the decision is a value the tests can assert
    /// rather than a comment.
    pub const RETAIN_ACROSS_NAVIGATION: bool = false;

    /// How many strokes one slide may hold. Beyond this the oldest is
    /// dropped: the alternative is a pen that silently stops writing in the
    /// middle of a talk, which is worse than losing a mark made minutes ago.
    pub const MAX_STROKES: usize = 64;

    /// How many points one stroke may hold. A pointer device reports far
    /// faster than a hand moves, and an unbounded stroke is an unbounded
    /// path to tessellate every frame.
    pub const MAX_POINTS_PER_STROKE: usize = 1024;

    /// Text is live UI state, so bound it before it reaches shaping/layout.
    pub const MAX_TEXT_BYTES: usize = 4096;

    /// How far the pointer must travel before a point is worth keeping, as a
    /// fraction of the page. Below this the points describe the tremor in a
    /// hand rather than the shape of the mark.
    pub const MIN_POINT_DISTANCE: f32 = 0.002;

    /// How many edits can be taken back. Deep enough to cover a slide's worth
    /// of drawing, bounded because a rehearsal is unbounded.
    pub const MAX_HISTORY: usize = 128;

    /// The current change count. Two reads returning the same number mean
    /// nothing visible changed between them.
    pub fn revision(&self) -> u64 {
        self.revision.0
    }

    fn bump(&mut self) {
        self.revision.0 = self.revision.0.wrapping_add(1);
    }

    /// Record an edit. Doing something new is what makes redo meaningless,
    /// so the taken-back edits go here rather than in every caller.
    fn record(&mut self, edit: Edit) {
        self.future.clear();
        if self.history.len() >= Self::MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(edit);
    }

    /// Forget the edit history, marks and all.
    ///
    /// Called wherever the strokes are replaced wholesale — a page turn, a
    /// clear, the stroke cap dropping the oldest mark — because an edit
    /// naming a position in a list that no longer exists is not something to
    /// offer a presenter a button for.
    fn forget_history(&mut self) {
        self.history.clear();
        self.future.clear();
    }

    /// Is there an edit to take back?
    pub fn can_undo(&self) -> bool {
        !self.history.is_empty() || self.drawing || self.typing.is_some()
    }

    /// Is there an edit to put back?
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// Is this a point on the page at all?
    ///
    /// Non-finite coordinates come from degenerate panel geometry, and
    /// out-of-range ones from the letterbox bars — neither is somewhere the
    /// author drew anything, so neither is somewhere a mark can go.
    pub fn is_on_page(point: (f32, f32)) -> bool {
        point.0.is_finite()
            && point.1.is_finite()
            && (0.0..=1.0).contains(&point.0)
            && (0.0..=1.0).contains(&point.1)
    }

    /// Start a stroke at this point. Refuses points that are not on the page.
    pub fn begin_stroke(&mut self, point: (f32, f32), width: f32, color: InkColor) -> bool {
        self.begin_mark(point, width, color, StrokeKind::Ink)
    }

    /// Start an ink or highlighter stroke.
    pub fn begin_mark(
        &mut self,
        point: (f32, f32),
        width: f32,
        color: InkColor,
        kind: StrokeKind,
    ) -> bool {
        if !Self::is_on_page(point) {
            return false;
        }
        let range = match kind {
            StrokeKind::Ink => INK_WIDTH_RANGE,
            StrokeKind::Highlight => HIGHLIGHT_WIDTH_RANGE,
        };
        let fallback = match kind {
            StrokeKind::Ink => AnnotationStyle::default().ink_width,
            StrokeKind::Highlight => 0.025,
        };
        let width = if width.is_finite() {
            width.clamp(range.0, range.1)
        } else {
            fallback
        };
        if self.strokes.len() >= Self::MAX_STROKES {
            // The oldest mark is gone for good, and every recorded edit is
            // written in positions that just moved, so the history goes with
            // it rather than becoming an undo that restores the wrong stroke.
            self.strokes.remove(0);
            self.forget_history();
        }
        self.strokes.push(InkStroke {
            points: vec![point],
            width,
            color,
            kind,
        });
        self.drawing = true;
        self.bump();
        true
    }

    /// Begin an eraser gesture and erase at its first point.
    pub fn begin_erase(&mut self, point: (f32, f32), radius: f32) -> bool {
        if !Self::is_on_page(point) {
            return false;
        }
        self.drawing = true;
        self.erasing = true;
        self.erase_at(point, radius)
    }

    /// Continue an eraser gesture while the pointer button is held.
    pub fn extend_erase(&mut self, point: (f32, f32), radius: f32) -> bool {
        if !self.drawing || !Self::is_on_page(point) {
            return false;
        }
        self.erase_at(point, radius)
    }

    /// Remove ink or text touched by a circular eraser.
    ///
    /// Stroke erasing is deliberate here: it keeps undo meaningful and
    /// avoids leaving tiny disconnected fragments that are hard to clear in
    /// front of an audience.
    fn erase_at(&mut self, point: (f32, f32), radius: f32) -> bool {
        let radius = if radius.is_finite() {
            radius.clamp(ERASER_RADIUS_RANGE.0, ERASER_RADIUS_RANGE.1)
        } else {
            0.02
        };
        let mut removed = RemovedMarks::default();
        let mut index = 0;
        self.strokes.retain(|stroke| {
            let hit_radius = radius + stroke.width / 2.0;
            let hit_radius_squared = hit_radius * hit_radius;
            // One pass: every interior point is an endpoint of some segment,
            // and the point-in-circle test is the degenerate case of the
            // segment test, so scanning the points *and* the segments tested
            // everything twice. Only a single-point stroke has no segment.
            let hit = match stroke.points.as_slice() {
                [only] => distance_squared(*only, point) <= hit_radius_squared,
                points => points.windows(2).any(|segment| {
                    distance_to_segment_squared(point, segment[0], segment[1]) <= hit_radius_squared
                }),
            };
            let position = index;
            index += 1;
            if hit {
                removed.strokes.push((position, stroke.clone()));
            }
            !hit
        });
        let mut text_index = 0;
        self.texts.retain(|mark| {
            let hit = text_mark_hit(mark, point, radius);
            let position = text_index;
            text_index += 1;
            if hit {
                removed.texts.push((position, mark.clone()));
            }
            !hit
        });
        if removed.is_empty() {
            return false;
        }
        // One sweep of the eraser is one thing the presenter did, so undo
        // gives back everything that sweep took rather than making them press
        // it once per stroke they crossed.
        match self.history.last_mut() {
            Some(Edit::Removed(sweep)) if self.erasing => {
                // The positions just recorded are positions in the shortened
                // list; the sweep's are positions in the list as it was when
                // the sweep began. Shifting the new ones past every stroke
                // the sweep already took is what puts both in the same
                // coordinates, so undo can insert them all in one pass.
                for (position, stroke) in removed.strokes {
                    let mut position = position;
                    for (taken, _) in &sweep.strokes {
                        if *taken <= position {
                            position += 1;
                        }
                    }
                    sweep.strokes.push((position, stroke));
                    sweep.strokes.sort_by_key(|(position, _)| *position);
                }
                for (position, mark) in removed.texts {
                    let mut position = position;
                    for (taken, _) in &sweep.texts {
                        if *taken <= position {
                            position += 1;
                        }
                    }
                    sweep.texts.push((position, mark));
                    sweep.texts.sort_by_key(|(position, _)| *position);
                }
                self.future.clear();
            }
            _ => self.record(Edit::Removed(removed)),
        }
        self.bump();
        true
    }

    /// Continue the open stroke, if the point says something new.
    ///
    /// Returns whether the point was kept, which is what a caller needs to
    /// know to decide whether anything has to be redrawn.
    pub fn extend_stroke(&mut self, point: (f32, f32)) -> bool {
        if !self.drawing || !Self::is_on_page(point) {
            return false;
        }
        let Some(stroke) = self.strokes.last_mut() else {
            return false;
        };
        if stroke.points.len() >= Self::MAX_POINTS_PER_STROKE {
            return false;
        }
        if let Some(last) = stroke.points.last() {
            let (dx, dy) = (point.0 - last.0, point.1 - last.1);
            if dx.hypot(dy) < Self::MIN_POINT_DISTANCE {
                return false;
            }
        }
        stroke.points.push(point);
        self.bump();
        true
    }

    /// Finish the open stroke, discarding one that carries no points.
    ///
    /// This is also where a drawn stroke enters the edit history: it is
    /// recorded finished, so undo gives back the whole mark rather than the
    /// first millimetre of it.
    pub fn end_stroke(&mut self) {
        let drawing = self.drawing;
        let erasing = self.erasing;
        self.drawing = false;
        self.erasing = false;
        if self.strokes.last().is_some_and(InkStroke::is_empty) {
            self.strokes.pop();
            self.bump();
            return;
        }
        if drawing && !erasing {
            if let Some(stroke) = self.strokes.last().cloned() {
                self.record(Edit::Added(stroke));
            }
        }
    }

    /// Start a label at `point`. An unfinished empty label is harmless and is
    /// discarded when typing ends.
    pub fn begin_text(&mut self, point: (f32, f32), size: f32, color: InkColor) -> bool {
        if !Self::is_on_page(point) {
            return false;
        }
        self.finish_text();
        let id = self
            .texts
            .iter()
            .map(|mark| mark.id)
            .max()
            .unwrap_or(0)
            .wrapping_add(1)
            .max(self.next_text_id)
            .max(1);
        self.next_text_id = self.next_text_id.wrapping_add(1).max(1);
        self.texts.push(TextMark {
            id,
            position: point,
            text: String::new(),
            size: if size.is_finite() {
                size.clamp(0.008, 0.12)
            } else {
                0.025
            },
            color,
        });
        self.typing = Some(self.texts.len() - 1);
        self.bump();
        true
    }

    /// Append composed keyboard text to the active label.
    pub fn type_text(&mut self, value: &str) -> bool {
        let Some(mark) = self.typing.and_then(|index| self.texts.get_mut(index)) else {
            return false;
        };
        let remaining = Self::MAX_TEXT_BYTES.saturating_sub(mark.text.len());
        if remaining == 0 {
            return false;
        }
        let end = value
            .char_indices()
            .map(|(index, ch)| index + ch.len_utf8())
            .take_while(|end| *end <= remaining)
            .last()
            .unwrap_or(0);
        if end == 0 {
            return false;
        }
        mark.text.push_str(&value[..end]);
        self.bump();
        true
    }

    pub fn backspace_text(&mut self) -> bool {
        let Some(mark) = self.typing.and_then(|index| self.texts.get_mut(index)) else {
            return false;
        };
        if mark.text.pop().is_some() {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Commit the active label as one undoable edit.
    pub fn finish_text(&mut self) -> bool {
        let Some(index) = self.typing.take() else {
            return false;
        };
        if self
            .texts
            .get(index)
            .is_some_and(|mark| mark.text.is_empty())
        {
            self.texts.remove(index);
        } else if let Some(mark) = self.texts.get(index).cloned() {
            self.record(Edit::AddedText(mark));
        }
        self.bump();
        true
    }

    pub fn is_typing(&self) -> bool {
        self.typing.is_some()
    }

    /// The mark receiving input, for presenter-only editing affordances.
    pub fn typing_index(&self) -> Option<usize> {
        self.typing
    }

    /// Is a stroke currently being drawn?
    pub fn is_drawing(&self) -> bool {
        self.drawing
    }

    /// Take back the most recent edit — a stroke drawn, or a sweep of the
    /// eraser. Returns whether there was one.
    ///
    /// Erasing is undoable for the same reason drawing is: the eraser takes a
    /// whole stroke at a time, so a slip of the hand in front of a room costs
    /// the diagram the presenter just drew, and nothing else can give it
    /// back.
    pub fn undo_stroke(&mut self) -> bool {
        // A stroke still under the pen is finished first, so undo means the
        // same thing whether or not the button happens to be down.
        if self.drawing {
            self.end_stroke();
        }
        self.finish_text();
        let Some(edit) = self.history.pop() else {
            return false;
        };
        match &edit {
            Edit::Added(_) => {
                self.strokes.pop();
            }
            Edit::Removed(removed) => {
                for (position, stroke) in &removed.strokes {
                    let position = (*position).min(self.strokes.len());
                    self.strokes.insert(position, stroke.clone());
                }
                for (position, mark) in &removed.texts {
                    let position = (*position).min(self.texts.len());
                    self.texts.insert(position, mark.clone());
                }
            }
            Edit::AddedText(_) => {
                self.texts.pop();
            }
        }
        self.future.push(edit);
        self.bump();
        true
    }

    /// Put back the most recently undone edit. Returns whether there was one.
    pub fn redo_stroke(&mut self) -> bool {
        if self.drawing {
            self.end_stroke();
        }
        self.finish_text();
        let Some(edit) = self.future.pop() else {
            return false;
        };
        match &edit {
            Edit::Added(stroke) => self.strokes.push(stroke.clone()),
            Edit::Removed(removed) => {
                // Descending, so each removal cannot move the position of the
                // one after it.
                for (position, _) in removed.strokes.iter().rev() {
                    if *position < self.strokes.len() {
                        self.strokes.remove(*position);
                    }
                }
                for (position, _) in removed.texts.iter().rev() {
                    if *position < self.texts.len() {
                        self.texts.remove(*position);
                    }
                }
            }
            Edit::AddedText(mark) => self.texts.push(mark.clone()),
        }
        self.history.push(edit);
        self.bump();
        true
    }

    /// Remove every mark, leaving the armed tool and the audience choice
    /// alone: clearing the page is not putting the pen down.
    pub fn clear(&mut self) {
        self.strokes.clear();
        self.texts.clear();
        self.pointer = None;
        self.spotlight = None;
        self.drawing = false;
        self.erasing = false;
        self.typing = None;
        self.forget_history();
        self.bump();
    }

    /// What navigation does to the annotations on the slide being left.
    ///
    /// Only the marks go. The armed tool and whether the audience can see
    /// annotations are settings the presenter chose, not properties of the
    /// slide, so they survive navigation.
    pub fn clear_on_slide_change(&mut self) {
        if !Self::RETAIN_ACROSS_NAVIGATION {
            self.clear();
        }
    }

    /// Take the marks off this slide, leaving the presenter's settings alone.
    fn take_marks(&mut self) -> (Vec<InkStroke>, Vec<TextMark>) {
        self.pointer = None;
        self.spotlight = None;
        self.drawing = false;
        self.erasing = false;
        self.finish_text();
        self.forget_history();
        self.bump();
        (
            std::mem::take(&mut self.strokes),
            std::mem::take(&mut self.texts),
        )
    }

    /// Move the pointer dot, or take it away. A point off the page takes it
    /// away rather than pinning it to the nearest edge, which would leave a
    /// dot sitting in the letterbox.
    pub fn set_pointer(&mut self, point: Option<(f32, f32)>) {
        let point = point.filter(|point| Self::is_on_page(*point));
        if self.pointer != point {
            self.pointer = point;
            self.bump();
        }
    }

    pub fn set_spotlight(&mut self, point: Option<(f32, f32)>) {
        let point = point.filter(|point| Self::is_on_page(*point));
        if self.spotlight != point {
            self.spotlight = point;
            self.bump();
        }
    }

    /// Choose whether the audience window shows these annotations.
    pub fn set_audience_visible(&mut self, visible: bool) {
        if self.audience_visible != visible {
            self.audience_visible = visible;
            self.bump();
        }
    }

    /// Is there nothing at all to draw?
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
            && self.texts.is_empty()
            && self.pointer.is_none()
            && self.spotlight.is_none()
    }

    /// Arm a tool, or put the pointer back to its ordinary duties. Changing
    /// tool takes away the marks that belonged to the old one, but never the
    /// ink: ink is the only annotation the presenter deliberately committed.
    pub fn arm(&mut self, tool: Option<AnnotationTool>) {
        self.tool = tool;
        self.drawing = false;
        self.erasing = false;
        if tool != Some(AnnotationTool::Text) {
            self.finish_text();
        }
        if tool != Some(AnnotationTool::Pointer) {
            self.pointer = None;
        }
        if tool != Some(AnnotationTool::Spotlight) {
            self.spotlight = None;
        }
        self.bump();
    }

    /// Does the pointer belong to the annotations rather than to links and
    /// media overlays? The one question the input router has to ask.
    pub fn is_armed(&self) -> bool {
        self.tool.is_some()
    }
}

/// The marks made on each slide, for the life of the process.
///
/// Ink is written in normalised page coordinates, so a mark only means
/// anything on the slide it was made on. Keeping the marks *with* their slide
/// is what lets the presenter go back to a diagram they drew on and find it
/// as they left it, without a mark ever appearing over unrelated content.
///
/// Deliberately not `Serialize`. The cache is process memory and nothing
/// else: closing pulpit wipes it. A talk's ink is a performance, not a
/// document, and writing it to disk would quietly turn every rehearsal
/// scribble into something the presenter has to remember to clean up — and,
/// since annotations can be shown to the audience, something with a real cost
/// if it came back unbidden at the next talk.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct InkCache {
    by_slide: std::collections::BTreeMap<usize, (Vec<InkStroke>, Vec<TextMark>)>,
    /// The slide the live annotations currently belong to.
    slide: Option<usize>,
}

impl InkCache {
    /// How many annotated slides are remembered. A deck longer than this can
    /// still be annotated everywhere; the least recently *left* slide loses
    /// its marks first, which beats an unbounded cache in a long rehearsal.
    pub const MAX_SLIDES: usize = 512;

    /// Follow the presenter to `slide`, stashing the marks on the slide being
    /// left and restoring any this slide already had.
    ///
    /// Idempotent: arriving where we already are changes nothing, so a
    /// navigation that commits the same slide twice cannot lose a stroke.
    pub fn follow(&mut self, slide: usize, live: &mut Annotations) {
        if self.slide == Some(slide) {
            return;
        }
        if let Some(previous) = self.slide {
            let marks = live.take_marks();
            if marks.0.is_empty() && marks.1.is_empty() {
                self.by_slide.remove(&previous);
            } else {
                self.by_slide.insert(previous, marks);
                while self.by_slide.len() > Self::MAX_SLIDES {
                    let Some(oldest) = self.by_slide.keys().next().copied() else {
                        break;
                    };
                    self.by_slide.remove(&oldest);
                }
            }
        } else {
            live.take_marks();
        }
        let (strokes, texts) = self.by_slide.get(&slide).cloned().unwrap_or_default();
        live.strokes = strokes;
        live.texts = texts;
        live.bump();
        self.slide = Some(slide);
    }

    /// Forget every slide's marks, including the live ones.
    pub fn clear_all(&mut self, live: &mut Annotations) {
        self.by_slide.clear();
        live.clear();
    }

    /// Forget the marks on the slide the presenter is looking at.
    pub fn clear_current(&mut self, live: &mut Annotations) {
        if let Some(slide) = self.slide {
            self.by_slide.remove(&slide);
        }
        live.clear();
    }

    /// How many slides carry marks, counting the live one.
    pub fn annotated_slides(&self) -> usize {
        self.by_slide.len()
    }

    /// Every annotated slide's marks, in slide order.
    ///
    /// The live annotations stand in for the slide the presenter is on: those
    /// marks have not been stashed yet, and the stroke drawn a second ago is
    /// exactly the one an export is being asked for.
    pub fn marks(&self, live: &Annotations) -> Vec<(usize, Vec<InkStroke>, Vec<TextMark>)> {
        let mut marks: Vec<_> = self
            .by_slide
            .iter()
            .filter(|(slide, _)| Some(**slide) != self.slide)
            .map(|(slide, (strokes, texts))| (*slide, strokes.clone(), texts.clone()))
            .collect();
        if let Some(slide) = self.slide {
            // The pointer and the spotlight are where the hand is, not marks
            // on the page, so a slide carrying only those exports as blank.
            if !live.strokes.is_empty() || !live.texts.is_empty() {
                marks.push((slide, live.strokes.clone(), live.texts.clone()));
            }
        }
        marks.sort_by_key(|(slide, ..)| *slide);
        marks
    }
}

fn distance_squared(left: (f32, f32), right: (f32, f32)) -> f32 {
    let x = left.0 - right.0;
    let y = left.1 - right.1;
    x * x + y * y
}

/// Hit-test the same approximate text block the canvas lays out. Exact glyph
/// outlines would couple the pure model to a renderer; a rectangular label
/// target is also much easier to erase deliberately at presentation speed.
fn text_mark_hit(mark: &TextMark, point: (f32, f32), radius: f32) -> bool {
    let mut lines = 0_usize;
    let mut longest = 0_usize;
    for line in mark.text.split('\n') {
        lines += 1;
        longest = longest.max(line.chars().count());
    }
    let width = longest as f32 * mark.size * 0.6;
    let height = lines.max(1) as f32 * mark.size * 1.2;
    let nearest = (
        point.0.clamp(mark.position.0, mark.position.0 + width),
        point.1.clamp(mark.position.1, mark.position.1 + height),
    );
    distance_squared(point, nearest) <= radius * radius
}

fn distance_to_segment_squared(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let segment = (end.0 - start.0, end.1 - start.1);
    let length_squared = segment.0 * segment.0 + segment.1 * segment.1;
    if length_squared <= f32::EPSILON {
        return distance_squared(point, start);
    }
    let offset = (point.0 - start.0, point.1 - start.1);
    let projection =
        ((offset.0 * segment.0 + offset.1 * segment.1) / length_squared).clamp(0.0, 1.0);
    let nearest = (
        start.0 + segment.0 * projection,
        start.1 + segment.1 * projection,
    );
    distance_squared(point, nearest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: InkColor = InkColor::Red;
    const WIDTH: f32 = 0.004;

    fn drawn(points: &[(f32, f32)]) -> Annotations {
        let mut annotations = Annotations::default();
        let mut points = points.iter();
        if let Some(first) = points.next() {
            assert!(annotations.begin_stroke(*first, WIDTH, RED));
        }
        for point in points {
            annotations.extend_stroke(*point);
        }
        annotations.end_stroke();
        annotations
    }

    #[test]
    fn a_stroke_records_the_points_the_hand_actually_moved_through() {
        let annotations = drawn(&[(0.1, 0.1), (0.3, 0.1), (0.5, 0.1)]);
        assert_eq!(annotations.strokes.len(), 1);
        assert_eq!(annotations.strokes[0].points.len(), 3);
        assert!(!annotations.is_empty());
    }

    #[test]
    fn points_closer_than_the_minimum_distance_are_dropped() {
        // A hand resting on a trackpad reports constantly; none of it is a
        // mark, and all of it would be geometry to tessellate every frame.
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.5, 0.5), WIDTH, RED);
        for step in 0..100 {
            let jitter = step as f32 * 1e-5;
            assert!(
                !annotations.extend_stroke((0.5 + jitter, 0.5)),
                "a tremor is not a stroke"
            );
        }
        assert_eq!(annotations.strokes[0].points.len(), 1);

        assert!(
            annotations.extend_stroke((0.6, 0.5)),
            "real movement is kept"
        );
        assert_eq!(annotations.strokes[0].points.len(), 2);
    }

    #[test]
    fn a_stroke_stops_growing_at_its_point_limit() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.0, 0.0), WIDTH, RED);
        // Every point is far enough apart to be kept, so only the cap stops
        // this.
        for step in 1..(Annotations::MAX_POINTS_PER_STROKE * 2) {
            let x = (step as f32 * Annotations::MIN_POINT_DISTANCE * 2.0) % 1.0;
            annotations.extend_stroke((x, 0.5));
        }
        assert_eq!(
            annotations.strokes[0].points.len(),
            Annotations::MAX_POINTS_PER_STROKE
        );
    }

    #[test]
    fn the_oldest_stroke_is_dropped_rather_than_the_pen_going_dry() {
        let mut annotations = Annotations::default();
        for step in 0..(Annotations::MAX_STROKES + 5) {
            let y = (step as f32) / 1000.0;
            assert!(
                annotations.begin_stroke((0.5, y), WIDTH, RED),
                "the pen always writes"
            );
            annotations.end_stroke();
        }
        assert_eq!(annotations.strokes.len(), Annotations::MAX_STROKES);
        assert_eq!(
            annotations.strokes[0].points[0].1,
            5.0 / 1000.0,
            "the five oldest strokes went, not the five newest"
        );
    }

    #[test]
    fn coordinates_off_the_page_are_refused_rather_than_clamped() {
        let mut annotations = Annotations::default();
        for point in [
            (-0.01, 0.5),
            (1.5, 0.5),
            (0.5, -2.0),
            (f32::NAN, 0.5),
            (0.5, f32::INFINITY),
        ] {
            assert!(
                !annotations.begin_stroke(point, WIDTH, RED),
                "{point:?} is not on the page"
            );
        }
        assert!(annotations.strokes.is_empty());

        annotations.begin_stroke((0.5, 0.5), WIDTH, RED);
        assert!(!annotations.extend_stroke((1.4, 0.5)));
        assert_eq!(annotations.strokes[0].points.len(), 1);

        annotations.set_pointer(Some((2.0, 0.5)));
        assert_eq!(annotations.pointer, None, "the letterbox holds no pointer");
        annotations.set_spotlight(Some((f32::NAN, 0.1)));
        assert_eq!(annotations.spotlight, None);
    }

    #[test]
    fn a_move_after_the_button_came_up_does_not_extend_anything() {
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.1)]);
        assert!(!annotations.is_drawing());
        assert!(!annotations.extend_stroke((0.9, 0.9)));
        assert_eq!(annotations.strokes[0].points.len(), 2);
    }

    #[test]
    fn undo_removes_one_stroke_and_says_when_there_are_none_left() {
        let mut annotations = Annotations::default();
        for y in [0.1, 0.2] {
            annotations.begin_stroke((0.1, y), WIDTH, RED);
            annotations.end_stroke();
        }
        assert!(annotations.undo_stroke());
        assert_eq!(annotations.strokes.len(), 1);
        assert!(annotations.undo_stroke());
        assert!(!annotations.undo_stroke(), "nothing left to take back");
        assert!(annotations.is_empty());
    }

    #[test]
    fn redo_puts_back_exactly_what_undo_took() {
        let mut annotations = Annotations::default();
        for y in [0.1, 0.2, 0.3] {
            annotations.begin_stroke((0.1, y), WIDTH, RED);
            annotations.extend_stroke((0.5, y));
            annotations.end_stroke();
        }
        assert!(annotations.undo_stroke());
        assert!(annotations.undo_stroke());
        assert_eq!(annotations.strokes.len(), 1);

        assert!(annotations.can_redo());
        assert!(annotations.redo_stroke());
        assert!(annotations.redo_stroke());
        assert_eq!(annotations.strokes.len(), 3);
        assert_eq!(annotations.strokes[2].points[0], (0.1, 0.3), "and in order");
        assert!(!annotations.redo_stroke(), "nothing left to put back");
    }

    #[test]
    fn drawing_again_is_what_makes_redo_meaningless() {
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.1)]);
        annotations.undo_stroke();
        assert!(annotations.can_redo());

        annotations.begin_stroke((0.6, 0.6), WIDTH, RED);
        annotations.end_stroke();
        assert!(
            !annotations.can_redo(),
            "the taken-back stroke belongs to a history that no longer happened"
        );
    }

    #[test]
    fn a_slip_of_the_eraser_can_be_taken_back() {
        let mut annotations = Annotations::default();
        for x in [0.2, 0.5, 0.8] {
            annotations.begin_stroke((x, 0.5), WIDTH, RED);
            annotations.end_stroke();
        }

        // One sweep across all three: one press of undo gives back all three.
        annotations.begin_erase((0.2, 0.5), 0.02);
        annotations.extend_erase((0.5, 0.5), 0.02);
        annotations.extend_erase((0.8, 0.5), 0.02);
        annotations.end_stroke();
        assert!(annotations.strokes.is_empty());

        assert!(annotations.undo_stroke());
        assert_eq!(annotations.strokes.len(), 3);
        let positions: Vec<f32> = annotations
            .strokes
            .iter()
            .map(|stroke| stroke.points[0].0)
            .collect();
        assert_eq!(positions, vec![0.2, 0.5, 0.8], "and where they were");

        assert!(annotations.redo_stroke());
        assert!(annotations.strokes.is_empty(), "the sweep happened again");
    }

    #[test]
    fn an_erased_stroke_comes_back_between_the_ones_that_stayed() {
        let mut annotations = Annotations::default();
        for x in [0.2, 0.5, 0.8] {
            annotations.begin_stroke((x, 0.5), WIDTH, RED);
            annotations.end_stroke();
        }
        annotations.begin_erase((0.5, 0.5), 0.02);
        annotations.end_stroke();
        assert_eq!(annotations.strokes.len(), 2);

        annotations.undo_stroke();
        let positions: Vec<f32> = annotations
            .strokes
            .iter()
            .map(|stroke| stroke.points[0].0)
            .collect();
        assert_eq!(positions, vec![0.2, 0.5, 0.8]);
    }

    #[test]
    fn a_page_turn_leaves_nothing_to_undo() {
        // Undo after navigation would restore a mark onto whatever slide the
        // presenter is now showing, which is the one thing ink must never do.
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.4)]);
        assert!(annotations.can_undo());
        annotations.clear_on_slide_change();
        assert!(!annotations.can_undo());
        assert!(!annotations.can_redo());
        assert!(!annotations.undo_stroke());
    }

    #[test]
    fn clearing_takes_the_marks_but_not_the_pen() {
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.4)]);
        annotations.arm(Some(AnnotationTool::Ink));
        annotations.audience_visible = true;
        annotations.set_pointer(Some((0.2, 0.2)));

        annotations.clear();

        assert!(annotations.is_empty());
        assert_eq!(
            annotations.tool,
            Some(AnnotationTool::Ink),
            "clearing the page does not put the pen down"
        );
        assert!(annotations.audience_visible);
    }

    #[test]
    fn navigation_discards_everything_because_ink_is_temporary() {
        let retained: bool = Annotations::RETAIN_ACROSS_NAVIGATION;
        assert!(!retained, "ink belongs to the slide it was drawn on");
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.4)]);
        annotations.set_spotlight(Some((0.5, 0.5)));
        annotations.arm(Some(AnnotationTool::Spotlight));

        annotations.clear_on_slide_change();

        assert!(annotations.is_empty(), "no mark survives a page turn");
        assert_eq!(annotations.tool, Some(AnnotationTool::Spotlight));
    }

    #[test]
    fn arming_a_tool_takes_away_only_the_other_tools_marks() {
        let mut annotations = drawn(&[(0.1, 0.1), (0.4, 0.4)]);
        annotations.arm(Some(AnnotationTool::Pointer));
        annotations.set_pointer(Some((0.3, 0.3)));

        annotations.arm(Some(AnnotationTool::Spotlight));
        assert_eq!(annotations.pointer, None, "the dot belonged to the pointer");
        assert_eq!(annotations.strokes.len(), 1, "the ink was committed");

        annotations.set_spotlight(Some((0.6, 0.6)));
        annotations.arm(None);
        assert_eq!(annotations.spotlight, None);
        assert!(
            !annotations.is_armed(),
            "the pointer is the document's again"
        );
    }

    #[test]
    fn audience_visibility_is_a_deliberate_choice_that_survives_marks_coming_and_going() {
        let mut annotations = Annotations::default();
        assert!(
            annotations.audience_visible,
            "annotations start shown to the audience"
        );
        annotations.audience_visible = false;
        annotations.begin_stroke((0.2, 0.2), WIDTH, RED);
        annotations.end_stroke();
        annotations.clear_on_slide_change();
        assert!(
            !annotations.audience_visible,
            "turning the page is not a decision about who can see"
        );
    }

    #[test]
    fn a_press_that_went_nowhere_leaves_a_dot_but_never_an_empty_stroke() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.5, 0.5), WIDTH, RED);
        annotations.end_stroke();
        assert_eq!(annotations.strokes.len(), 1, "a dot is a mark");
        assert!(!annotations.strokes[0].is_empty());

        annotations.strokes[0].points.clear();
        annotations.begin_stroke((0.1, 0.1), WIDTH, RED);
        annotations.strokes.last_mut().unwrap().points.clear();
        annotations.end_stroke();
        assert_eq!(annotations.strokes.len(), 1, "the empty one went");
    }

    #[test]
    fn absurd_measures_are_brought_back_into_range() {
        let mut style = AnnotationStyle {
            pointer_radius: 40.0,
            pointer_color: InkColor::Cyan,
            spotlight_radius: -1.0,
            ink_width: f32::NAN,
            dim: 5.0,
        };
        style.sanitise();
        assert_eq!(style.pointer_radius, POINTER_RADIUS_RANGE.1);
        assert_eq!(style.spotlight_radius, SPOTLIGHT_RADIUS_RANGE.0);
        assert_eq!(style.ink_width, AnnotationStyle::default().ink_width);
        assert_eq!(style.dim, 0.95);
    }

    #[test]
    fn a_stroke_width_outside_the_range_is_bounded_when_the_stroke_starts() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.5, 0.5), 9.0, RED);
        assert_eq!(annotations.strokes[0].width, INK_WIDTH_RANGE.1);
        annotations.end_stroke();
        annotations.begin_stroke((0.4, 0.5), f32::NAN, RED);
        assert_eq!(
            annotations.strokes[1].width,
            AnnotationStyle::default().ink_width
        );
    }

    #[test]
    fn a_highlighter_stroke_keeps_its_translucent_kind_and_own_width_range() {
        let mut annotations = Annotations::default();
        assert!(annotations.begin_mark((0.5, 0.5), 9.0, InkColor::Yellow, StrokeKind::Highlight,));
        annotations.end_stroke();
        let stroke = &annotations.strokes[0];
        assert_eq!(stroke.kind, StrokeKind::Highlight);
        assert_eq!(stroke.width, HIGHLIGHT_WIDTH_RANGE.1);
        assert!(stroke.kind.opacity() < 1.0);
    }

    #[test]
    fn the_eraser_removes_only_a_stroke_it_touches() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.2, 0.2), WIDTH, RED);
        annotations.end_stroke();
        annotations.begin_stroke((0.8, 0.8), WIDTH, RED);
        annotations.end_stroke();

        annotations.begin_erase((0.21, 0.2), 0.02);
        annotations.end_stroke();

        assert_eq!(annotations.strokes.len(), 1);
        assert_eq!(annotations.strokes[0].points[0], (0.8, 0.8));
    }

    #[test]
    fn the_eraser_detects_a_crossing_between_sampled_points() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.2, 0.5), WIDTH, RED);
        annotations.extend_stroke((0.8, 0.5));
        annotations.end_stroke();

        annotations.begin_erase((0.5, 0.5), ERASER_RADIUS_RANGE.0);
        annotations.end_stroke();

        assert!(annotations.strokes.is_empty());
    }

    #[test]
    fn one_eraser_sweep_removes_text_and_ink_and_undo_restores_both() {
        let mut annotations = Annotations::default();
        annotations.begin_stroke((0.25, 0.22), WIDTH, RED);
        annotations.end_stroke();
        annotations.begin_text((0.2, 0.2), 0.03, InkColor::Black);
        annotations.type_text("erase me");
        annotations.finish_text();

        annotations.begin_erase((0.25, 0.22), 0.02);
        annotations.end_stroke();
        assert!(annotations.strokes.is_empty());
        assert!(annotations.texts.is_empty());

        assert!(annotations.undo_stroke());
        assert_eq!(annotations.strokes.len(), 1);
        assert_eq!(annotations.texts[0].text, "erase me");
        assert!(annotations.redo_stroke());
        assert!(annotations.strokes.is_empty());
        assert!(annotations.texts.is_empty());
    }

    /// Draw one stroke on the slide the presenter is currently on.
    fn scribble(live: &mut Annotations, y: f32) {
        assert!(live.begin_stroke((0.2, y), WIDTH, RED));
        live.extend_stroke((0.6, y));
        live.end_stroke();
    }

    #[test]
    fn marks_come_back_when_the_presenter_returns_to_the_slide() {
        let mut cache = InkCache::default();
        let mut live = Annotations::default();

        cache.follow(3, &mut live);
        scribble(&mut live, 0.5);
        assert_eq!(live.strokes.len(), 1);

        // Away: the slide's marks go with it rather than onto the next page.
        cache.follow(4, &mut live);
        assert!(
            live.strokes.is_empty(),
            "ink followed the presenter forward"
        );

        // Back: and they are exactly as they were left.
        cache.follow(3, &mut live);
        assert_eq!(live.strokes.len(), 1);
        assert_eq!(live.strokes[0].color, RED);
    }

    #[test]
    fn each_slide_keeps_its_own_marks() {
        let mut cache = InkCache::default();
        let mut live = Annotations::default();
        for slide in 0..3 {
            cache.follow(slide, &mut live);
            for _ in 0..=slide {
                scribble(&mut live, 0.2 + slide as f32 * 0.1);
            }
        }
        cache.follow(99, &mut live);
        for slide in 0..3 {
            cache.follow(slide, &mut live);
            assert_eq!(live.strokes.len(), slide + 1, "slide {slide}");
        }
    }

    #[test]
    fn arriving_where_we_already_are_keeps_the_marks_being_made() {
        // Navigation can commit the same slide twice; that must not be a way
        // to lose a stroke that is already on the page.
        let mut cache = InkCache::default();
        let mut live = Annotations::default();
        cache.follow(2, &mut live);
        scribble(&mut live, 0.4);
        cache.follow(2, &mut live);
        assert_eq!(live.strokes.len(), 1);
    }

    #[test]
    fn a_slide_wiped_clean_does_not_come_back_annotated() {
        let mut cache = InkCache::default();
        let mut live = Annotations::default();
        cache.follow(1, &mut live);
        scribble(&mut live, 0.5);
        cache.clear_current(&mut live);
        cache.follow(2, &mut live);
        cache.follow(1, &mut live);
        assert!(live.strokes.is_empty(), "cleared ink came back");
        assert_eq!(cache.annotated_slides(), 0);
    }

    #[test]
    fn text_is_live_then_committed_as_one_undoable_mark() {
        let mut annotations = Annotations::default();
        assert!(annotations.begin_text((0.2, 0.3), 0.025, InkColor::White));
        assert!(annotations.type_text("Energy = mc²"));
        assert_eq!(annotations.texts[0].text, "Energy = mc²");
        assert!(annotations.is_typing());
        assert!(annotations.finish_text());
        assert!(!annotations.is_typing());
        assert!(annotations.undo_stroke());
        assert!(annotations.texts.is_empty());
        assert!(annotations.redo_stroke());
        assert_eq!(annotations.texts[0].text, "Energy = mc²");
    }

    #[test]
    fn text_follows_the_slide_like_ink() {
        let mut cache = InkCache::default();
        let mut live = Annotations::default();
        cache.follow(1, &mut live);
        live.begin_text((0.1, 0.1), 0.03, InkColor::Cyan);
        live.type_text("Remember this");
        live.finish_text();
        cache.follow(2, &mut live);
        assert!(live.texts.is_empty());
        cache.follow(1, &mut live);
        assert_eq!(live.texts[0].text, "Remember this");
    }

    #[test]
    fn clearing_everything_leaves_no_slide_annotated() {
        let mut cache = InkCache::default();
        let mut live = Annotations::default();
        for slide in 0..4 {
            cache.follow(slide, &mut live);
            scribble(&mut live, 0.5);
        }
        cache.clear_all(&mut live);
        assert!(live.strokes.is_empty());
        for slide in 0..4 {
            cache.follow(slide, &mut live);
            assert!(live.strokes.is_empty(), "slide {slide} kept its marks");
        }
    }

    #[test]
    fn the_presenters_settings_are_not_properties_of_a_slide() {
        // Whether the audience can see the ink, and which tool is in hand,
        // are choices the presenter made. Turning a page must not undo them.
        let mut cache = InkCache::default();
        let mut live = Annotations {
            audience_visible: true,
            ..Annotations::default()
        };
        live.arm(Some(AnnotationTool::Ink));
        cache.follow(1, &mut live);
        scribble(&mut live, 0.5);
        cache.follow(2, &mut live);
        assert!(live.audience_visible, "the audience toggle was reset");
        assert_eq!(live.tool, Some(AnnotationTool::Ink), "the pen was put down");
    }

    #[test]
    fn a_long_rehearsal_cannot_grow_the_cache_without_bound() {
        let mut cache = InkCache::default();
        let mut live = Annotations::default();
        for slide in 0..InkCache::MAX_SLIDES + 20 {
            cache.follow(slide, &mut live);
            scribble(&mut live, 0.5);
        }
        cache.follow(usize::MAX, &mut live);
        assert!(cache.annotated_slides() <= InkCache::MAX_SLIDES);
    }

    #[test]
    fn a_mixed_colour_survives_the_round_trip_and_compares_equal() {
        let mixed = InkColor::from_rgb(0.2, 0.4, 0.6);
        assert!(mixed.is_custom());
        assert_eq!(mixed, InkColor::from_rgb(0.2, 0.4, 0.6), "same colour");
        assert_ne!(mixed, InkColor::from_rgb(0.2, 0.4, 0.61));

        let (red, green, blue) = mixed.rgb();
        for (component, wanted) in [(red, 0.2), (green, 0.4), (blue, 0.6)] {
            assert!((component - wanted).abs() < 1.0 / 255.0, "{component}");
        }

        // Out-of-range components are brought back rather than wrapped: a
        // colour is a colour, not an arithmetic accident.
        assert_eq!(
            InkColor::from_rgb(-1.0, 2.0, f32::NAN),
            InkColor::Custom {
                red: 0,
                green: 255,
                blue: 0
            }
        );
        assert!(!InkColor::Red.is_custom());
        assert_eq!(mixed.label(), "Custom");
    }

    #[test]
    fn every_ink_colour_is_a_real_colour_with_a_name() {
        for color in InkColor::ALL {
            assert!(!color.label().is_empty());
            let (r, g, b) = color.rgb();
            for component in [r, g, b] {
                assert!((0.0..=1.0).contains(&component), "{color:?}");
            }
        }
        for tool in AnnotationTool::ALL {
            assert!(!tool.label().is_empty());
        }
    }
}
