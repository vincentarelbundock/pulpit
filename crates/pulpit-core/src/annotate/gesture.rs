//! Gesture state: bounded, ephemeral, and never a second copy of a mark.
//!
//! Invariant A2. While the pointer is down the UI draws the unfinished stroke
//! itself, because a round trip to the worker for every sample would put PDF
//! rendering in the input path. On release the gesture produces exactly one
//! command and is dropped. Nothing here is ever written to the session
//! snapshot (§3.2): an interrupted gesture is a gesture that did not happen.

use crate::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect};

pub use crate::annotation::AnnotationTool;
use crate::annotation::MarkupKind;

use super::draft::{
    AnnotationCommand, AnnotationDraft, FreeTextDraft, HighlightDraft, InkDraft, MarkStyle,
    NoteDraft, StampDraft, StampMark, TextSource,
};
use super::id::AnnotationId;
use super::stroke::{accept_sample, simplify, InkPoint, MAX_INK_POINTS, SIMPLIFY_TOLERANCE};

/// What the pointer is currently in the middle of.
///
/// One gesture at a time on purpose: a second pointer starting a second stroke
/// while the first is open is a stray event, not two strokes, and the state
/// machine says so by having nowhere to put it.
#[derive(Debug, Clone, PartialEq)]
pub enum Gesture {
    /// A stroke being drawn. The points are already thinned by the sampling
    /// rule; simplification happens once, on release.
    Ink {
        page: PageIndex,
        points: Vec<InkPoint>,
        style: MarkStyle,
    },
    /// A text selection being dragged. The quads are whatever the engine last
    /// resolved; they are display state and are re-queried as the drag moves.
    Selecting {
        page: PageIndex,
        anchor: PagePoint,
        head: PagePoint,
        quads: Vec<PageQuad>,
        text: String,
        style: MarkStyle,
        /// Which of the three marks the release will leave behind. Fixed when
        /// the sweep begins, so changing the option mid-drag cannot turn the
        /// mark under the hand into a different one.
        kind: MarkupKind,
    },
    /// An eraser sweep. Collects the annotations it has passed over so one
    /// sweep is one undo entry (§8.3).
    Erasing {
        page: PageIndex,
        at: PagePoint,
        touched: Vec<AnnotationId>,
    },
    /// A rubber band being dragged over the page. Selects rather than marks:
    /// the release gathers up what the band encloses and commits nothing, so
    /// this is the one gesture that can end without the document hearing
    /// about it (§8.4).
    Marquee {
        page: PageIndex,
        anchor: PagePoint,
        head: PagePoint,
    },
    /// A selected annotation being dragged or resized. The document is
    /// untouched until release (§8.4).
    Transforming {
        id: AnnotationId,
        page: PageIndex,
        original: PageRect,
        current: PageRect,
        /// Whether the pointer is carrying the whole mark or one of its
        /// corners. The release builds a different replacement for each — a
        /// translation or a scaling — so the drag has to remember which it is.
        handle: TransformHandle,
    },
}

/// Which corner of a selected mark the pointer is holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// Every corner, in the order a view draws the handles.
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    /// Where this corner of `rect` is.
    pub fn of(self, rect: PageRect) -> PagePoint {
        match self {
            Corner::TopLeft => PagePoint::new(rect.left, rect.top),
            Corner::TopRight => PagePoint::new(rect.right, rect.top),
            Corner::BottomLeft => PagePoint::new(rect.left, rect.bottom),
            Corner::BottomRight => PagePoint::new(rect.right, rect.bottom),
        }
    }
}

/// What a transform drag is doing to the mark it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformHandle {
    /// The whole mark follows the pointer.
    Move,
    /// This corner follows the pointer; the opposite one stays where it is.
    Resize(Corner),
}

/// The smallest a mark may be dragged down to, in page points.
///
/// A rectangle dragged through zero comes out inside-out, and one dragged to
/// nothing is a mark the reader can no longer find in order to undo it.
pub const MIN_MARK_SIZE: f32 = 4.0;

