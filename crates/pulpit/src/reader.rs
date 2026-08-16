//! The reader session: what document mode holds while a document is open.
//!
//! Mode is which layout is mounted, not which document is loaded (§2.3), so
//! this lives beside the presentation state rather than replacing it: a
//! session can be a presentation, a reader, or the same file in both, and
//! switching between them keeps the document open and the revision unchanged.
//!
//! Nothing here talks to PDFium. It holds the viewport, the armed tool and the
//! answers the worker last gave, and turns a [`ReadCommand`] into a new
//! viewport — which is why it is testable without a window or a PDF.

use std::collections::{HashMap, HashSet};

use pulpit_core::annotate::AnnotationInteraction;
use pulpit_core::annotation::AnnotationTool;
use pulpit_core::page::{PageGeometry, PageIndex, PagePoint};
use pulpit_render::document::{
    CompatibilityLevel, DocumentRevision, DocumentTransaction, DocumentWarning, FormField,
    TextSelection,
};

use crate::widgets::context::{OutlineRow, ReaderData, ReaderPage};
use crate::widgets::document::model::{Column, ReaderControls, Zoom};
use crate::widgets::event::ReadCommand;

/// What a pointer release produced.
#[derive(Debug)]
pub enum Released {
    /// Nothing worth committing: a press that went nowhere, an eraser sweep
    /// that touched no mark, a drag that returned to where it started.
    Nothing,
    /// One atomic user action, ready to send.
    Commit(DocumentTransaction),
    /// A text selection the engine has to resolve before it can be committed.
    /// The answer arrives through [`ReaderSession::selection_resolved`].
    AwaitingSelection {
        page: PageIndex,
        selection: TextSelection,
    },
}

/// Which stack an applied transaction's undo operation belongs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliedKind {
    /// An ordinary edit. Its inverse can be undone, and whatever had been
    /// undone before is no longer reachable.
    Edit,
    /// An undo. Its inverse redoes the thing that was undone.
    Undo,
    /// A redo, whose inverse undoes it again.
    Redo,
}

/// What the reader is looking at.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct ReaderSession {
    /// `None` when no document is open in document mode — which is different
    /// from a document with no pages.
    open: bool,
    /// Every page's canonical geometry, as the worker reported it.
    pages: Vec<PageGeometry>,
    column: Column,
    controls: ReaderControls,
    /// The size of the cell the page surface was last drawn in, in layout
    /// points. The zoom's fits are meaningless without it.
    cell: (f32, f32),
    scale: f32,
    outline: Vec<OutlineRow>,
    fields: Vec<FormField>,
    level: CompatibilityLevel,
    warnings: Vec<DocumentWarning>,
    /// What is being typed into the page box, while it is being typed.
    page_entry: Option<String>,
    dirty: bool,
    revision: DocumentRevision,
    /// How many actions are behind and ahead of the present, for the two
    /// history controls. The operations themselves live in the worker.
    undo_depth: usize,
    redo_depth: usize,
    /// The pages that have been drawn, by page and by the width they were
    /// drawn at.
    ///
    /// Keyed by width because a frame rendered for a narrower column is the
    /// wrong picture for a wider one — sharp at the size it was made and soft
    /// at any other. A frame at the wrong width is still shown while the right
    /// one is rendered, because a blank sheet is worse than a soft one.
    frames: HashMap<PageIndex, RenderedPage>,
    /// Renders that have been asked for and not yet answered, so the same page
    /// is not asked for again on every tick while the first answer is in
    /// flight.
    in_flight: HashSet<PageIndex>,
    /// The undo operations the worker handed back, newest last.
    ///
    /// Held here rather than in the worker because the worker answers one
    /// request at a time and has no opinion about history; this is the stack
    /// the two history controls read, and it is the same stack for annotation
    /// and field edits, in user action order (§8.6).
    undo_stack: Vec<pulpit_render::document::DocumentUndo>,
    redo_stack: Vec<pulpit_render::document::DocumentUndo>,
    /// What the pointer is doing to the page: the armed tool and any open
    /// gesture, and nothing that has been committed (§5.3, A2).
    interaction: AnnotationInteraction,
    /// Where the pointer last was, on which page. Transient, and never
    /// snapshotted (§3.2).
    cursor: Option<(PageIndex, PagePoint)>,
    /// True between a release that needed a selection resolved and the answer
    /// that resolved it.
    awaiting_selection: bool,
    /// What is on each page that has been looked at, for hit-testing.
    ///
    /// A cache of the document's answer rather than a second store of the
    /// marks (A1): it holds geometry and identity and nothing else, and it is
    /// dropped for a page the moment that page is edited.
    annotations: HashMap<PageIndex, Vec<pulpit_core::annotate::AnnotationHit>>,
    /// The same answer in full, for the one thing a hit-test cannot do: build
    /// the replacement a move commits. Dropped with the hit list.
    summaries: HashMap<PageIndex, Vec<pulpit_render::document::AnnotationSummary>>,
    /// Where a transform started, so movement can be measured from it.
    transform_origin: Option<PagePoint>,
    /// Does the document carry an AcroForm at all?
    ///
    /// Separate from whether any fields were listed, because the two differ
    /// while filling is unimplemented: a form document with an empty field
    /// list must not be described as having no form (§8.6).
    has_form: bool,
}

/// One page as it was last drawn.
#[derive(Debug, Clone)]
struct RenderedPage {
    handle: iced::widget::image::Handle,
    /// The width it was drawn at, in physical pixels.
    width: u32,
    /// The revision it contains. A frame older than the document is shown
    /// until a newer one arrives, and never painted over a newer one (A7).
    revision: DocumentRevision,
}