impl TransformHandle {
    /// Where `original` ends up when the pointer has moved by `(dx, dy)`.
    ///
    /// Here rather than in the view because it is arithmetic with edge cases —
    /// a corner dragged past its opposite, a drag that collapses the box — and
    /// those are worth a test that needs no window.
    pub fn applied(self, original: PageRect, dx: f32, dy: f32) -> PageRect {
        if !dx.is_finite() || !dy.is_finite() {
            return original;
        }
        let TransformHandle::Resize(corner) = self else {
            return PageRect::new(
                original.left + dx,
                original.top + dy,
                original.right + dx,
                original.bottom + dy,
            );
        };
        let (mut left, mut top, mut right, mut bottom) =
            (original.left, original.top, original.right, original.bottom);
        match corner {
            Corner::TopLeft => {
                left += dx;
                top += dy;
            }
            Corner::TopRight => {
                right += dx;
                top += dy;
            }
            Corner::BottomLeft => {
                left += dx;
                bottom += dy;
            }
            Corner::BottomRight => {
                right += dx;
                bottom += dy;
            }
        }
        // Clamped away from the corner that did *not* move, so a box squashed
        // flat collapses towards its anchor rather than around its centre.
        match corner {
            Corner::TopLeft | Corner::BottomLeft => left = left.min(right - MIN_MARK_SIZE),
            Corner::TopRight | Corner::BottomRight => right = right.max(left + MIN_MARK_SIZE),
        }
        match corner {
            Corner::TopLeft | Corner::TopRight => top = top.min(bottom - MIN_MARK_SIZE),
            Corner::BottomLeft | Corner::BottomRight => bottom = bottom.max(top + MIN_MARK_SIZE),
        }
        PageRect::new(left, top, right, bottom)
    }
}

impl Gesture {
    pub fn page(&self) -> PageIndex {
        match self {
            Gesture::Ink { page, .. }
            | Gesture::Selecting { page, .. }
            | Gesture::Erasing { page, .. }
            | Gesture::Marquee { page, .. }
            | Gesture::Transforming { page, .. } => *page,
        }
    }

    /// Has this gesture accumulated anything worth committing?
    pub fn is_empty(&self) -> bool {
        match self {
            Gesture::Ink { points, .. } => points.is_empty(),
            Gesture::Selecting { quads, .. } => quads.is_empty(),
            Gesture::Erasing { touched, .. } => touched.is_empty(),
            // A band that never left the point it started from is a click,
            // which gathers nothing up: what a click means is decided by the
            // caller, which has the marks to test it against (§8.4).
            Gesture::Marquee { anchor, head, .. } => anchor == head,
            Gesture::Transforming {
                original, current, ..
            } => original == current,
        }
    }
}

/// What releasing the pointer produced.
#[derive(Debug, Clone, PartialEq)]
pub enum GestureOutcome {
    /// One atomic user action: one revision, one undo entry (§9.1). Several
    /// commands only for an eraser sweep or a compound replacement.
    Commit(Vec<AnnotationCommand>),
    /// The gesture ended without making a mark — a press that went nowhere, a
    /// selection that resolved to no text, a drag that returned to where it
    /// started. Nothing is sent, and nothing is reported as an error.
    Nothing,
}

impl GestureOutcome {
    pub fn commands(&self) -> &[AnnotationCommand] {
        match self {
            GestureOutcome::Commit(commands) => commands,
            GestureOutcome::Nothing => &[],
        }
    }
}

/// The armed tool, the open gesture and the transient pointer effects.
///
/// This is the type §5.3 splits out of the presenter's `Annotations`: what the
/// user is doing, with none of what they have done. Committed marks live in
/// the open PDF document and nowhere else (A1).
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationInteraction {
    tool: Option<AnnotationTool>,
    gesture: Option<Gesture>,
    /// Normalised, because these two are drawn by the presenter's existing
    /// transient painting path and never reach the PDF API (§3.2).
    pub pointer: Option<(f32, f32)>,
    pub spotlight: Option<(f32, f32)>,
    /// The annotations the user has selected, for move, resize and delete.
    /// Application state, not document state (§8.4).
    ///
    /// A list because the rubber band takes several at once. In `/Annots`
    /// order, and without repeats: the same mark held twice would be deleted
    /// twice by one press.
    selected: Vec<AnnotationId>,
    /// The style the next mark will be made in, per tool.
    ink_style: MarkStyle,
    highlight_style: MarkStyle,
    /// Which mark the highlighter makes: a wash, an underline or a strikeout.
    ///
    /// Beside the style rather than inside it, because it is not how the mark
    /// is painted but *which mark it is* — it chooses the PDF subtype, and the
    /// opacity follows from it rather than the other way round.
    markup_kind: MarkupKind,
    /// What placed text — free text and notes — is written in. Separate from
    /// the ink's: the pen drawing in green does not make the commentary green.
    text_style: MarkStyle,
}

impl Default for AnnotationInteraction {
    /// The same thing as [`AnnotationInteraction::new`], spelled out so a
    /// derived `Default` can never quietly hand the highlighter the ink's
    /// opaque style again.
    fn default() -> Self {
        Self::new()
    }
}

impl AnnotationInteraction {
    pub fn new() -> Self {
        Self {
            tool: None,
            gesture: None,
            pointer: None,
            spotlight: None,
            selected: Vec::new(),
            ink_style: MarkStyle::default(),
            highlight_style: MarkStyle::highlighter(),
            markup_kind: MarkupKind::Highlight,
            text_style: MarkStyle::default(),
        }
    }

    pub fn tool(&self) -> Option<AnnotationTool> {
        self.tool
    }

    /// Arm a tool. Doing so abandons any open gesture without mutating the
    /// document: changing tools mid-stroke is a change of mind.
    pub fn arm(&mut self, tool: Option<AnnotationTool>) {
        if self.tool != tool {
            self.gesture = None;
        }
        self.tool = tool;
    }

    pub fn gesture(&self) -> Option<&Gesture> {
        self.gesture.as_ref()
    }

    pub fn is_drawing(&self) -> bool {
        self.gesture.is_some()
    }

    /// Everything the reader is holding, in paint order.
    pub fn selection(&self) -> &[AnnotationId] {
        &self.selected
    }

    /// The one mark the reader is holding, when they are holding exactly one.
    ///
    /// `None` for a band's worth of marks rather than the first of them: the
    /// things this answers — where to put the resize grips, what to reopen for
    /// rewriting — are questions about a single mark, and picking one out of
    /// several would act on a mark the reader did not point at.
    pub fn selected(&self) -> Option<&AnnotationId> {
        match self.selected.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    pub fn is_selected(&self, id: &AnnotationId) -> bool {
        self.selected.contains(id)
    }

    pub fn select(&mut self, id: Option<AnnotationId>) {
        self.selected = id.into_iter().collect();
    }

    /// Hold everything the band gathered up. Deduplicated, because the caller
    /// assembles it from page geometry and a repeat would be one mark deleted
    /// twice.
    pub fn select_many(&mut self, ids: Vec<AnnotationId>) {
        self.selected = Vec::with_capacity(ids.len());
        for id in ids {
            if !self.selected.contains(&id) {
                self.selected.push(id);
            }
        }
    }

    pub fn ink_style(&self) -> MarkStyle {
        self.ink_style
    }

    pub fn highlight_style(&self) -> MarkStyle {
        self.highlight_style
    }

    pub fn set_ink_style(&mut self, style: MarkStyle) {
        self.ink_style = style.sanitised();
    }

    pub fn set_highlight_style(&mut self, style: MarkStyle) {
        self.highlight_style = style.sanitised();
    }

    pub fn markup_kind(&self) -> MarkupKind {
        self.markup_kind
    }

    /// Choose which mark the highlighter makes.
    ///
    /// The opacity follows: a wash is translucent and a rule is not, and a
    /// reader who switched to underlining and got a 40% grey line would have
    /// been given a faded mark nobody asked for.
    pub fn set_markup_kind(&mut self, kind: MarkupKind) {
        self.markup_kind = kind;
        self.highlight_style.opacity = kind.opacity();
    }

    pub fn text_style(&self) -> MarkStyle {
        self.text_style
    }

    pub fn set_text_style(&mut self, style: MarkStyle) {
        self.text_style = style.sanitised();
    }

    /// Abandon whatever is open. Escape, a tool change, a page change and a
    /// lost pointer capture all land here, and none of them touches the PDF.
    pub fn cancel(&mut self) {
        self.gesture = None;
    }

    /// Everything a page change clears: the transient effects and any
    /// unfinished gesture, and nothing else. Committed annotations are in the
    /// document and stay there (§8.7).
    pub fn clear_transient(&mut self) {
        self.gesture = None;
        self.pointer = None;
        self.spotlight = None;
    }

    /// Pointer-down on `page` at `at`.
    ///
    /// Returns `false` when the armed tool has no gesture — the pointer and
    /// spotlight tools are transient effects, not gestures — so the caller
    /// knows the press was not consumed.
    pub fn begin(&mut self, page: PageIndex, at: PagePoint) -> bool {
        let Some(tool) = self.tool else {
            return false;
        };
        self.gesture = match tool {
            AnnotationTool::Ink => Some(Gesture::Ink {
                page,
                points: vec![InkPoint { at }],
                style: self.ink_style,
            }),
            AnnotationTool::Highlighter => Some(Gesture::Selecting {
                page,
                anchor: at,
                head: at,
                quads: Vec::new(),
                text: String::new(),
                style: MarkStyle {
                    opacity: self.markup_kind.opacity(),
                    ..self.highlight_style
                },
                kind: self.markup_kind,
            }),
            AnnotationTool::Eraser => Some(Gesture::Erasing {
                page,
                at,
                touched: Vec::new(),
            }),
            // The band, and the only thing this tool does: picking one mark up
            // needs no tool at all (§8.4).
            AnnotationTool::Select => Some(Gesture::Marquee {
                page,
                anchor: at,
                head: at,
            }),
            AnnotationTool::Pointer
            | AnnotationTool::Spotlight
            | AnnotationTool::Text
            | AnnotationTool::Note
            | AnnotationTool::Stamp => None,
        };
        self.gesture.is_some()
    }

    /// Pointer movement. Returns `true` when the gesture changed and the
    /// preview needs redrawing.
    pub fn extend(&mut self, at: PagePoint) -> bool {
        match &mut self.gesture {
            Some(Gesture::Ink { points, .. }) => {
                let candidate = InkPoint { at };
                if points.len() >= MAX_INK_POINTS
                    || !accept_sample(points.last().copied(), candidate)
                {
                    return false;
                }
                points.push(candidate);
                true
            }
            Some(Gesture::Selecting { head, .. }) => {
                if *head == at {
                    return false;
                }
                *head = at;
                true
            }
            Some(Gesture::Erasing { at: current_at, .. }) => {
                *current_at = at;
                true
            }
            Some(Gesture::Marquee { head, .. }) => {
                if *head == at {
                    return false;
                }
                *head = at;
                true
            }
            Some(Gesture::Transforming { .. }) | None => false,
        }
    }

    /// What the open selection gesture is asking the engine to resolve.
    ///
    /// `None` unless a text selection is in progress. The UI draws the live
    /// selection from the quads the engine returns and may re-query as the
    /// drag moves; the query itself is read-only and mutates nothing (§8.2).
    pub fn pending_selection(&self) -> Option<(PageIndex, PagePoint, PagePoint)> {
        match &self.gesture {
            Some(Gesture::Selecting {
                page, anchor, head, ..
            }) => Some((*page, *anchor, *head)),
            _ => None,
        }
    }

    /// The engine answered a selection query for the open drag.
    pub fn set_selection_result(&mut self, quads: Vec<PageQuad>, text: String) {
        if let Some(Gesture::Selecting {
            quads: current,
            text: current_text,
            ..
        }) = &mut self.gesture
        {
            *current = quads;
            *current_text = text;
        }
    }

    /// The rectangle the open rubber band covers, normalised so a band dragged
    /// up and to the left is the same rectangle as one dragged down and to the
    /// right.
    ///
    /// `None` unless a band is open. What the caller tests annotations against
    /// on release, and what the view draws while the drag lasts.
    pub fn marquee(&self) -> Option<(PageIndex, PageRect)> {
        match &self.gesture {
            Some(Gesture::Marquee { page, anchor, head }) => Some((
                *page,
                PageRect::new(
                    anchor.x.min(head.x),
                    anchor.y.min(head.y),
                    anchor.x.max(head.x),
                    anchor.y.max(head.y),
                ),
            )),
            _ => None,
        }
    }

    /// The eraser passed over an annotation. Recorded once however many times
    /// the sweep crosses it.
    pub fn touch_for_erase(&mut self, id: AnnotationId) {
        if let Some(Gesture::Erasing { touched, .. }) = &mut self.gesture {
            if !touched.contains(&id) {
                touched.push(id);
            }
        }
    }

    /// Start moving or resizing a selected annotation.
    pub fn begin_transform(
        &mut self,
        id: AnnotationId,
        page: PageIndex,
        bounds: PageRect,
        handle: TransformHandle,
    ) {
        self.gesture = Some(Gesture::Transforming {
            id,
            page,
            original: bounds,
            current: bounds,
            handle,
        });
    }

    /// Where the drag has got to. The document is not touched until release.
    pub fn set_transform(&mut self, bounds: PageRect) {
        if let Some(Gesture::Transforming { current, .. }) = &mut self.gesture {
            *current = bounds;
        }
    }

    /// Pointer-up. Consumes the gesture and produces at most one atomic
    /// action's worth of commands.
    ///
    /// `page_geometry` is the page the gesture happened on; a draft that does
    /// not validate against it commits nothing, because a mark that cannot be
    /// written is better refused here than half-applied by the worker.
    pub fn finish(&mut self, page_geometry: &PageGeometry) -> GestureOutcome {
        let Some(gesture) = self.gesture.take() else {
            return GestureOutcome::Nothing;
        };
        let commands = match gesture {
            Gesture::Ink {
                page,
                points,
                style,
            } => {
                let points = simplify(&points, SIMPLIFY_TOLERANCE);
                let draft = AnnotationDraft::Ink(InkDraft {
                    page,
                    points,
                    style,
                });
                vec![AnnotationCommand::Create(draft)]
            }
            Gesture::Selecting {
                page,
                quads,
                text,
                style,
                kind,
                ..
            } => {
                let draft = AnnotationDraft::Highlight(HighlightDraft {
                    page,
                    kind,
                    quads,
                    text,
                    style,
                });
                vec![AnnotationCommand::Create(draft)]
            }
            Gesture::Erasing { touched, .. } => touched
                .into_iter()
                .map(|id| AnnotationCommand::Delete { id })
                .collect(),
            // A transform is *not* finished here. Its replacement draft needs
            // the annotation's contents, which live with the caller and not in
            // the gesture, so the caller reads `Gesture::Transforming` and
            // builds the `Replace` itself before ever reaching this point.
            //
            // Nothing, rather than the bare `Delete` this once returned: a
            // caller that forgot to intercept would have had the mark it was
            // moving silently deleted, and losing a mark is far worse than
            // losing a move (A3).
            Gesture::Transforming { .. } => return GestureOutcome::Nothing,
            // Nor is a band. It changes what is *held*, which is application
            // state, and the caller has the page's annotations to test it
            // against; nothing about it reaches the document.
            Gesture::Marquee { .. } => return GestureOutcome::Nothing,
        };

        let ok = commands.iter().all(|command| {
            command
                .draft()
                .map(|draft| draft.validate(page_geometry).is_ok())
                .unwrap_or(true)
        });
        if !ok || commands.is_empty() {
            return GestureOutcome::Nothing;
        }
        GestureOutcome::Commit(commands)
    }

    /// A free-text, note or stamp mark placed by a click rather than a drag.
    ///
    /// These three have no gesture: the click chooses a spot, and the content
    /// arrives afterwards from a text editor or a palette. Committing is one
    /// command, as for a stroke.
    pub fn place(
        &self,
        page: PageIndex,
        at: PagePoint,
        content: PlacedMark,
        page_geometry: &PageGeometry,
    ) -> GestureOutcome {
        let draft = match content {
            PlacedMark::FreeText { text, source, size } => {
                AnnotationDraft::FreeText(FreeTextDraft {
                    page,
                    rect: PageRect::new(at.x, at.y, at.x + size.0, at.y + size.1),
                    text,
                    source,
                    style: self.text_style,
                })
            }
            PlacedMark::Note { text } => AnnotationDraft::Note(NoteDraft {
                page,
                at,
                text,
                // A note wears the sticky-note colour rather than the text
                // tool's: `/C` on a `/Text` annotation paints the icon, not
                // the words, and the words are drawn from `/Contents` by
                // whatever viewer opens the popup.
                style: MarkStyle::note(),
            }),
            PlacedMark::Stamp { mark, size } => AnnotationDraft::Stamp(StampDraft {
                page,
                rect: PageRect::new(at.x, at.y, at.x + size.0, at.y + size.1),
                mark,
                style: self.ink_style,
                source: None,
            }),
        };
        if draft.validate(page_geometry).is_err() {
            return GestureOutcome::Nothing;
        }
        GestureOutcome::Commit(vec![AnnotationCommand::Create(draft)])
    }
}

/// What a click-placed mark is.
#[derive(Debug, Clone, PartialEq)]
pub enum PlacedMark {
    FreeText {
        text: String,
        source: TextSource,
        /// Width and height in page points.
        size: (f32, f32),
    },
    Note {
        text: String,
    },
    Stamp {
        mark: StampMark,
        size: (f32, f32),
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::draft::AnnotationKind;

    fn page() -> PageGeometry {
        PageGeometry::upright(612.0, 792.0)
    }

    fn drawing() -> AnnotationInteraction {
        let mut interaction = AnnotationInteraction::new();
        interaction.arm(Some(AnnotationTool::Ink));
        assert!(interaction.begin(PageIndex(0), PagePoint::new(100.0, 100.0)));
        interaction
    }

    #[test]
    fn a_stroke_commits_exactly_one_create_command() {
        let mut interaction = drawing();
        for step in 1..40 {
            interaction.extend(PagePoint::new(100.0 + step as f32 * 3.0, 100.0));
        }
        let outcome = interaction.finish(&page());
        let commands = outcome.commands();
        assert_eq!(commands.len(), 1, "one gesture is one undo entry");
        let draft = commands[0].draft().unwrap();
        assert_eq!(draft.kind(), AnnotationKind::Ink);
        assert!(!interaction.is_drawing(), "the gesture is gone");
    }

    #[test]
    fn a_committed_stroke_is_simplified_before_it_is_sent() {
        let mut interaction = drawing();
        for step in 1..200 {
            interaction.extend(PagePoint::new(100.0 + step as f32, 100.0));
        }
        let AnnotationDraft::Ink(ink) = interaction
            .finish(&page())
            .commands()
            .first()
            .unwrap()
            .draft()
            .unwrap()
            .clone()
        else {
            panic!("an ink gesture makes an ink draft")
        };
        assert_eq!(ink.points.len(), 2, "a straight line needs two points");
    }

    #[test]
    fn samples_on_top_of_each_other_do_not_grow_the_stroke() {
        let mut interaction = drawing();
        assert!(!interaction.extend(PagePoint::new(100.0, 100.0)));
        assert!(!interaction.extend(PagePoint::new(100.05, 100.0)));
        assert!(interaction.extend(PagePoint::new(140.0, 100.0)));
    }

    #[test]
    fn a_gesture_cannot_grow_past_the_point_limit() {
        let mut interaction = drawing();
        for step in 1..(MAX_INK_POINTS + 100) {
            interaction.extend(PagePoint::new(
                100.0 + (step % 400) as f32,
                100.0 + (step / 400) as f32,
            ));
        }
        let Some(Gesture::Ink { points, .. }) = interaction.gesture() else {
            panic!("still drawing")
        };
        assert!(points.len() <= MAX_INK_POINTS);
    }

    #[test]
    fn cancelling_leaves_the_document_alone() {
        let mut interaction = drawing();
        interaction.extend(PagePoint::new(200.0, 200.0));
        interaction.cancel();
        assert_eq!(interaction.finish(&page()), GestureOutcome::Nothing);
    }

    #[test]
    fn changing_tool_mid_stroke_abandons_it() {
        let mut interaction = drawing();
        interaction.arm(Some(AnnotationTool::Eraser));
        assert!(!interaction.is_drawing());
        // Re-arming the same tool is not a change of mind and must not drop a
        // stroke somebody is in the middle of.
        let mut interaction = drawing();
        interaction.arm(Some(AnnotationTool::Ink));
        assert!(interaction.is_drawing());
    }

    #[test]
    fn a_page_change_clears_the_transient_effects_and_nothing_else() {
        let mut interaction = drawing();
        interaction.pointer = Some((0.5, 0.5));
        interaction.spotlight = Some((0.5, 0.5));
        interaction.select(Some(super::super::id::IdGenerator::new(0).next_id()));
        interaction.clear_transient();
        assert!(interaction.pointer.is_none());
        assert!(interaction.spotlight.is_none());
        assert!(!interaction.is_drawing());
        assert!(
            interaction.selected().is_some(),
            "a selection is not a transient pointer effect"
        );
    }

    #[test]
    fn the_pointer_and_spotlight_tools_start_no_gesture() {
        for tool in [AnnotationTool::Pointer, AnnotationTool::Spotlight] {
            let mut interaction = AnnotationInteraction::new();
            interaction.arm(Some(tool));
            assert!(!interaction.begin(PageIndex(0), PagePoint::new(1.0, 1.0)));
            assert!(!interaction.is_drawing());
        }
        let mut unarmed = AnnotationInteraction::new();
        assert!(!unarmed.begin(PageIndex(0), PagePoint::new(1.0, 1.0)));
    }

    #[test]
    fn one_eraser_sweep_is_one_transaction_however_many_marks_it_takes() {
        let mut generator = super::super::id::IdGenerator::new(3);
        let (a, b) = (generator.next_id(), generator.next_id());
        let mut interaction = AnnotationInteraction::new();
        interaction.arm(Some(AnnotationTool::Eraser));
        interaction.begin(PageIndex(0), PagePoint::new(10.0, 10.0));
        interaction.touch_for_erase(a.clone());
        interaction.touch_for_erase(b.clone());
        // Crossing the same stroke twice does not delete it twice.
        interaction.touch_for_erase(a.clone());
        let outcome = interaction.finish(&page());
        assert_eq!(
            outcome.commands(),
            &[
                AnnotationCommand::Delete { id: a },
                AnnotationCommand::Delete { id: b }
            ]
        );
    }

    #[test]
    fn an_eraser_sweep_that_touched_nothing_commits_nothing() {
        let mut interaction = AnnotationInteraction::new();
        interaction.arm(Some(AnnotationTool::Eraser));
        interaction.begin(PageIndex(0), PagePoint::new(10.0, 10.0));
        interaction.extend(PagePoint::new(20.0, 20.0));
        assert_eq!(interaction.finish(&page()), GestureOutcome::Nothing);
    }

    #[test]
    fn a_selection_that_resolved_to_no_text_commits_nothing() {
        let mut interaction = AnnotationInteraction::new();
        interaction.arm(Some(AnnotationTool::Highlighter));
        interaction.begin(PageIndex(0), PagePoint::new(50.0, 50.0));
        interaction.extend(PagePoint::new(300.0, 50.0));
        assert_eq!(interaction.finish(&page()), GestureOutcome::Nothing);
    }

    #[test]
    fn a_resolved_selection_commits_one_highlight_carrying_its_text() {
        let mut interaction = AnnotationInteraction::new();
        interaction.arm(Some(AnnotationTool::Highlighter));
        interaction.begin(PageIndex(0), PagePoint::new(50.0, 50.0));
        interaction.extend(PagePoint::new(300.0, 50.0));
        interaction.set_selection_result(
            vec![PageQuad::from_rect(PageRect::new(50.0, 44.0, 300.0, 58.0))],
            "the selected words".into(),
        );
        let outcome = interaction.finish(&page());
        let AnnotationDraft::Highlight(highlight) = outcome.commands()[0].draft().unwrap().clone()
        else {
            panic!("the highlighter makes a highlight")
        };
        assert_eq!(highlight.text, "the selected words");
        assert_eq!(highlight.quads.len(), 1);
    }

    #[test]
    fn the_nib_chooses_the_subtype_the_sweep_commits() {
        use crate::annotation::MarkupKind;

        for kind in MarkupKind::ALL {
            let mut interaction = AnnotationInteraction::new();
            interaction.set_markup_kind(kind);
            interaction.arm(Some(AnnotationTool::Highlighter));
            interaction.begin(PageIndex(0), PagePoint::new(50.0, 50.0));
            interaction.extend(PagePoint::new(300.0, 50.0));
            interaction.set_selection_result(
                vec![PageQuad::from_rect(PageRect::new(50.0, 44.0, 300.0, 58.0))],
                "the selected words".into(),
            );
            let outcome = interaction.finish(&page());
            let draft = outcome.commands()[0].draft().unwrap().clone();
            assert_eq!(
                draft.kind(),
                crate::annotate::AnnotationKind::from(kind),
                "{kind:?} must reach the file as its own subtype"
            );
            // A rule is opaque and a wash is not: the nib carries the opacity
            // rather than the style the tool happened to be left in.
            assert_eq!(draft.style().opacity, kind.opacity(), "{kind:?}");
        }
    }

    #[test]
    fn changing_the_nib_mid_sweep_leaves_the_mark_under_the_hand_alone() {
        use crate::annotation::MarkupKind;

        let mut interaction = AnnotationInteraction::new();
        interaction.arm(Some(AnnotationTool::Highlighter));
        interaction.begin(PageIndex(0), PagePoint::new(50.0, 50.0));
        interaction.set_markup_kind(MarkupKind::StrikeOut);
        interaction.extend(PagePoint::new(300.0, 50.0));
        interaction.set_selection_result(
            vec![PageQuad::from_rect(PageRect::new(50.0, 44.0, 300.0, 58.0))],
            "the selected words".into(),
        );
        let outcome = interaction.finish(&page());
        assert_eq!(
            outcome.commands()[0].draft().unwrap().kind(),
            crate::annotate::AnnotationKind::Highlight,
            "the sweep fixed its kind when it began"
        );
    }

    #[test]
    fn a_press_that_went_nowhere_still_makes_the_dot_it_meant_to() {
        let mut interaction = drawing();
        let outcome = interaction.finish(&page());
        assert_eq!(outcome.commands().len(), 1);
    }

    #[test]
    fn a_drag_that_returned_to_where_it_started_is_not_an_edit() {
        let mut interaction = AnnotationInteraction::new();
        let id = super::super::id::IdGenerator::new(1).next_id();
        let bounds = PageRect::new(10.0, 10.0, 100.0, 40.0);
        interaction.begin_transform(id, PageIndex(0), bounds, TransformHandle::Move);
        interaction.set_transform(PageRect::new(20.0, 20.0, 110.0, 50.0));
        interaction.set_transform(bounds);
        assert_eq!(interaction.finish(&page()), GestureOutcome::Nothing);
    }

    #[test]
    fn finishing_a_transform_here_never_deletes_the_mark() {
        // The caller assembles a transform's `Replace` from the annotation's
        // contents and intercepts before `finish`. A caller that forgot must
        // lose the move, not the mark (A3).
        let mut interaction = AnnotationInteraction::new();
        let id = super::super::id::IdGenerator::new(1).next_id();
        let bounds = PageRect::new(10.0, 10.0, 100.0, 40.0);
        interaction.begin_transform(id, PageIndex(0), bounds, TransformHandle::Move);
        interaction.set_transform(PageRect::new(60.0, 60.0, 150.0, 90.0));
        assert_eq!(interaction.finish(&page()), GestureOutcome::Nothing);
    }

    #[test]
    fn moving_carries_every_edge_the_same_distance() {
        let original = PageRect::new(10.0, 20.0, 110.0, 60.0);
        assert_eq!(
            TransformHandle::Move.applied(original, 5.0, -7.0),
            PageRect::new(15.0, 13.0, 115.0, 53.0)
        );
    }

    #[test]
    fn a_corner_drag_leaves_the_opposite_corner_where_it_was() {
        let original = PageRect::new(10.0, 20.0, 110.0, 60.0);
        assert_eq!(
            TransformHandle::Resize(Corner::TopLeft).applied(original, -4.0, -6.0),
            PageRect::new(6.0, 14.0, 110.0, 60.0)
        );
        assert_eq!(
            TransformHandle::Resize(Corner::BottomRight).applied(original, 30.0, 10.0),
            PageRect::new(10.0, 20.0, 140.0, 70.0)
        );
    }

    #[test]
    fn a_corner_dragged_past_its_opposite_stops_rather_than_inverting() {
        // A box dragged through zero comes out inside-out, and a mark with a
        // negative width is one nobody can find again in order to undo it.
        let original = PageRect::new(10.0, 20.0, 110.0, 60.0);

        let squashed = TransformHandle::Resize(Corner::TopLeft).applied(original, 500.0, 500.0);
        assert!(squashed.width() >= MIN_MARK_SIZE && squashed.height() >= MIN_MARK_SIZE);
        assert_eq!(squashed.right, 110.0, "the anchored corner moved");
        assert_eq!(squashed.bottom, 60.0, "the anchored corner moved");

        let squashed =
            TransformHandle::Resize(Corner::BottomRight).applied(original, -500.0, -500.0);
        assert!(squashed.width() >= MIN_MARK_SIZE && squashed.height() >= MIN_MARK_SIZE);
        assert_eq!(squashed.left, 10.0, "the anchored corner moved");
        assert_eq!(squashed.top, 20.0, "the anchored corner moved");
    }

    #[test]
    fn a_drag_by_something_that_is_not_a_number_moves_nothing() {
        let original = PageRect::new(10.0, 20.0, 110.0, 60.0);
        assert_eq!(
            TransformHandle::Move.applied(original, f32::NAN, 0.0),
            original
        );
        assert_eq!(
            TransformHandle::Resize(Corner::TopLeft).applied(original, 0.0, f32::INFINITY),
            original
        );
    }

    #[test]
    fn a_placed_mark_off_the_page_commits_nothing() {
        let interaction = AnnotationInteraction::new();
        let outcome = interaction.place(
            PageIndex(0),
            PagePoint::new(99_000.0, 10.0),
            PlacedMark::Note {
                text: "hello".into(),
            },
            &page(),
        );
        assert_eq!(outcome, GestureOutcome::Nothing);
    }

    #[test]
    fn a_placed_note_commits_one_create() {
        let interaction = AnnotationInteraction::new();
        let outcome = interaction.place(
            PageIndex(0),
            PagePoint::new(100.0, 100.0),
            PlacedMark::Note {
                text: "remember this".into(),
            },
            &page(),
        );
        assert_eq!(outcome.commands().len(), 1);
        assert_eq!(
            outcome.commands()[0].draft().unwrap().kind(),
            AnnotationKind::Note
        );
    }

    #[test]
    fn a_default_interaction_is_the_same_as_a_new_one() {
        // The derived `Default` once handed the highlighter the ink's opaque
        // style, and every session built by `..Default::default()` drew
        // highlights as solid bars.
        assert_eq!(
            AnnotationInteraction::default(),
            AnnotationInteraction::new()
        );
        let translucent = AnnotationInteraction::default().highlight_style();
        assert!(translucent.opacity < 1.0, "a highlight is see-through");
    }

    #[test]
    fn styles_are_repaired_on_the_way_in() {
        let mut interaction = AnnotationInteraction::new();
        interaction.set_ink_style(MarkStyle {
            opacity: 9.0,
            ..MarkStyle::default()
        });
        assert_eq!(interaction.ink_style().opacity, 1.0);
        assert_eq!(interaction.highlight_style().opacity, 0.4);
    }
}