// A few readers of this state are the recovery path's, which §11 has yet to
// write; the rest is live.
#[allow(dead_code)]
impl ReaderSession {
    pub fn new() -> ReaderSession {
        ReaderSession {
            level: CompatibilityLevel::AnnotateOnly,
            scale: 1.0,
            // A cell size that is not yet known: the first draw supplies the
            // real one, and until then a fit resolves to something sane rather
            // than to a division by zero.
            cell: (800.0, 600.0),
            ..ReaderSession::default()
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn controls(&self) -> &ReaderControls {
        &self.controls
    }

    pub fn tool(&self) -> Option<AnnotationTool> {
        self.controls.tool
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// A document was opened. Everything about the previous one goes.
    pub fn opened(
        &mut self,
        pages: Vec<PageGeometry>,
        level: CompatibilityLevel,
        warnings: Vec<DocumentWarning>,
    ) {
        *self = ReaderSession {
            open: true,
            pages,
            level,
            warnings,
            ..ReaderSession::new()
        };
        self.relayout();
    }

    pub fn closed(&mut self) {
        *self = ReaderSession::new();
    }

    pub fn set_has_form(&mut self, has_form: bool) {
        self.has_form = has_form;
    }

    pub fn set_outline(&mut self, outline: Vec<OutlineRow>) {
        self.outline = outline;
    }

    pub fn set_fields(&mut self, fields: Vec<FormField>) {
        self.fields = fields;
    }

    /// The worker applied something.
    ///
    /// `redoing` says which stack the operation it handed back belongs on: an
    /// ordinary edit pushes onto undo and clears the redo stack, because the
    /// future the user had taken back is no longer reachable from where they
    /// now are; an undo pushes onto redo, and a redo back onto undo.
    pub fn applied(&mut self, applied: &pulpit_render::document::Applied, kind: AppliedKind) {
        self.revision = applied.document_revision;
        self.dirty = true;
        match kind {
            AppliedKind::Edit => {
                self.undo_stack.push(applied.undo.clone());
                self.redo_stack.clear();
            }
            AppliedKind::Undo => self.redo_stack.push(applied.undo.clone()),
            AppliedKind::Redo => self.undo_stack.push(applied.undo.clone()),
        }
        self.undo_depth = self.undo_stack.len();
        self.redo_depth = self.redo_stack.len();

        // Every page the transaction touched is out of date. Dropping the
        // frames rather than repainting them here is what makes A7 hold: the
        // page keeps its old picture until one carrying the new revision
        // arrives, and never shows a half-drawn state in between.
        for page in &applied.dirty_pages {
            self.in_flight.remove(page);
            // …and so is what was on it, which is what the eraser hit-tests
            // against. Keeping a stale list would let a second sweep try to
            // erase a mark that is already gone.
            self.annotations.remove(page);
            self.summaries.remove(page);
        }
    }

    /// What is on a page, for hit-testing. Replaced wholesale, because the
    /// list *is* the document's `/Annots` order and a merge would invent one.
    pub fn set_annotations(
        &mut self,
        page: PageIndex,
        summaries: &[pulpit_render::document::AnnotationSummary],
    ) {
        self.annotations.insert(
            page,
            summaries.iter().map(|summary| summary.to_hit()).collect(),
        );
        self.summaries.insert(page, summaries.to_vec());
    }

    /// The annotation the reader has picked up, if any (§8.4).
    ///
    /// Selection is application state: nothing about it is in the document,
    /// and it survives no restart.
    pub fn selected(&self) -> Option<&pulpit_core::annotate::AnnotationId> {
        self.interaction.selected()
    }

    /// The pages whose annotations are not known and are worth asking for:
    /// the ones in the window.
    pub fn annotations_wanted(&mut self) -> Vec<PageIndex> {
        if !self.open {
            return Vec::new();
        }
        self.column
            .visible(self.controls.offset, self.cell.1)
            .into_iter()
            .map(|placed| placed.page)
            .filter(|page| !self.annotations.contains_key(page))
            .collect()
    }

    /// The pointer moved over a page, at a canonical page point (A4).
    pub fn pointer_moved(&mut self, page: PageIndex, x: f32, y: f32) {
        let at = PagePoint::new(x, y);
        let from = self
            .cursor
            .filter(|(was, _)| *was == page)
            .map(|(_, at)| at);
        self.cursor = Some((page, at));
        self.interaction.extend(at);

        // A held mark follows the pointer. The document is untouched until
        // the release: what moves until then is a preview (§8.4).
        if let (Some(origin), Some(pulpit_core::annotate::Gesture::Transforming { original, .. })) =
            (self.transform_origin, self.interaction.gesture().cloned())
        {
            let (dx, dy) = (at.x - origin.x, at.y - origin.y);
            self.interaction
                .set_transform(pulpit_core::page::PageRect::new(
                    original.left + dx,
                    original.top + dy,
                    original.right + dx,
                    original.bottom + dy,
                ));
        }

        // An eraser sweep takes what it passes over, and what it passed over
        // is the segment between two samples rather than the samples
        // themselves: at speed those are tens of points apart, and testing
        // only the positions leaves marks standing in the gaps (§8.3).
        if matches!(
            self.interaction.gesture(),
            Some(pulpit_core::annotate::Gesture::Erasing { .. })
        ) {
            let tolerance = self.eraser_tolerance();
            let from = from.unwrap_or(at);
            if let Some(candidates) = self.annotations.get(&page) {
                let taken: Vec<_> =
                    pulpit_core::annotate::hit::erasable(candidates, from, at, tolerance)
                        .into_iter()
                        .map(|hit| hit.id.clone())
                        .collect();
                for id in taken {
                    self.interaction.touch_for_erase(id);
                }
            }
        }
    }

    /// The unfinished gesture on `page`, if it is there and worth drawing.
    ///
    /// Only ink and a text selection draw anything: an eraser sweep shows its
    /// effect by taking marks away, and a click-placed note has no gesture at
    /// all. Nothing here outlives the gesture, so it cannot become a second
    /// copy of a committed mark (A1).
    fn preview_for(
        &self,
        page: PageIndex,
    ) -> Option<crate::widgets::document::preview::GesturePreview> {
        use pulpit_core::annotate::Gesture;

        let gesture = self.interaction.gesture()?;
        if gesture.page() != page {
            return None;
        }
        let (points, quads, style) = match gesture {
            Gesture::Ink { points, style, .. } => (
                points.iter().map(|point| point.at).collect(),
                Vec::new(),
                *style,
            ),
            Gesture::Selecting { quads, style, .. } => (Vec::new(), quads.clone(), *style),
            Gesture::Erasing { .. } | Gesture::Transforming { .. } => return None,
        };
        let preview = crate::widgets::document::preview::GesturePreview {
            points,
            quads,
            color: style.color.rgb(),
            opacity: style.opacity,
            width: style.width,
        };
        (!preview.is_empty()).then_some(preview)
    }

    /// How far from a mark the eraser still takes it, in page points.
    ///
    /// Scaled by the zoom, because the eraser is aimed with a pointer on
    /// screen: at a small zoom a fixed page-point tolerance is a hair's
    /// breadth on the display, and at a large one it swallows neighbouring
    /// marks.
    fn eraser_tolerance(&self) -> f32 {
        /// Roughly a fingertip's worth of slack on screen.
        const ON_SCREEN_POINTS: f32 = 6.0;
        if self.scale > 0.0 {
            ON_SCREEN_POINTS / self.scale
        } else {
            ON_SCREEN_POINTS
        }
    }

    /// Where a click-placed mark would go, when the armed tool places one.
    ///
    /// Free text and notes have no gesture: the click chooses a spot and the
    /// text arrives afterwards from an editor (§8.5). This is what the
    /// application opens that editor for.
    pub fn placement(&self) -> Option<(PageIndex, PagePoint, AnnotationTool)> {
        let tool = self.controls.tool?;
        if !matches!(tool, AnnotationTool::Text | AnnotationTool::Note) {
            return None;
        }
        let (page, at) = self.cursor?;
        Some((page, at, tool))
    }

    /// Commit a mark the user placed and then typed into.
    ///
    /// Returns `None` when the text is empty or the spot is off the page:
    /// an empty note is a note nobody wrote (§8.5).
    pub fn place_text(
        &self,
        page: PageIndex,
        at: PagePoint,
        tool: AnnotationTool,
        text: String,
    ) -> Option<DocumentTransaction> {
        let geometry = self.pages.get(page.get()).copied()?;
        let content = match tool {
            AnnotationTool::Note => pulpit_core::annotate::PlacedMark::Note { text, open: false },
            _ => pulpit_core::annotate::PlacedMark::FreeText {
                text,
                source: pulpit_core::annotate::TextSource::Plain,
                // A box wide enough for a line of comment at the style's own
                // size, and tall enough for two of them. The reader can move
                // and resize it afterwards; guessing narrower would clip the
                // first thing anybody types.
                size: (
                    (self.interaction.ink_style().font_size * 18.0).min(geometry.width),
                    self.interaction.ink_style().font_size * 2.4,
                ),
            },
        };
        let outcome = self.interaction.place(page, at, content, &geometry);
        let commands = outcome.commands();
        (!commands.is_empty()).then(|| DocumentTransaction::from_annotations(commands.to_vec()))
    }

    /// Place a mark generated from Typst markup (§7.4).
    ///
    /// A `/Stamp` whose appearance is the rendered picture and whose
    /// namespaced entry is the source, so other viewers show what it looks
    /// like and pulpit can reopen what it says. The size comes from the
    /// compile rather than being guessed: Typst has already decided how much
    /// room the markup needs.
    pub fn place_typst(
        &self,
        page: PageIndex,
        at: PagePoint,
        source: String,
        rendered: crate::typst_annotation::RasterisedText,
    ) -> Option<DocumentTransaction> {
        let geometry = self.pages.get(page.get()).copied()?;
        let draft =
            pulpit_core::annotate::AnnotationDraft::Stamp(pulpit_core::annotate::StampDraft {
                page,
                rect: pulpit_core::page::PageRect::new(
                    at.x,
                    at.y,
                    at.x + rendered.width_pt,
                    at.y + rendered.height_pt,
                ),
                mark: pulpit_core::annotate::StampMark::Image {
                    pixel_width: rendered.pixel_width,
                    pixel_height: rendered.pixel_height,
                    rgba: rendered.rgba,
                },
                style: self.interaction.ink_style(),
                source: Some(source),
            });
        if draft.validate(&geometry).is_err() {
            return None;
        }
        Some(DocumentTransaction::from_annotations([
            pulpit_core::annotate::AnnotationCommand::Create(draft),
        ]))
    }

    /// The pointer went down on the page it was last over.
    ///
    /// Returns `false` when nothing took the press — no tool armed, or a tool
    /// whose mark is placed rather than drawn — so the caller knows the press
    /// belongs to the document's own links and fields.
    pub fn pointer_pressed(&mut self) -> bool {
        let Some((page, at)) = self.cursor else {
            return false;
        };
        // The selection tool has no gesture of its own: it picks a mark up
        // and then drags it, which is a transform rather than a stroke (§8.4).
        if self.controls.tool == Some(AnnotationTool::Select) {
            return self.pick_up(page, at);
        }
        self.interaction.begin(page, at)
    }

    /// Pick up the topmost mark under `at`, and start moving it.
    ///
    /// A press on bare page puts down whatever was held: clicking away from a
    /// selection is how a selection is dismissed everywhere else.
    fn pick_up(&mut self, page: PageIndex, at: PagePoint) -> bool {
        let tolerance = self.eraser_tolerance();
        let Some(candidates) = self.annotations.get(&page) else {
            self.interaction.select(None);
            return false;
        };
        let Some(hit) = pulpit_core::annotate::hit::topmost(candidates, at, tolerance) else {
            self.interaction.select(None);
            return false;
        };
        if !hit.editable || !hit.kind.is_freely_movable() {
            // Still selected, so the reader can see what they hit and why it
            // will not move — text markup describes real text runs and cannot
            // be dragged off them (§8.4).
            self.interaction.select(Some(hit.id.clone()));
            return false;
        }
        let (id, bounds) = (hit.id.clone(), hit.bounds);
        self.interaction.select(Some(id.clone()));
        self.interaction.begin_transform(id, page, bounds);
        self.transform_origin = Some(at);
        true
    }

    /// What the open gesture wants the engine to resolve, if anything.
    ///
    /// Only the highlighter has one: it selects *text*, and only the engine
    /// knows where the text is. The query is read-only and never moves the
    /// revision (§6.3).
    pub fn pending_selection(&self) -> Option<(PageIndex, TextSelection)> {
        let (page, anchor, head) = self.interaction.pending_selection()?;
        Some((page, TextSelection::Range { anchor, head }))
    }

    /// The engine answered a selection query.
    ///
    /// Returns the transaction to commit when the answer was the one the
    /// release was waiting for — the release cannot commit on its own, because
    /// the quads it needs may still have been in flight when the pointer came
    /// up (§8.2).
    pub fn selection_resolved(
        &mut self,
        quads: Vec<pulpit_core::page::PageQuad>,
        text: String,
        finalising: bool,
    ) -> Option<DocumentTransaction> {
        self.interaction.set_selection_result(quads, text);
        if !finalising {
            return None;
        }
        self.awaiting_selection = false;
        self.finish_gesture()
    }

    /// Is a release waiting on a selection answer? While it is, the toolbar
    /// must not treat the gesture as finished.
    pub fn is_awaiting_selection(&self) -> bool {
        self.awaiting_selection
    }

    /// The pointer came up. Returns the one atomic action it produced, if any.
    ///
    /// Nothing is applied here: the transaction goes to the worker, and the
    /// mark appears when a frame carrying it arrives (A1, A7). A gesture that
    /// resolved to nothing — a selection with no text under it, an eraser
    /// sweep that touched nothing — returns `None` and is not an error.
    pub fn pointer_released(&mut self) -> Released {
        if self.interaction.gesture().is_none() {
            return Released::Nothing;
        }
        // A text selection cannot be committed from what the UI happens to
        // hold: the quads it is drawing may be from a query one movement
        // behind, and `/QuadPoints` has to describe the text that was actually
        // selected (§7.2). So the release asks once more and commits on that
        // answer.
        if let Some((page, selection)) = self.pending_selection() {
            self.awaiting_selection = true;
            return Released::AwaitingSelection { page, selection };
        }
        match self.finish_gesture() {
            Some(transaction) => Released::Commit(transaction),
            None => Released::Nothing,
        }
    }

    /// Close the open gesture and turn it into a transaction, if it made one.
    fn finish_gesture(&mut self) -> Option<DocumentTransaction> {
        // A move is a *replacement*, not a delete and a create: the mark keeps
        // its identity, which is what lets undo put back the same annotation
        // rather than a copy of it (A3, §8.4).
        if let Some(pulpit_core::annotate::Gesture::Transforming {
            id,
            page,
            original,
            current,
        }) = self.interaction.gesture().cloned()
        {
            self.interaction.cancel();
            self.transform_origin = None;
            if original == current {
                return None;
            }
            let geometry = self.pages.get(page.get()).copied()?;
            let summary = self
                .summaries
                .get(&page)?
                .iter()
                .find(|summary| summary.id == id)?;
            let moved = summary
                .to_draft()?
                .translated(current.left - original.left, current.top - original.top)?;
            // Refused here rather than by the worker: a mark dragged off the
            // sheet is a drag the reader can take back by letting go, and
            // committing it would be a mark they cannot see.
            if moved.validate(&geometry).is_err() {
                return None;
            }
            return Some(DocumentTransaction::from_annotations([
                pulpit_core::annotate::AnnotationCommand::Replace {
                    id,
                    replacement: moved,
                },
            ]));
        }

        let page = self.interaction.gesture()?.page();
        let geometry = self.pages.get(page.get()).copied()?;
        let outcome = self.interaction.finish(&geometry);
        let commands = outcome.commands();
        if commands.is_empty() {
            return None;
        }
        Some(DocumentTransaction::from_annotations(commands.to_vec()))
    }

    /// The gesture was abandoned — the pointer left the page, or Escape was
    /// pressed. Nothing is committed and nothing is reported (§8.1).
    pub fn pointer_cancelled(&mut self) {
        self.interaction.cancel();
        self.cursor = None;
        self.transform_origin = None;
    }

    /// Is a gesture open? The page surface draws its own preview while one is.
    pub fn is_drawing(&self) -> bool {
        self.interaction.is_drawing()
    }

    /// The operation that undoes the last edit, if there is one.
    pub fn undo_operation(&mut self) -> Option<pulpit_render::document::DocumentUndo> {
        self.undo_stack.pop()
    }

    /// …and the one that redoes it.
    pub fn redo_operation(&mut self) -> Option<pulpit_render::document::DocumentUndo> {
        self.redo_stack.pop()
    }

    /// A frame arrived. Ignored when it is older than one already held, so a
    /// late answer cannot repaint a page with a picture from before the last
    /// edit (A7).
    pub fn frame_arrived(
        &mut self,
        page: PageIndex,
        width: u32,
        height: u32,
        revision: DocumentRevision,
        pixels: Vec<u8>,
    ) {
        self.in_flight.remove(&page);
        if let Some(existing) = self.frames.get(&page) {
            if existing.revision > revision {
                return;
            }
        }
        self.frames.insert(
            page,
            RenderedPage {
                handle: iced::widget::image::Handle::from_rgba(width, height, pixels),
                width,
                revision,
            },
        );
    }

    /// The pages in the window that need drawing, at the size to draw them.
    ///
    /// A page is asked for when there is no frame for it, when the frame it
    /// has was drawn at a different width, or when the document has moved on
    /// since the frame was made. Nothing is asked for twice while an answer is
    /// in flight, which is what stops a tick loop from queueing one render per
    /// frame for the same page.
    pub fn renders_wanted(&mut self, scale_factor: f32) -> Vec<(PageIndex, u32, u32)> {
        if !self.open {
            return Vec::new();
        }
        let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let mut wanted = Vec::new();
        for placed in self.column.visible(self.controls.offset, self.cell.1) {
            let width = (placed.width * scale).round().max(1.0) as u32;
            let height = (placed.height * scale).round().max(1.0) as u32;
            if self.in_flight.contains(&placed.page) {
                continue;
            }
            let current = self.frames.get(&placed.page);
            let stale = match current {
                None => true,
                Some(frame) => frame.width != width || frame.revision < self.revision,
            };
            if stale {
                self.in_flight.insert(placed.page);
                wanted.push((placed.page, width, height));
            }
        }
        wanted
    }

    /// Forget a render that was asked for and will not be answered.
    pub fn render_abandoned(&mut self, page: PageIndex) {
        self.in_flight.remove(&page);
    }

    /// The page surface was drawn at this size. Recomputes the column, because
    /// a fit that ignored the cell it is fitting to is not a fit.
    pub fn set_cell(&mut self, width: f32, height: f32) {
        let cell = (width.max(0.0), height.max(0.0));
        if cell != self.cell {
            self.cell = cell;
            self.relayout();
        }
    }

    /// Recompute the scale and the column from the pages and the zoom.
    fn relayout(&mut self) {
        let reference = self
            .pages
            .get(self.controls.page.get())
            .or_else(|| self.pages.first())
            .copied()
            .unwrap_or_default();
        self.scale = self.controls.zoom.scale(&reference, self.cell);
        self.column = Column::lay_out(&self.pages, self.scale, self.cell.0);
        self.controls.offset = self.column.clamp_offset(self.controls.offset, self.cell.1);
        if let Some(page) = self.column.current(self.controls.offset, self.cell.1) {
            self.controls.page = page;
        }
    }

    /// Do what a reader widget asked for.
    ///
    /// Returns `true` when the change needs pages re-rendering — a zoom or a
    /// scroll — so the caller knows to ask the worker for frames. A tool
    /// change and an outline toggle do not.
    pub fn apply(&mut self, command: &ReadCommand) -> bool {
        match command {
            ReadCommand::ScrollTo(offset) => {
                self.controls.offset = self.column.clamp_offset(*offset, self.cell.1);
                if let Some(page) = self.column.current(self.controls.offset, self.cell.1) {
                    self.controls.page = page;
                }
                true
            }
            ReadCommand::GoToPage(page) => {
                let page = PageIndex(page.get().min(self.pages.len().saturating_sub(1)));
                if let Some(offset) = self.column.offset_of(page) {
                    self.controls.offset = self.column.clamp_offset(offset, self.cell.1);
                }
                self.controls.page = page;
                // The typed page has been honoured, so the box goes back to
                // showing where the reader actually is.
                self.page_entry = None;
                true
            }
            ReadCommand::SetZoom(zoom) => {
                self.set_zoom(*zoom);
                true
            }
            ReadCommand::ZoomIn => {
                self.set_zoom(Zoom::zoomed_in(self.scale));
                true
            }
            ReadCommand::ZoomOut => {
                self.set_zoom(Zoom::zoomed_out(self.scale));
                true
            }
            ReadCommand::TypePage(typed) => {
                // Kept as typed. Deciding what of it is a page number is the
                // commit's job, so a half-typed "1" on the way to "12" does
                // not jump the document to page one.
                self.page_entry = Some(typed.clone());
                false
            }
            ReadCommand::CommitPage => {
                let Some(entry) = self.page_entry.take() else {
                    return false;
                };
                // "12 / 40" is what the box shows when nobody is typing in it,
                // so the leading number is what a reader would have edited.
                let number = entry
                    .split(['/', ' '])
                    .find(|part| !part.trim().is_empty())
                    .and_then(|part| part.trim().parse::<usize>().ok());
                match number {
                    Some(page) if page >= 1 && page <= self.pages.len() => {
                        self.apply(&ReadCommand::GoToPage(PageIndex(page - 1)))
                    }
                    // Nonsense goes back to showing where the reader is,
                    // rather than to page one or to an error.
                    _ => false,
                }
            }
            ReadCommand::SetOutlineView(view) => {
                self.controls.outline = *view;
                false
            }
            ReadCommand::Arm(tool) => {
                self.controls.tool = *tool;
                // The toolbar and the gesture state are one choice, not two:
                // arming through the interaction is also what abandons any
                // open gesture, so changing tools mid-stroke drops the stroke
                // rather than finishing it with the new tool.
                self.interaction.arm(*tool);
                false
            }
            // The rest are the application's to route to the worker: a page
            // gesture, a field edit, an undo or a save is not a viewport
            // change, and this type owns only the viewport.
            ReadCommand::PageCursor { .. }
            | ReadCommand::PagePressed
            | ReadCommand::PageReleased
            | ReadCommand::PageCancelled
            | ReadCommand::Undo
            | ReadCommand::Redo
            | ReadCommand::SaveAs => false,
        }
    }

    fn set_zoom(&mut self, zoom: Zoom) {
        // Zooming keeps the reader where they were reading rather than
        // returning them to the top: the page under the middle of the window
        // stays under the middle of the window.
        let anchor = self.controls.page;
        self.controls.zoom = zoom;
        self.relayout();
        if let Some(offset) = self.column.offset_of(anchor) {
            self.controls.offset = self.column.clamp_offset(offset, self.cell.1);
            self.controls.page = anchor;
        }
    }

    /// The facet the reader's widgets are drawn from.
    ///
    /// `live` is false in the editor and in a preview, where the controls are
    /// drawn inert; the document behind them is the same one either way, so
    /// this is the only place that has to know the difference.
    pub fn facet(&self, live: bool) -> ReaderData<'_> {
        let visible = if live && self.open {
            self.column
                .visible(self.controls.offset, self.cell.1)
                .into_iter()
                .map(|placed| ReaderPage {
                    // The open gesture, drawn by the UI so the stroke follows
                    // the hand rather than the round trip (A2). Only ever on
                    // the page the gesture is on.
                    preview: self.preview_for(placed.page),
                    canonical: self
                        .pages
                        .get(placed.page.get())
                        .map(|page| (page.width, page.height))
                        .unwrap_or((1.0, 1.0)),
                    // Whatever frame this page has, even one drawn at another
                    // width or before the last edit: it is replaced when a
                    // newer one arrives (A7). Until the first one does, the
                    // sheet is drawn blank at its full size, so the column
                    // does not move under the reader when it lands.
                    frame: self
                        .frames
                        .get(&placed.page)
                        .map(|frame| frame.handle.clone()),
                    placed,
                })
                .collect()
        } else {
            Vec::new()
        };
        ReaderData {
            open: self.open && live,
            page_count: self.pages.len(),
            column: &self.column,
            visible,
            controls: &self.controls,
            scale: self.scale,
            outline: &self.outline,
            fields: &self.fields,
            has_form: self.has_form,
            level: self.level,
            warnings: &self.warnings,
            dirty: self.dirty,
            page_entry: self.page_entry.clone(),
            can_undo: self.undo_depth > 0,
            can_redo: self.redo_depth > 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `Applied`, for the tests that only care what the session
    /// does with one.
    fn applied(revision: DocumentRevision) -> pulpit_render::document::Applied {
        pulpit_render::document::Applied {
            effects: Vec::new(),
            document_revision: revision,
            dirty_region: None,
            dirty_pages: vec![PageIndex(0)],
            undo: pulpit_render::document::DocumentUndo {
                operations: Vec::new(),
                restores: DocumentRevision(revision.0.saturating_sub(1)),
                label: "Add Ink".into(),
            },
        }
    }

    fn open(pages: usize) -> ReaderSession {
        let mut session = ReaderSession::new();
        session.opened(
            vec![PageGeometry::upright(612.0, 792.0); pages],
            CompatibilityLevel::Native,
            Vec::new(),
        );
        session.set_cell(612.0, 400.0);
        session
    }

    #[test]
    fn a_closed_reader_has_nothing_open_and_says_so() {
        let session = ReaderSession::new();
        assert!(!session.is_open());
        let facet = session.facet(true);
        assert!(!facet.open);
        assert_eq!(facet.counter(), "—");
        assert!(!facet.annotatable());
    }

    #[test]
    fn opening_a_document_fits_its_width_at_the_first_page() {
        let session = open(5);
        assert!(session.is_open());
        assert_eq!(session.controls().zoom, Zoom::FitWidth);
        assert_eq!(session.controls().page, PageIndex(0));
        // 612 points of page into 612 points of cell.
        assert!((session.scale - 1.0).abs() < 1e-4, "{}", session.scale);
        assert_eq!(session.facet(true).counter(), "1 / 5");
    }

    #[test]
    fn opening_a_second_document_forgets_the_first() {
        let mut session = open(5);
        session.apply(&ReadCommand::GoToPage(PageIndex(4)));
        session.applied(&applied(DocumentRevision(3)), AppliedKind::Edit);
        session.opened(
            vec![PageGeometry::upright(612.0, 792.0); 2],
            CompatibilityLevel::AnnotateOnly,
            Vec::new(),
        );
        assert_eq!(session.controls().page, PageIndex(0));
        assert_eq!(session.revision(), DocumentRevision::INITIAL);
        assert!(!session.is_dirty(), "a fresh document is not dirty");
    }

    #[test]
    fn going_to_a_page_puts_its_top_at_the_top() {
        let mut session = open(10);
        assert!(session.apply(&ReadCommand::GoToPage(PageIndex(3))));
        assert_eq!(session.controls().page, PageIndex(3));
        assert_eq!(
            session.controls().offset,
            session.column.offset_of(PageIndex(3)).unwrap()
        );
    }

    #[test]
    fn a_page_beyond_the_document_lands_on_the_last_one() {
        let mut session = open(4);
        session.apply(&ReadCommand::GoToPage(PageIndex(99)));
        assert_eq!(session.controls().page, PageIndex(3));
    }

    #[test]
    fn typing_in_the_page_box_moves_nothing_until_it_is_committed() {
        let mut session = open(40);
        assert!(!session.apply(&ReadCommand::TypePage("1".into())));
        assert_eq!(
            session.controls().page,
            PageIndex(0),
            "a half-typed number must not jump the document"
        );
        session.apply(&ReadCommand::TypePage("12".into()));
        assert!(session.apply(&ReadCommand::CommitPage));
        assert_eq!(session.controls().page, PageIndex(11));
        assert!(
            session.facet(true).page_entry.is_none(),
            "the box goes back to showing where the reader is"
        );
    }

    #[test]
    fn a_page_number_that_is_not_one_is_refused_without_moving_anything() {
        let mut session = open(10);
        session.apply(&ReadCommand::GoToPage(PageIndex(5)));
        for nonsense in ["", "  ", "abc", "0", "99", "-3"] {
            session.apply(&ReadCommand::TypePage(nonsense.into()));
            session.apply(&ReadCommand::CommitPage);
            assert_eq!(
                session.controls().page,
                PageIndex(5),
                "{nonsense:?} moved the document"
            );
        }
    }

    #[test]
    fn the_page_box_is_edited_from_what_it_was_showing() {
        // The box shows "12 / 40"; a reader who edits the first number and
        // presses return means that page, not a parse failure.
        let mut session = open(40);
        session.apply(&ReadCommand::TypePage("7 / 40".into()));
        session.apply(&ReadCommand::CommitPage);
        assert_eq!(session.controls().page, PageIndex(6));
    }

    #[test]
    fn zooming_steps_the_ladder_and_keeps_the_reader_where_they_were() {
        let mut session = open(10);
        session.apply(&ReadCommand::GoToPage(PageIndex(4)));
        let before = session.controls().page;
        assert!(session.apply(&ReadCommand::ZoomIn));
        assert_eq!(session.controls().zoom, Zoom::Fixed(1.25));
        assert_eq!(
            session.controls().page,
            before,
            "zooming must not send the reader back to the top"
        );
        session.apply(&ReadCommand::ZoomOut);
        assert_eq!(session.controls().zoom, Zoom::Fixed(1.0));
    }

    #[test]
    fn a_fit_is_recomputed_when_the_cell_changes_size() {
        let mut session = open(3);
        let before = session.scale;
        session.set_cell(306.0, 400.0);
        assert!(session.scale < before, "a narrower cell fits smaller");
        assert!((session.scale - 0.5).abs() < 1e-4);
        // A fixed zoom is the reader's choice and does not follow the window.
        session.apply(&ReadCommand::SetZoom(Zoom::Fixed(2.0)));
        session.set_cell(1_224.0, 400.0);
        assert_eq!(session.scale, 2.0);
    }

    #[test]
    fn scrolling_stays_inside_the_document_and_moves_the_counter() {
        let mut session = open(3);
        session.apply(&ReadCommand::ScrollTo(-1_000.0));
        assert_eq!(session.controls().offset, 0.0);
        session.apply(&ReadCommand::ScrollTo(1e9));
        assert_eq!(
            session.controls().page,
            PageIndex(2),
            "scrolling to the end is being on the last page"
        );
    }

    #[test]
    fn arming_a_tool_and_toggling_the_outline_need_no_re_render() {
        let mut session = open(2);
        assert!(!session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink))));
        assert_eq!(session.tool(), Some(AnnotationTool::Ink));
        assert!(!session.apply(&ReadCommand::Arm(None)));
        assert!(session.tool().is_none());

        use crate::widgets::document::model::OutlineView;
        assert!(!session.apply(&ReadCommand::SetOutlineView(OutlineView::Thumbnails)));
        assert_eq!(session.controls().outline, OutlineView::Thumbnails);
    }

    #[test]
    fn the_commands_the_worker_owns_are_not_viewport_changes() {
        let mut session = open(2);
        for command in [
            ReadCommand::PagePressed,
            ReadCommand::PageReleased,
            ReadCommand::PageCancelled,
            ReadCommand::Undo,
            ReadCommand::Redo,
            ReadCommand::SaveAs,
        ] {
            assert!(!session.apply(&command), "{command:?} is not a re-render");
        }
    }

    #[test]
    fn only_the_pages_in_the_window_are_handed_to_the_widgets() {
        let mut session = open(500);
        session.apply(&ReadCommand::GoToPage(PageIndex(250)));
        let facet = session.facet(true);
        assert!(
            facet.visible.len() <= 3,
            "a 500-page document put {} pages in the window",
            facet.visible.len()
        );
        assert_eq!(facet.visible[0].placed.page, PageIndex(250));
        assert_eq!(facet.page_count, 500);
    }

    #[test]
    fn a_preview_draws_the_controls_inert_and_the_page_empty() {
        let session = open(3);
        let facet = session.facet(false);
        assert!(!facet.open);
        assert!(facet.visible.is_empty());
    }

    #[test]
    fn the_history_controls_follow_what_the_worker_reported() {
        let mut session = open(2);
        let facet = session.facet(true);
        assert!(!facet.can_undo && !facet.can_redo);
        session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        let facet = session.facet(true);
        assert!(facet.can_undo && !facet.can_redo);
        assert!(facet.dirty);
        assert_eq!(session.revision(), DocumentRevision(1));
    }

    #[test]
    fn only_the_visible_pages_are_asked_for_and_only_once() {
        let mut session = open(20);
        let wanted = session.renders_wanted(1.0);
        assert!(!wanted.is_empty());
        assert!(
            wanted.len() <= 3,
            "a twenty-page document asked for {} renders",
            wanted.len()
        );
        assert_eq!(wanted[0].0, PageIndex(0));
        // The width asked for is the width the column placed the page at.
        assert_eq!(wanted[0].1, 612);

        // Asking again while the first answers are in flight asks for nothing:
        // a tick loop must not queue one render per frame for one page.
        assert!(session.renders_wanted(1.0).is_empty());
    }

    #[test]
    fn a_page_is_asked_for_again_when_it_is_drawn_at_a_different_size() {
        let mut session = open(3);
        let wanted = session.renders_wanted(1.0);
        let (page, width, height) = wanted[0];
        session.frame_arrived(page, width, height, DocumentRevision::INITIAL, vec![0; 4]);
        assert!(session.renders_wanted(1.0).is_empty(), "nothing changed");

        // A wider cell is a different picture, not the same one stretched.
        session.set_cell(1_224.0, 400.0);
        let again = session.renders_wanted(1.0);
        assert!(again.iter().any(|(p, _, _)| *p == page));
        assert!(again[0].1 > width);
    }

    #[test]
    fn a_frame_older_than_one_already_held_is_not_painted_over_it() {
        // A7, at the point a late answer arrives: a frame from before the last
        // edit must not replace the one that contains it.
        let mut session = open(2);
        let (page, width, height) = session.renders_wanted(1.0)[0];
        session.frame_arrived(page, width, height, DocumentRevision(3), vec![1; 4]);
        session.frame_arrived(page, width, height, DocumentRevision(1), vec![2; 4]);
        let facet = session.facet(true);
        let drawn = facet
            .visible
            .iter()
            .find(|candidate| candidate.placed.page == page)
            .unwrap();
        assert!(drawn.frame.is_some());
        // The newer frame is still the one held: asking again wants nothing,
        // because the held revision is not behind the document's.
        assert!(session.renders_wanted(1.0).is_empty());
    }

    #[test]
    fn an_edit_makes_the_pages_it_touched_stale() {
        let mut session = open(3);
        let (page, width, height) = session.renders_wanted(1.0)[0];
        session.frame_arrived(page, width, height, DocumentRevision::INITIAL, vec![0; 4]);
        assert!(session.renders_wanted(1.0).is_empty());

        session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        let wanted = session.renders_wanted(1.0);
        assert!(
            wanted.iter().any(|(p, _, _)| *p == page),
            "the edited page was not re-rendered"
        );
    }

    #[test]
    fn undo_and_redo_move_operations_between_the_two_stacks() {
        let mut session = open(2);
        session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        assert!(session.facet(true).can_undo);
        assert!(!session.facet(true).can_redo);

        let operation = session.undo_operation().expect("something to undo");
        session.applied(&applied(DocumentRevision(2)), AppliedKind::Undo);
        assert!(!session.facet(true).can_undo);
        assert!(session.facet(true).can_redo);
        let _ = operation;

        session.redo_operation().expect("something to redo");
        session.applied(&applied(DocumentRevision(3)), AppliedKind::Redo);
        assert!(session.facet(true).can_undo);
        assert!(!session.facet(true).can_redo);
    }

    #[test]
    fn a_new_edit_makes_the_taken_back_future_unreachable() {
        // Standard undo semantics, stated because getting it wrong leaves a
        // redo control that puts back an edit from a history the user left.
        let mut session = open(2);
        session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        session.undo_operation();
        session.applied(&applied(DocumentRevision(2)), AppliedKind::Undo);
        assert!(session.facet(true).can_redo);

        session.applied(&applied(DocumentRevision(3)), AppliedKind::Edit);
        assert!(!session.facet(true).can_redo);
        assert!(session.facet(true).can_undo);
    }

    #[test]
    fn a_stroke_on_a_page_becomes_one_transaction() {
        let mut session = open(3);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(1), 100.0, 100.0);
        assert!(session.pointer_pressed());
        assert!(session.is_drawing());
        for step in 1..20 {
            session.pointer_moved(PageIndex(1), 100.0 + step as f32 * 6.0, 100.0 + step as f32);
        }
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("a stroke commits")
        };
        assert_eq!(transaction.len(), 1, "one gesture is one undo entry");
        assert!(!session.is_drawing());
        assert_eq!(transaction.label(), "Add Ink");
    }

    #[test]
    fn a_press_with_nothing_armed_belongs_to_the_document() {
        let mut session = open(2);
        session.pointer_moved(PageIndex(0), 40.0, 40.0);
        assert!(
            !session.pointer_pressed(),
            "an unarmed press must reach the page's own links and fields"
        );
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_gesture_the_pointer_left_commits_nothing() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 50.0, 50.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 150.0, 90.0);
        session.pointer_cancelled();
        assert!(!session.is_drawing());
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn changing_tool_mid_stroke_drops_the_stroke() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 50.0, 50.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 150.0, 90.0);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Eraser)));
        assert!(!session.is_drawing());
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_stroke_off_the_page_commits_nothing() {
        // The gesture validates against the page it was drawn on before it is
        // sent, so a mark that cannot be written is refused here rather than
        // by the worker (§8.1).
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 90_000.0, 40.0);
        session.pointer_pressed();
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_highlight_waits_for_the_engine_to_say_where_the_text_is() {
        // §8.2: the release cannot commit from what the UI happens to hold —
        // those quads may be one query behind — so it asks once more and the
        // answer is what commits.
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Highlighter)));
        session.pointer_moved(PageIndex(0), 72.0, 100.0);
        assert!(session.pointer_pressed());
        session.pointer_moved(PageIndex(0), 300.0, 100.0);

        // While the drag is open the engine is asked where the text is.
        let (page, _) = session.pending_selection().expect("a selection is open");
        assert_eq!(page, PageIndex(0));

        let Released::AwaitingSelection { .. } = session.pointer_released() else {
            panic!("a highlight release waits for the engine")
        };
        assert!(session.is_awaiting_selection());

        let quads = vec![pulpit_core::page::PageQuad::from_rect(
            pulpit_core::page::PageRect::new(72.0, 92.0, 300.0, 108.0),
        )];
        let transaction = session
            .selection_resolved(quads, "the marked words".into(), true)
            .expect("the resolved selection commits");
        assert_eq!(transaction.len(), 1);
        assert_eq!(transaction.label(), "Add Highlight");
        assert!(!session.is_awaiting_selection());
    }

    #[test]
    fn a_selection_with_no_text_under_it_commits_nothing() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Highlighter)));
        session.pointer_moved(PageIndex(0), 72.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 300.0, 100.0);
        let Released::AwaitingSelection { .. } = session.pointer_released() else {
            panic!("a highlight release waits")
        };
        assert!(session
            .selection_resolved(Vec::new(), String::new(), true)
            .is_none());
        assert!(!session.is_awaiting_selection());
        assert!(!session.is_drawing());
    }

    #[test]
    fn a_selection_answer_mid_drag_only_updates_what_is_drawn() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Highlighter)));
        session.pointer_moved(PageIndex(0), 72.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 100.0);
        let quads = vec![pulpit_core::page::PageQuad::from_rect(
            pulpit_core::page::PageRect::new(72.0, 92.0, 200.0, 108.0),
        )];
        assert!(
            session
                .selection_resolved(quads, "some".into(), false)
                .is_none(),
            "a query that is not the release's must not commit"
        );
        assert!(session.is_drawing(), "the drag is still open");
    }

    #[test]
    fn only_the_highlighter_asks_the_engine_where_the_text_is() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 72.0, 100.0);
        session.pointer_pressed();
        assert!(
            session.pending_selection().is_none(),
            "ink selects no text and must not query for it"
        );
    }

    /// A summary of one ink stroke on page zero, for the eraser tests.
    fn stroke_at(y: f32) -> pulpit_render::document::AnnotationSummary {
        use pulpit_core::page::{PagePoint, PageRect};
        pulpit_render::document::AnnotationSummary {
            id: pulpit_core::annotate::IdGenerator::new(y as u64).next_id(),
            page: PageIndex(0),
            kind: pulpit_core::annotate::AnnotationKind::Ink,
            bounds: PageRect::new(50.0, y - 2.0, 400.0, y + 2.0),
            style: pulpit_core::annotate::MarkStyle::default(),
            contents: Default::default(),
            support: pulpit_render::document::AnnotationSupport::Editable,
            revision: DocumentRevision::INITIAL,
            path: vec![PagePoint::new(50.0, y), PagePoint::new(400.0, y)],
            quads: Vec::new(),
            geometry_elided: false,
        }
    }

    #[test]
    fn an_eraser_sweep_takes_what_it_passes_over_and_commits_once() {
        let mut session = open(2);
        let marks = [stroke_at(100.0), stroke_at(300.0)];
        session.set_annotations(PageIndex(0), &marks);

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Eraser)));
        session.pointer_moved(PageIndex(0), 200.0, 100.0);
        assert!(session.pointer_pressed());
        // Across the first stroke, then across the second.
        session.pointer_moved(PageIndex(0), 210.0, 300.0);

        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("a sweep that took marks commits")
        };
        assert_eq!(
            transaction.len(),
            2,
            "one sweep, one transaction, both marks"
        );
        assert_eq!(transaction.label(), "Erase");
    }

    #[test]
    fn an_eraser_sweep_over_empty_page_commits_nothing() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(100.0)]);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Eraser)));
        session.pointer_moved(PageIndex(0), 200.0, 500.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 260.0, 520.0);
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_fast_sweep_does_not_jump_over_a_mark() {
        // The samples are far apart; only testing the segment between them
        // finds the stroke that lies across it (§8.3).
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(200.0)]);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Eraser)));
        session.pointer_moved(PageIndex(0), 200.0, 60.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 400.0);
        assert!(matches!(session.pointer_released(), Released::Commit(_)));
    }

    #[test]
    fn editing_a_page_drops_what_was_known_about_it() {
        // A stale hit-test list would let a second sweep try to erase a mark
        // that is already gone.
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(100.0)]);
        assert!(session.annotations_wanted().is_empty());
        session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        assert!(
            session.annotations_wanted().contains(&PageIndex(0)),
            "the edited page's marks were not re-read"
        );
    }

    #[test]
    fn an_open_stroke_is_previewed_on_its_own_page_and_nowhere_else() {
        // A2: the UI draws the unfinished stroke so it follows the hand
        // rather than the round trip that rasterises it.
        let mut session = open(3);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);

        let facet = session.facet(true);
        let drawn = facet.visible.iter().filter(|page| page.preview.is_some());
        assert_eq!(drawn.count(), 1, "the preview is on exactly one page");
        let on = facet
            .visible
            .iter()
            .find(|page| page.preview.is_some())
            .unwrap();
        assert_eq!(on.placed.page, PageIndex(0));
        assert!(on.preview.as_ref().unwrap().points.len() >= 2);
    }

    #[test]
    fn the_preview_is_gone_the_moment_the_gesture_is() {
        // Nothing here may outlive the commit, or it would be a second copy
        // of the mark (A1).
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);
        assert!(session
            .facet(true)
            .visible
            .iter()
            .any(|page| page.preview.is_some()));

        let Released::Commit(_) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        assert!(
            session
                .facet(true)
                .visible
                .iter()
                .all(|page| page.preview.is_none()),
            "the preview outlived the gesture"
        );
    }

    #[test]
    fn an_eraser_sweep_previews_nothing() {
        // It shows its effect by taking marks away; a line drawn along the
        // sweep would read as a mark being made.
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(100.0)]);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Eraser)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 100.0);
        assert!(session
            .facet(true)
            .visible
            .iter()
            .all(|page| page.preview.is_none()));
    }

    #[test]
    fn a_selection_previews_the_runs_the_engine_resolved() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Highlighter)));
        session.pointer_moved(PageIndex(0), 72.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 300.0, 100.0);
        // Nothing resolved yet: nothing to show.
        assert!(session
            .facet(true)
            .visible
            .iter()
            .all(|page| page.preview.is_none()));

        session.selection_resolved(
            vec![pulpit_core::page::PageQuad::from_rect(
                pulpit_core::page::PageRect::new(72.0, 92.0, 300.0, 108.0),
            )],
            "words".into(),
            false,
        );
        let facet = session.facet(true);
        let preview = facet
            .visible
            .iter()
            .find_map(|page| page.preview.as_ref())
            .expect("the resolved runs are drawn");
        assert_eq!(preview.quads.len(), 1);
    }

    #[test]
    fn a_placed_mark_is_a_spot_first_and_text_afterwards() {
        // §8.5: free text and notes have no gesture. The click chooses a
        // spot; what happens next is a text editor.
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Note)));
        session.pointer_moved(PageIndex(0), 120.0, 240.0);
        assert!(
            !session.pointer_pressed(),
            "a placing tool starts no gesture"
        );
        let (page, at, tool) = session.placement().expect("a note has somewhere to go");
        assert_eq!(page, PageIndex(0));
        assert_eq!(tool, AnnotationTool::Note);

        let transaction = session
            .place_text(page, at, tool, "remember this".into())
            .expect("a note with text in it commits");
        assert_eq!(transaction.len(), 1);
        assert_eq!(transaction.label(), "Add Note");
    }

    #[test]
    fn an_empty_note_is_not_a_note() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Note)));
        session.pointer_moved(PageIndex(0), 120.0, 240.0);
        let (page, at, tool) = session.placement().unwrap();
        assert!(session.place_text(page, at, tool, String::new()).is_none());
        assert!(session
            .place_text(page, at, tool, "   \n ".into())
            .is_none());
    }

    #[test]
    fn only_a_placing_tool_has_somewhere_to_place() {
        let mut session = open(2);
        session.pointer_moved(PageIndex(0), 120.0, 240.0);
        assert!(
            session.placement().is_none(),
            "nothing armed places nothing"
        );

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert!(session.placement().is_none(), "ink is drawn, not placed");

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Text)));
        assert!(session.placement().is_some());
    }

    #[test]
    fn a_mark_placed_off_the_page_commits_nothing() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Note)));
        session.pointer_moved(PageIndex(0), 99_000.0, 40.0);
        let (page, at, tool) = session.placement().unwrap();
        assert!(session.place_text(page, at, tool, "hello".into()).is_none());
    }

    #[test]
    fn a_selected_mark_moves_as_one_replacement_that_keeps_its_identity() {
        // §8.4 and A3: a move is a replacement, not a delete and a create, so
        // undo puts back the same annotation rather than a copy of it.
        let mut session = open(2);
        let mark = stroke_at(200.0);
        let id = mark.id.clone();
        session.set_annotations(PageIndex(0), &[mark]);

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Select)));
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        assert!(session.pointer_pressed(), "the mark was picked up");
        assert_eq!(session.selected(), Some(&id));

        session.pointer_moved(PageIndex(0), 260.0, 240.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("a move commits")
        };
        assert_eq!(transaction.len(), 1);
        assert_eq!(transaction.label(), "Edit Ink");
    }

    #[test]
    fn a_drag_that_returned_to_where_it_started_is_not_an_edit() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(200.0)]);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Select)));
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 300.0, 260.0);
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn pressing_bare_page_puts_down_whatever_was_held() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(200.0)]);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Select)));
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        session.pointer_pressed();
        assert!(session.selected().is_some());

        session.pointer_released();
        session.pointer_moved(PageIndex(0), 200.0, 600.0);
        assert!(!session.pointer_pressed(), "there is nothing there");
        assert!(session.selected().is_none());
    }

    #[test]
    fn text_markup_can_be_selected_but_not_dragged_off_its_text() {
        // §8.4: /QuadPoints describe real text runs, and a highlight dragged
        // elsewhere would describe text no longer under it.
        let mut session = open(2);
        let mut highlight = stroke_at(200.0);
        highlight.kind = pulpit_core::annotate::AnnotationKind::Highlight;
        highlight.path = Vec::new();
        highlight.quads = vec![pulpit_core::page::PageQuad::from_rect(
            pulpit_core::page::PageRect::new(50.0, 192.0, 400.0, 208.0),
        )];
        let id = highlight.id.clone();
        session.set_annotations(PageIndex(0), &[highlight]);

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Select)));
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        assert!(!session.pointer_pressed(), "it must not start a move");
        assert_eq!(
            session.selected(),
            Some(&id),
            "it is still selected, so the reader can see what they hit"
        );
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_mark_dragged_off_the_sheet_is_not_committed() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(200.0)]);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Select)));
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 99_000.0, 200.0);
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_closed_reader_asks_for_nothing() {
        let mut session = ReaderSession::new();
        assert!(session.renders_wanted(1.0).is_empty());
    }

    #[test]
    fn a_render_that_will_never_be_answered_is_forgotten_rather_than_leaked() {
        let mut session = open(3);
        let (page, _, _) = session.renders_wanted(1.0)[0];
        assert!(session.renders_wanted(1.0).is_empty());
        session.render_abandoned(page);
        assert!(
            session
                .renders_wanted(1.0)
                .iter()
                .any(|(p, _, _)| *p == page),
            "an abandoned render left the page unaskable"
        );
    }

    #[test]
    fn a_document_that_cannot_be_annotated_says_so_to_the_toolbar() {
        let mut session = ReaderSession::new();
        session.opened(
            vec![PageGeometry::upright(612.0, 792.0)],
            CompatibilityLevel::Unsupported,
            vec![DocumentWarning::Encrypted],
        );
        assert!(!session.facet(true).annotatable());
    }
}
