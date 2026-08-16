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
    CompatibilityLevel, DocumentRevision, DocumentTransaction, DocumentWarning, TextSelection,
};

use crate::widgets::context::{OutlineRow, ReaderData, ReaderPage};
use crate::widgets::document::model::{Column, PageSpread, PlacedPage, ReaderControls, Zoom};
use crate::widgets::event::ReadCommand;

/// A text mark that can be reopened for editing (§8.5).
#[derive(Debug, Clone, PartialEq)]
pub struct EditableText {
    pub id: pulpit_core::annotate::AnnotationId,
    pub page: PageIndex,
    /// The mark's top-left corner: where it is, and where it stays.
    pub at: PagePoint,
    pub tool: AnnotationTool,
    /// What the editor opens: the markup for a generated mark, the text
    /// itself for a plain one.
    pub text: String,
    pub typst: bool,
}

/// One committed mark, kept drawn by the UI while the round trip that puts it
/// into a rendered frame runs.
///
/// This is deliberately *not* a second copy of the mark (A1): it holds only
/// what the preview painter needs, it is stamped with the revision its commit
/// produced, and it is dropped the moment a frame rendered at or beyond that
/// revision lands on its page.
#[derive(Debug, Clone, PartialEq)]
struct RetainedMark {
    page: PageIndex,
    preview: crate::widgets::document::preview::GesturePreview,
    /// The revision the commit produced — `None` between sending and the
    /// worker's answer. Commits are answered in order, so the oldest unstamped
    /// mark is the one each answer names.
    revision: Option<pulpit_render::document::DocumentRevision>,
    /// The name the worker gave it, once it has answered.
    ///
    /// Identity is what lets a later delete or move in the same breath find
    /// this preview again and take it down or move it, instead of leaving a
    /// mark drawn that the document no longer contains.
    id: Option<pulpit_core::annotate::AnnotationId>,
}

/// Whether a committed transaction leaves the rendered picture complete.
///
/// The picture the reader shows is a rendered frame plus the previews stacked
/// on it (§9.2). When every part of a transaction is drawn by a preview, that
/// combination is not a stale picture waiting for a real one — it is right,
/// and re-rendering would produce the same thing at the cost of a full
/// document snapshot, a reopen and a cold rasterisation of every visible page.
/// Ordered so that combining the urgencies of several commands is `max`:
/// one part of a transaction the previews cannot cover makes the whole of it
/// prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum RasterUrgency {
    /// Nothing on screen is wrong. The renderer can catch up when the hand
    /// stops.
    #[default]
    Deferred,
    /// Something on screen is wrong until it is re-rendered: a mark no preview
    /// can draw, or a change to something already in the picture.
    Prompt,
}

/// What the preview painter would draw for a draft, if it can draw it at all.
///
/// The painter takes polylines and quads (§9.2), which is ink and highlights.
/// A stamp's picture and typeset text are not among them, and drawing an
/// approximation of them would be exactly the second appearance the retained
/// preview exists to avoid.
fn preview_of(
    draft: &pulpit_core::annotate::AnnotationDraft,
) -> Option<(PageIndex, crate::widgets::document::preview::GesturePreview)> {
    use pulpit_core::annotate::AnnotationDraft;

    let (page, points, quads, style) = match draft {
        AnnotationDraft::Ink(ink) => (
            ink.page,
            ink.points.iter().map(|point| point.at).collect(),
            Vec::new(),
            ink.style,
        ),
        AnnotationDraft::Highlight(highlight) => (
            highlight.page,
            Vec::new(),
            highlight.quads.clone(),
            highlight.style,
        ),
        _ => return None,
    };
    let preview = crate::widgets::document::preview::GesturePreview {
        points,
        quads,
        color: style.color.rgb(),
        opacity: style.opacity,
        width: style.width,
    };
    (!preview.is_empty()).then_some((page, preview))
}

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
    /// Has the surface ever reported its size? Until it has, `cell` is a
    /// placeholder, and a page rendered to fit a placeholder is a page
    /// rendered at the wrong size — an expensive answer that is thrown away
    /// the moment the real cell arrives.
    cell_known: bool,
    scale: f32,
    outline: Vec<OutlineRow>,
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
    /// Annotation lists asked for and not yet answered, so a page whose
    /// answer is in flight is not asked for again on every tick. Before this
    /// set existed a fast scroll posted one duplicate `ListAnnotations` per
    /// tick per unanswered page, and on a serial worker every one of them
    /// queued ahead of the page renders the reader was waiting on.
    annotation_requests: HashSet<PageIndex>,
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
    /// Committed marks the UI keeps drawing until a frame that contains them
    /// arrives (§9.2). Without this the stroke follows the hand, vanishes at
    /// release, and only reappears after the snapshot round trip — the gap
    /// that made the tools feel like they "eventually work".
    retained: Vec<RetainedMark>,
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
    /// Has the surface said how tall its window is? Until it has, the
    /// layout's estimate stands in.
    viewport_reported: bool,
    /// The offset the last batch of renders was asked for at, which is how
    /// "the reader is scrolling" is told from "the reader has stopped".
    last_render_offset: f32,
    /// The point in the column the hand grabbed, in layout points from the
    /// document's top left corner.
    ///
    /// An anchor rather than a running delta: the pan is "keep the spot that
    /// was under the hand under the hand", so every move is measured against
    /// where the grab started and a scroll that clamped at the end of the
    /// document does not accumulate an error the reader has to unwind. Both
    /// axes, because a page zoomed past the width of the window is read
    /// sideways as much as down (§8.1).
    pan: Option<(f32, f32)>,
}

/// One render the plan wants, in physical pixels.
///
/// The session says what to draw and at what size; the application owns the
/// cache and the in-flight set, so nothing here remembers what has already
/// been asked for. The same entry can appear tick after tick and cost
/// nothing: a satisfied request is dropped against the cache before it is
/// submitted, exactly as the slide plan's are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedRender {
    pub page: PageIndex,
    pub width: u32,
    pub height: u32,
    pub quality: pulpit_render::protocol::Quality,
    /// On screen right now, as opposed to warming in the margin.
    pub visible: bool,
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
            cell_known: false,
            ..ReaderSession::default()
        }
    }

    /// How many pages the open document has, zero when nothing is open.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn revision(&self) -> DocumentRevision {
        self.revision
    }

    /// The canonical geometry of one page, if the engine has described the
    /// document and that page exists.
    ///
    /// Presentation needs this as much as document mode does: a mark made on a
    /// slide is placed on a *page*, and the page's size and rotation are what
    /// turn one into the other (A4).
    pub fn page_geometry(&self, page: PageIndex) -> Option<&PageGeometry> {
        self.pages.get(page.0)
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

    pub fn set_outline(&mut self, outline: Vec<OutlineRow>) {
        self.outline = outline;
    }

    /// The worker applied something.
    ///
    /// `redoing` says which stack the operation it handed back belongs on: an
    /// ordinary edit pushes onto undo and clears the redo stack, because the
    /// future the user had taken back is no longer reachable from where they
    /// now are; an undo pushes onto redo, and a redo back onto undo.
    #[must_use]
    pub fn applied(
        &mut self,
        applied: &pulpit_render::document::Applied,
        kind: AppliedKind,
    ) -> RasterUrgency {
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

        // Every page the transaction touched is out of date — the frames the
        // application holds for it keep showing until ones rendered from a
        // newer snapshot arrive (A7), so nothing is dropped here. What *is*
        // dropped is what was on the page, which is what the eraser hit-tests
        // against: keeping a stale list would let a second sweep try to erase
        // a mark that is already gone.
        for page in &applied.dirty_pages {
            self.annotations.remove(page);
            self.summaries.remove(page);
            self.annotation_requests.remove(page);
        }

        // Give the previews the names and the revision the worker just made
        // for them. Matched by name where there is one — a replace keeps the
        // mark's identity — and otherwise by order, because a create has no
        // name until this answer and commits are answered in order.
        if kind == AppliedKind::Edit {
            for effect in &applied.effects {
                let pulpit_render::document::AppliedEffect::Annotation(summary) = effect else {
                    continue;
                };
                let named = self
                    .retained
                    .iter()
                    .position(|mark| mark.id.as_ref() == Some(&summary.id));
                let at = named.or_else(|| {
                    self.retained
                        .iter()
                        .position(|mark| mark.id.is_none() && mark.revision.is_none())
                });
                if let Some(mark) = at.and_then(|at| self.retained.get_mut(at)) {
                    mark.id = Some(summary.id.clone());
                }
            }
            // Every preview still waiting belongs to this answer: commits are
            // answered in order, so anything older is already stamped. The
            // stamp does not depend on the effects — a preview that never got
            // one would never come down.
            for mark in self
                .retained
                .iter_mut()
                .filter(|mark| mark.revision.is_none())
            {
                mark.revision = Some(applied.document_revision);
            }
            return RasterUrgency::Deferred;
        }

        // An undo or a redo. Which of the two cases it is only becomes clear
        // from what it did: taking back a mark that is still only a preview
        // is dropping the preview, and costs no render. Taking back anything
        // the renderer has already drawn, or putting something back, does.
        let mut urgency = RasterUrgency::Deferred;
        for effect in &applied.effects {
            match effect {
                pulpit_render::document::AppliedEffect::Deleted(id) => {
                    match self
                        .retained
                        .iter()
                        .position(|mark| mark.id.as_ref() == Some(id))
                    {
                        Some(at) => {
                            self.retained.remove(at);
                        }
                        None => urgency = RasterUrgency::Prompt,
                    }
                }
                // Something was restored or changed back. The before-image is
                // the worker's, not the preview painter's, so the picture is
                // only right once it has been drawn.
                _ => urgency = RasterUrgency::Prompt,
            }
        }
        urgency
    }

    /// Keep drawing what `transaction` creates until a frame containing it
    /// arrives. Called as the transaction is sent; the worker's answer stamps
    /// the mark with its revision, and [`ReaderSession::frame_landed`] is what
    /// takes it down (§9.2).
    ///
    /// Only ink and highlights are retained: they are what the preview
    /// painter can draw. Text, notes and stamps stay editor-shaped until their
    /// frame arrives.
    ///
    /// The answer says whether the rendered picture has to catch up promptly
    /// or may wait for a quiet spell. A transaction every part of which is
    /// drawn by a preview leaves the picture *complete* rather than merely
    /// not-yet-stale, so nothing is gained by re-rendering it now — which is
    /// what lets a mark made and taken back within a few seconds cost no
    /// render at all.
    #[must_use]
    pub fn retain_commit(
        &mut self,
        transaction: &pulpit_render::document::DocumentTransaction,
    ) -> RasterUrgency {
        use pulpit_core::annotate::AnnotationCommand;

        let mut urgency = RasterUrgency::Deferred;
        for command in &transaction.0 {
            let pulpit_render::document::DocumentCommand::Annotation(command) = command else {
                // A form field's value is drawn by the renderer and by nothing
                // else here.
                urgency = RasterUrgency::Prompt;
                continue;
            };
            match command {
                AnnotationCommand::Create(draft) => match preview_of(draft) {
                    Some((page, preview)) => self.retained.push(RetainedMark {
                        page,
                        preview,
                        revision: None,
                        id: None,
                    }),
                    // Nothing can predraw it, so the page is wrong until the
                    // renderer has drawn it.
                    None => urgency = RasterUrgency::Prompt,
                },
                AnnotationCommand::Delete { id } => {
                    // A mark that is only a preview is unmade by dropping the
                    // preview, and the picture underneath was never wrong. A
                    // mark already in the picture is an absence nothing can
                    // predraw.
                    let held = self
                        .retained
                        .iter()
                        .position(|mark| mark.id.as_ref() == Some(id));
                    match held {
                        Some(at) => {
                            self.retained.remove(at);
                        }
                        None => urgency = RasterUrgency::Prompt,
                    }
                }
                AnnotationCommand::Replace { id, replacement } => {
                    let held = self
                        .retained
                        .iter_mut()
                        .find(|mark| mark.id.as_ref() == Some(id));
                    match (held, preview_of(replacement)) {
                        // Moving or restyling something that is still only a
                        // preview: the preview moves with it.
                        (Some(mark), Some((page, preview))) => {
                            mark.page = page;
                            mark.preview = preview;
                            // It is about to have a new revision, and until
                            // the worker says which, it must not be taken down
                            // by a frame at the old one.
                            mark.revision = None;
                        }
                        _ => urgency = RasterUrgency::Prompt,
                    }
                }
            }
        }
        urgency
    }

    /// The retained highlight washes on `page`, for the caller that
    /// composites them into the page picture.
    ///
    /// Highlights are handled apart from ink because of how a `/Highlight`
    /// is blended: PDFium's appearance multiplies the wash with the page, so
    /// text under it stays fully dark. A translucent rectangle drawn *over*
    /// the frame lightens that text, and the difference is a visible settle
    /// the moment the real frame arrives. Multiplying the frame's own pixels
    /// is the same arithmetic the renderer will do, so there is nothing to
    /// settle.
    pub fn retained_washes(
        &self,
        page: PageIndex,
    ) -> Vec<&crate::widgets::document::preview::GesturePreview> {
        self.retained
            .iter()
            .filter(|mark| mark.page == page && !mark.preview.quads.is_empty())
            .map(|mark| &mark.preview)
            .collect()
    }

    /// How many marks are currently drawn as previews rather than rendered.
    ///
    /// Each one is composited on every draw, so the set has to be bounded:
    /// deferring the render is free for a handful of marks and stops being
    /// free for a hundred.
    pub fn retained_count(&self) -> usize {
        self.retained.len()
    }

    /// A commit was refused or lost: the oldest unstamped retained mark is a
    /// mark the document will never contain, and keeping it drawn would show
    /// an edit that did not happen.
    pub fn commit_refused(&mut self) {
        if let Some(waiting) = self
            .retained
            .iter()
            .position(|mark| mark.revision.is_none())
        {
            self.retained.remove(waiting);
        }
    }

    /// A frame rendered from a snapshot at `revision` landed for `page`: the
    /// frame now shows every mark committed at or before it, so their
    /// previews come down.
    pub fn frame_landed(
        &mut self,
        page: PageIndex,
        revision: pulpit_render::document::DocumentRevision,
    ) {
        self.retained.retain(|mark| {
            mark.page != page || mark.revision.map(|r| r > revision).unwrap_or(true)
        });
    }

    /// A partial repaint of `region` at `revision` arrived for `page`.
    ///
    /// The answer says whether it may be used. A patch is drawn from the
    /// worker's document, so inside its rectangle it contains *every* mark
    /// committed at or before its revision — the ones previews are standing in
    /// for included. Any preview wholly inside it is therefore redundant and
    /// comes down, exactly as it would for a full frame (§9.2).
    ///
    /// A preview that only *partly* overlaps the rectangle is the case that
    /// cannot be reconciled: half of it is in the patch and half is not, so
    /// neither keeping it (the overlap would be drawn twice, and a highlight
    /// drawn twice is visibly darker) nor dropping it (the outside half would
    /// vanish) is right. The patch is refused, and the page waits for the
    /// snapshot it would have waited for anyway.
    #[must_use]
    pub fn patch_landed(
        &mut self,
        page: PageIndex,
        region: pulpit_core::notes::Region,
        revision: pulpit_render::document::DocumentRevision,
    ) -> bool {
        let Some(geometry) = self.page_geometry(page) else {
            return false;
        };
        let patched = pulpit_core::page::PageRect::new(
            region.x * geometry.width,
            region.y * geometry.height,
            (region.x + region.width) * geometry.width,
            (region.y + region.height) * geometry.height,
        );
        let covered = |mark: &RetainedMark| -> bool {
            mark.preview
                .bounds()
                .is_some_and(|bounds| patched.contains_rect(&bounds))
        };
        let overlaps = |mark: &RetainedMark| -> bool {
            mark.preview
                .bounds()
                .is_some_and(|bounds| patched.intersects(&bounds))
        };
        if self
            .retained
            .iter()
            .any(|mark| mark.page == page && overlaps(mark) && !covered(mark))
        {
            return false;
        }
        self.retained.retain(|mark| {
            mark.page != page
                || !covered(mark)
                || mark.revision.map(|at| at > revision).unwrap_or(true)
        });
        true
    }

    /// What is on a page, for hit-testing. Replaced wholesale, because the
    /// list *is* the document's `/Annots` order and a merge would invent one.
    pub fn set_annotations(
        &mut self,
        page: PageIndex,
        summaries: &[pulpit_render::document::AnnotationSummary],
    ) {
        self.annotation_requests.remove(&page);
        self.annotations.insert(
            page,
            summaries.iter().map(|summary| summary.to_hit()).collect(),
        );
        self.summaries.insert(page, summaries.to_vec());
    }

    /// What the document holds on one page, as last reported.
    ///
    /// Read by presentation as well as by document mode: the marks on a slide
    /// are a view of these (A1), so this is where a page turn gets them from
    /// rather than from a cache beside the file.
    pub fn annotations_on(
        &self,
        page: PageIndex,
    ) -> impl Iterator<Item = &pulpit_render::document::AnnotationSummary> {
        self.summaries.get(&page).into_iter().flatten()
    }

    /// The annotation the reader has picked up, if any (§8.4).
    ///
    /// Selection is application state: nothing about it is in the document,
    /// and it survives no restart.
    pub fn selected(&self) -> Option<&pulpit_core::annotate::AnnotationId> {
        self.interaction.selected()
    }

    /// Everything the reader is holding — a band's worth of marks, or the one
    /// the hand picked up, or nothing.
    pub fn selection(&self) -> &[pulpit_core::annotate::AnnotationId] {
        self.interaction.selection()
    }

    /// The pages whose annotations are not known and are worth asking for:
    /// the ones in the window.
    pub fn annotations_wanted(&mut self) -> Vec<PageIndex> {
        if !self.open {
            return Vec::new();
        }
        let wanted: Vec<PageIndex> = self
            .column
            .visible(self.controls.offset, self.cell.1)
            .into_iter()
            .map(|placed| placed.page)
            .filter(|page| {
                !self.annotations.contains_key(page) && !self.annotation_requests.contains(page)
            })
            .collect();
        self.annotation_requests.extend(wanted.iter().copied());
        wanted
    }

    /// Forget every annotation list in flight: the worker answered something
    /// other than what was asked, so nothing outstanding will be answered and
    /// each visible page must be askable again.
    pub fn annotations_abandoned(&mut self) {
        self.annotation_requests.clear();
    }

    /// Where a point on a page sits in the column, in layout points from the
    /// top of the document.
    ///
    /// The one unit a pan can be measured in: the pointer is reported in the
    /// page's own points on whichever page it is over, and a drag that starts
    /// on one page and continues onto the next has to mean one continuous
    /// movement rather than two.
    fn document_y(&self, page: PageIndex, y: f32) -> Option<f32> {
        Some(self.column.offset_of(page)? + y * self.scale)
    }

    /// The same across the column: a page point's distance from the column's
    /// left edge, in layout points.
    fn document_x(&self, page: PageIndex, x: f32) -> Option<f32> {
        Some(self.column.left_of(page)? + x * self.scale)
    }

    /// The whole grabbed point, when the column can place it.
    fn document_point(&self, page: PageIndex, x: f32, y: f32) -> Option<(f32, f32)> {
        Some((self.document_x(page, x)?, self.document_y(page, y)?))
    }

    /// Is the hand dragging the page about? The cursor says so while it is.
    pub fn is_panning(&self) -> bool {
        self.pan.is_some()
    }

    /// The pointer moved over a page, at a canonical page point (A4).
    pub fn pointer_moved(&mut self, page: PageIndex, x: f32, y: f32) {
        // The hand drags the document rather than marking it: the spot that
        // was grabbed is put back under the pointer, which is what makes a
        // page feel like a sheet of paper being pushed about (§8.1).
        if let Some((anchor_x, anchor_y)) = self.pan {
            if let Some((here_x, here_y)) = self.document_point(page, x, y) {
                self.controls.offset = self
                    .column
                    .clamp_offset(self.controls.offset + anchor_y - here_y, self.cell.1);
                self.controls.offset_x = self
                    .column
                    .clamp_offset_x(self.controls.offset_x + anchor_x - here_x, self.cell.0);
                if let Some(page) = self.column.current(self.controls.offset, self.cell.1) {
                    self.controls.page = page;
                }
            }
            // Nothing else: a hand that also moved the cursor would leave a
            // gesture half-open behind the pan.
            return;
        }
        let at = PagePoint::new(x, y);
        let from = self
            .cursor
            .filter(|(was, _)| *was == page)
            .map(|(_, at)| at);
        self.cursor = Some((page, at));
        self.interaction.extend(at);

        // A held mark follows the pointer. The document is untouched until
        // the release: what moves until then is a preview (§8.4). Which of the
        // mark follows it — all of it, or one corner — is the handle's to say.
        if let (
            Some(origin),
            Some(pulpit_core::annotate::Gesture::Transforming {
                original, handle, ..
            }),
        ) = (self.transform_origin, self.interaction.gesture().cloned())
        {
            let (dx, dy) = (at.x - origin.x, at.y - origin.y);
            self.interaction
                .set_transform(handle.applied(original, dx, dy));
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
            // The band is chrome and is drawn with the selection, not here:
            // this layer paints marks in their own colour, and the band is
            // not a mark.
            Gesture::Erasing { .. } | Gesture::Transforming { .. } | Gesture::Marquee { .. } => {
                return None
            }
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

    /// The text mark under the pointer, when there is one to reopen (§8.5).
    ///
    /// A mark that says something is a mark somebody may have mistyped, so a
    /// double click opens what it says rather than making a second mark on top
    /// of the first. What is opened is the *source* where there is one: a
    /// Typst mark reopens its markup, not the picture it compiled to (§7.4).
    pub fn text_under_cursor(&self) -> Option<EditableText> {
        let (page, at) = self.cursor?;
        let candidates = self.annotations.get(&page)?;
        let hit = pulpit_core::annotate::hit::topmost(candidates, at, self.eraser_tolerance())?;
        if !hit.editable {
            return None;
        }
        self.editable_mark(page, &hit.id)
    }

    /// The same, for the mark the reader has already picked up (§8.5).
    ///
    /// What the Enter key and the toolbar's edit control open. A selection is
    /// aimed at once and then acted on, which is easier than hitting a note's
    /// icon twice in a row at reading zoom.
    pub fn selected_editable(&self) -> Option<EditableText> {
        let id = self.interaction.selected()?.clone();
        let page = self
            .summaries
            .iter()
            .find(|(_, marks)| marks.iter().any(|summary| summary.id == id))
            .map(|(page, _)| *page)?;
        self.editable_mark(page, &id)
    }

    /// What the text editor opens for the mark `id` on `page`, when that mark
    /// is one this editor writes.
    fn editable_mark(
        &self,
        page: PageIndex,
        id: &pulpit_core::annotate::AnnotationId,
    ) -> Option<EditableText> {
        let summary = self
            .summaries
            .get(&page)?
            .iter()
            .find(|summary| &summary.id == id)?;
        if !summary.editable() {
            return None;
        }
        let typst = summary.contents.pulpit_source.is_some();
        let text = summary
            .contents
            .pulpit_source
            .clone()
            .unwrap_or_else(|| summary.contents.text.clone());
        // Only the kinds this editor writes. An ink stroke has no text, and a
        // highlight's `/Contents` is the page's own words copied in for
        // search — editing that would be editing a record of what was marked,
        // not the mark.
        let tool = match summary.kind {
            pulpit_core::annotate::AnnotationKind::FreeText => AnnotationTool::Text,
            pulpit_core::annotate::AnnotationKind::Note => AnnotationTool::Note,
            // A stamp is only ours to reopen when pulpit generated it from
            // markup; a check, a cross or a dropped-in picture has no text.
            pulpit_core::annotate::AnnotationKind::Stamp if typst => AnnotationTool::Text,
            _ => return None,
        };
        // The mark is rewritten where it already is, so the box does not walk
        // across the page every time it is edited.
        Some(EditableText {
            id: summary.id.clone(),
            page,
            at: PagePoint::new(summary.bounds.left, summary.bounds.top),
            tool,
            text,
            typst,
        })
    }

    /// Rewrite a text mark in place, keeping its identity (A3, §8.4).
    ///
    /// A replacement rather than a delete and a create, for the same reason a
    /// move is: the annotation the reader edited is the annotation that is
    /// still there afterwards, and one undo puts back what it said before.
    pub fn replace_text(
        &self,
        id: &pulpit_core::annotate::AnnotationId,
        page: PageIndex,
        text: String,
    ) -> Option<DocumentTransaction> {
        let geometry = self.pages.get(page.get()).copied()?;
        let summary = self
            .summaries
            .get(&page)?
            .iter()
            .find(|summary| &summary.id == id)?;
        let replacement = match summary.to_draft()? {
            pulpit_core::annotate::AnnotationDraft::FreeText(mut free) => {
                free.text = text;
                pulpit_core::annotate::AnnotationDraft::FreeText(free)
            }
            pulpit_core::annotate::AnnotationDraft::Note(mut note) => {
                note.text = text;
                pulpit_core::annotate::AnnotationDraft::Note(note)
            }
            // Anything else has no text to rewrite, and guessing at one would
            // replace a mark with a different kind of mark.
            _ => return None,
        };
        if replacement.validate(&geometry).is_err() {
            return None;
        }
        Some(DocumentTransaction::from_annotations([
            pulpit_core::annotate::AnnotationCommand::Replace {
                id: id.clone(),
                replacement,
            },
        ]))
    }

    /// Rewrite a Typst mark in place from freshly compiled markup (§7.4).
    ///
    /// The picture is new — the markup changed, so the appearance must — but
    /// the mark keeps its identity and its top-left corner. Only its size
    /// follows what Typst decided the new markup needs.
    pub fn replace_typst(
        &self,
        id: &pulpit_core::annotate::AnnotationId,
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
            pulpit_core::annotate::AnnotationCommand::Replace {
                id: id.clone(),
                replacement: draft,
            },
        ]))
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
            AnnotationTool::Note => pulpit_core::annotate::PlacedMark::Note { text },
            _ => pulpit_core::annotate::PlacedMark::FreeText {
                text,
                source: pulpit_core::annotate::TextSource::Plain,
                // A box wide enough for a line of comment at the style's own
                // size, and tall enough for two of them. The reader can move
                // and resize it afterwards; guessing narrower would clip the
                // first thing anybody types.
                size: (
                    (self.interaction.text_style().font_size * 18.0).min(geometry.width),
                    self.interaction.text_style().font_size * 2.4,
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
        // Nothing armed is the hand, and the hand takes hold of whatever is
        // under it: a mark, which it picks up to move or resize, or else the
        // page, which it pans. A mark that overlaps a link therefore wins the
        // press — the mark is the nearer thing and the one the reader put
        // there (§8.4).
        if self.controls.tool.is_none() {
            if self.pick_up(page, at) {
                return true;
            }
            self.pan = self.document_point(page, at.x, at.y);
            return false;
        }
        self.interaction.begin(page, at)
    }

    /// Pick up the topmost mark under `at`, and start moving it.
    ///
    /// A press on bare page puts down whatever was held: clicking away from a
    /// selection is how a selection is dismissed everywhere else.
    fn pick_up(&mut self, page: PageIndex, at: PagePoint) -> bool {
        // A corner of what is already held comes first: the handles sit on the
        // mark's own edge, so a press there would otherwise be read as a press
        // on the mark and start a move instead of a resize.
        if let Some((id, bounds, corner)) = self.handle_under(page, at) {
            self.interaction.begin_transform(
                id,
                page,
                bounds,
                pulpit_core::annotate::TransformHandle::Resize(corner),
            );
            self.transform_origin = Some(at);
            return true;
        }

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
        self.interaction.begin_transform(
            id,
            page,
            bounds,
            pulpit_core::annotate::TransformHandle::Move,
        );
        self.transform_origin = Some(at);
        true
    }

    /// How close to a corner counts as grabbing it, in page points.
    ///
    /// Wider than the eraser's slack: a handle is aimed at deliberately, and a
    /// grip the size of the dot that draws it is a grip nobody can hit.
    fn handle_tolerance(&self) -> f32 {
        /// Comfortably bigger than the drawn handle, on screen.
        const ON_SCREEN_POINTS: f32 = 9.0;
        if self.scale > 0.0 {
            ON_SCREEN_POINTS / self.scale
        } else {
            ON_SCREEN_POINTS
        }
    }

    /// The resize handle of the selected mark under `at`, if the pointer is on
    /// one. Only the selection has handles: an unselected mark is picked up
    /// whole, and offering corners on everything under the pointer would make
    /// the page a field of grips.
    fn handle_under(
        &self,
        page: PageIndex,
        at: PagePoint,
    ) -> Option<(
        pulpit_core::annotate::AnnotationId,
        pulpit_core::page::PageRect,
        pulpit_core::annotate::Corner,
    )> {
        let id = self.interaction.selected()?.clone();
        let bounds = self.resizable_bounds(page, &id)?;
        let tolerance = self.handle_tolerance();
        let corner = pulpit_core::annotate::Corner::ALL
            .into_iter()
            .find(|corner| {
                let point = corner.of(bounds);
                (point.x - at.x).abs() <= tolerance && (point.y - at.y).abs() <= tolerance
            })?;
        Some((id, bounds, corner))
    }

    /// Where the mark `id` is on `page`, when it is one a corner drag can
    /// reshape at all.
    fn resizable_bounds(
        &self,
        page: PageIndex,
        id: &pulpit_core::annotate::AnnotationId,
    ) -> Option<pulpit_core::page::PageRect> {
        let summary = self
            .summaries
            .get(&page)?
            .iter()
            .find(|summary| &summary.id == id)?;
        if !summary.editable() {
            return None;
        }
        summary
            .to_draft()
            .filter(pulpit_core::annotate::AnnotationDraft::is_resizable)
            .map(|_| summary.bounds)
    }

    /// The selected marks on `page`, as the page surface draws them (§8.4).
    ///
    /// While a drag is open this is where the mark *would* land rather than
    /// where the document still has it: the outline is the proposal the
    /// release will commit, which is the whole point of drawing it. The open
    /// rubber band is drawn the same way, because it is the same kind of
    /// thing — a rectangle proposing what the release will do.
    fn selection_for(&self, page: PageIndex) -> Vec<crate::widgets::context::SelectedMark> {
        use pulpit_core::annotate::Gesture;

        if let Some(Gesture::Transforming {
            id,
            page: held,
            current,
            ..
        }) = self.interaction.gesture()
        {
            if *held != page {
                return Vec::new();
            }
            return vec![crate::widgets::context::SelectedMark {
                bounds: *current,
                dragging: true,
                handles: self.handles_for(page, id),
            }];
        }

        // The band, while it is open, and nothing else: what it has gathered
        // is not decided until the pointer comes up, so outlining marks under
        // it would promise a selection the release might not make.
        if let Some((banded, band)) = self.interaction.marquee() {
            if banded != page {
                return Vec::new();
            }
            return vec![crate::widgets::context::SelectedMark {
                bounds: band,
                dragging: true,
                handles: Vec::new(),
            }];
        }

        let Some(candidates) = self.annotations.get(&page) else {
            return Vec::new();
        };
        // In paint order rather than selection order, so a stack of marks is
        // outlined the way the page draws it.
        candidates
            .iter()
            .filter(|candidate| self.interaction.is_selected(&candidate.id))
            .map(|hit| crate::widgets::context::SelectedMark {
                bounds: hit.bounds,
                dragging: false,
                // Grips only when one mark is held: a corner belongs to the
                // mark it is on, and four sets of them over a band's worth of
                // marks would be four sets nobody could aim at.
                handles: match self.interaction.selected() {
                    Some(only) if only == &hit.id => self.handles_for(page, only),
                    _ => Vec::new(),
                },
            })
            .collect()
    }

    /// The corners that can be grabbed on the mark `id`.
    fn handles_for(
        &self,
        page: PageIndex,
        id: &pulpit_core::annotate::AnnotationId,
    ) -> Vec<pulpit_core::annotate::Corner> {
        match self.resizable_bounds(page, id) {
            Some(_) => pulpit_core::annotate::Corner::ALL.to_vec(),
            None => Vec::new(),
        }
    }

    /// Take the selected marks out of the document (§8.4).
    ///
    /// The eraser is a sweep and takes whatever it passes over; this is the
    /// other way to remove marks — pick them out and press Delete — and a
    /// band's worth of them is one transaction and one undo entry, because
    /// one press of Delete is one thing the reader did (§9.1).
    pub fn delete_selected(&mut self) -> Option<DocumentTransaction> {
        // Marks this document only preserves are passed over rather than
        // refusing the whole press: pulpit does not rewrite what it does not
        // model (A5), and deleting is a rewrite, but one such mark inside a
        // band must not save the rest.
        let doomed: Vec<_> = self
            .interaction
            .selection()
            .iter()
            .filter(|id| {
                self.summaries
                    .values()
                    .flatten()
                    .find(|summary| &&summary.id == id)
                    .is_some_and(|summary| summary.editable())
            })
            .cloned()
            .collect();
        if doomed.is_empty() {
            return None;
        }
        // The gesture goes with them: a mark being dragged when Delete arrives
        // must not come back on the release.
        self.interaction.cancel();
        self.transform_origin = None;
        self.interaction.select(None);
        Some(DocumentTransaction::from_annotations(
            doomed
                .into_iter()
                .map(|id| pulpit_core::annotate::AnnotationCommand::Delete { id }),
        ))
    }

    /// Put down whatever is held, without committing anything. What Escape
    /// does when there is a selection but no open gesture.
    pub fn clear_selection(&mut self) -> bool {
        let had = !self.interaction.selection().is_empty();
        self.interaction.select(None);
        had
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
        // Letting go of the hand ends the pan and commits nothing: the
        // document was moved, not marked.
        if self.pan.take().is_some() {
            return Released::Nothing;
        }
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
        // A band commits nothing: what it changes is what the reader is
        // holding. Intercepted here for the same reason a transform is — the
        // annotations it has to be tested against live with this session and
        // not in the gesture (§8.4).
        if let Some((page, band)) = self.interaction.marquee() {
            self.interaction.cancel();
            let gathered = self
                .annotations
                .get(&page)
                .map(|candidates| {
                    pulpit_core::annotate::hit::enclosed(candidates, band)
                        .into_iter()
                        .map(|hit| hit.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            // An empty band puts down what was held rather than leaving it:
            // dragging over blank page is how a reader says "none of these",
            // and it reads the same as clicking away from a selection.
            self.interaction.select_many(gathered);
            return None;
        }

        // A move is a *replacement*, not a delete and a create: the mark keeps
        // its identity, which is what lets undo put back the same annotation
        // rather than a copy of it (A3, §8.4).
        if let Some(pulpit_core::annotate::Gesture::Transforming {
            id,
            page,
            original,
            current,
            handle,
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
            let draft = summary.to_draft()?;
            // A move carries the whole mark the same distance; a corner drag
            // scales it out of the box it was in and into the one it was
            // dragged to. Either way it is the same annotation afterwards.
            let moved = match handle {
                pulpit_core::annotate::TransformHandle::Move => {
                    draft.translated(current.left - original.left, current.top - original.top)?
                }
                pulpit_core::annotate::TransformHandle::Resize(_) => {
                    draft.resized(original, current)?
                }
            };
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
        self.pan = None;
    }

    /// Is a gesture open? The page surface draws its own preview while one is.
    pub fn is_drawing(&self) -> bool {
        self.interaction.is_drawing()
    }

    /// The pages in the window right now, as the column placed them. What
    /// the application pins in the frame cache: a visible sheet's picture
    /// must not be the entry the budget takes first.
    pub fn visible_pages(&self) -> Vec<PlacedPage> {
        if !self.open {
            return Vec::new();
        }
        self.column.visible(self.controls.offset, self.cell.1)
    }

    /// Whether the two history controls have anything to do, without paying
    /// for a whole facet.
    pub fn can_undo(&self) -> bool {
        self.undo_depth > 0
    }

    pub fn can_redo(&self) -> bool {
        self.redo_depth > 0
    }

    /// The operation that undoes the last edit, if there is one.
    pub fn undo_operation(&mut self) -> Option<pulpit_render::document::DocumentUndo> {
        self.undo_stack.pop()
    }

    /// …and the one that redoes it.
    pub fn redo_operation(&mut self) -> Option<pulpit_render::document::DocumentUndo> {
        self.redo_stack.pop()
    }

    /// The pages worth drawing right now, nearest first, at the size and
    /// quality to draw them.
    ///
    /// A *plan*, not a queue: the application checks each entry against the
    /// frame cache and its in-flight set before submitting anything, so the
    /// same entries can be returned tick after tick for free. What the plan
    /// decides is coverage and urgency — the pages on screen, then a window's
    /// worth of margin either side; a small coarse frame while the reader is
    /// moving, the full-size refined one once they have settled. Coarse
    /// entries come with the refined ones rather than instead of them, so a
    /// freshly landed page paints quickly and sharpens in place, the way a
    /// slide does.
    pub fn render_plan(&mut self, scale_factor: f32) -> Vec<PlannedRender> {
        if !self.open || !self.cell_known {
            return Vec::new();
        }
        let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let mut wanted = Vec::new();
        let on_screen = self.column.visible(self.controls.offset, self.cell.1);
        // A window's worth of pages either side of the screen, nearest first.
        // This is what keeps a scroll from arriving on white paper: the page
        // about to come into view was asked for while its predecessor was
        // being read. Strictly after everything on screen — the worker
        // answers in order, and a prefetch that delayed a visible page would
        // be worse than none.
        let top = self.controls.offset;
        let bottom = top + self.cell.1;
        let mut margin: Vec<PlacedPage> = self
            .column
            .visible(top - self.cell.1, self.cell.1 * 3.0)
            .into_iter()
            .filter(|placed| !on_screen.iter().any(|seen| seen.page == placed.page))
            .collect();
        margin.sort_by(|a, b| {
            let distance = |placed: &PlacedPage| {
                if placed.top > bottom {
                    placed.top - bottom
                } else {
                    top - placed.bottom()
                }
            };
            distance(a).total_cmp(&distance(b))
        });
        // Is the reader moving, or looking? A full page of a real document
        // takes the better part of a second to rasterise, which is forever at
        // scrolling speed: a reader who is moving gets a small frame that
        // arrives in a fraction of that, and the sharp one is rendered when
        // they stop. Blurred text for a moment beats white paper — the page
        // is *there*, and it is what they are steering by (A7).
        let moving = (self.controls.offset - self.last_render_offset).abs() > VIEWPORT_EPSILON;
        self.last_render_offset = self.controls.offset;

        use pulpit_render::protocol::Quality;
        for (placed, visible) in on_screen
            .into_iter()
            .map(|placed| (placed, true))
            .chain(margin.into_iter().map(|placed| (placed, false)))
        {
            let full = renderable_size(placed.width * scale, placed.height * scale);
            let preview = preview_size(full);
            // The coarse frame first, always: it is what a freshly landed
            // page paints from while the full one renders, and asking for it
            // costs nothing once anything at least as wide is cached.
            wanted.push(PlannedRender {
                page: placed.page,
                width: preview.0,
                height: preview.1,
                quality: Quality::Coarse,
                visible,
            });
            // The sharp frame only once the reader has settled: rendered
            // mid-scroll it would be finished for a page already left, and
            // the worker time it took was taken from the page arrived at.
            if !moving {
                wanted.push(PlannedRender {
                    page: placed.page,
                    width: full.0,
                    height: full.1,
                    quality: Quality::Refined,
                    visible,
                });
            }
        }
        wanted
    }

    /// The page surface was drawn at this size. Recomputes the column, because
    /// a fit that ignored the cell it is fitting to is not a fit.
    pub fn set_cell(&mut self, width: f32, height: f32) {
        // The height only until the surface has reported its own: the layout
        // knows what it allotted the cell, the surface knows what is left
        // after the chrome around it, and only the second of those is what a
        // page is fitted into.
        let height = if self.viewport_reported {
            self.cell.1
        } else {
            height.max(0.0)
        };
        let cell = (width.max(0.0), height);
        let first = !self.cell_known;
        self.cell_known = true;
        if (cell.0 - self.cell.0).abs() > VIEWPORT_EPSILON
            || (cell.1 - self.cell.1).abs() > VIEWPORT_EPSILON
            || first
        {
            self.cell = cell;
            self.relayout();
        }
    }

    /// Put the column at `offset`, and say which page that is.
    fn scroll_to(&mut self, offset: f32) {
        self.controls.offset = self.column.clamp_offset(offset, self.cell.1);
        if let Some(page) = self.column.current(self.controls.offset, self.cell.1) {
            self.controls.page = page;
        }
    }

    /// The surface reported how tall its window really is.
    ///
    /// Only a change worth a relayout counts: the reported height wobbles by
    /// a fraction of a point as the content changes, and re-fitting on every
    /// one of those is a page that shivers while it is read.
    fn set_viewport(&mut self, height: f32) {
        if !height.is_finite() || height <= 0.0 {
            return;
        }
        self.cell_known = true;
        // From here on the surface is the authority on its own height, and
        // the layout's estimate is not consulted again: two answers to "how
        // tall is the window" is a fit that changes every tick.
        self.viewport_reported = true;
        if (height - self.cell.1).abs() > VIEWPORT_EPSILON {
            self.cell.1 = height;
            self.relayout();
        }
    }

    /// Where a page key lands when whole pages fit in the window.
    ///
    /// `None` when they do not, which is when a page key means a screenful
    /// rather than a page.
    fn whole_page_step(&self, windows: i32) -> Option<f32> {
        let current = self.column.pages.get(self.controls.page.get())?;
        if current.height > self.cell.1 + VIEWPORT_EPSILON {
            return None;
        }
        // Only from the top of the page. A reader parked half way down a page
        // is reading the seam between two pages, and stepping to the top of
        // the next one there would scroll them backwards over lines they have
        // not read yet: from an unaligned offset a page key is a screenful.
        if (self.controls.offset - current.top).abs() > VIEWPORT_EPSILON {
            return None;
        }
        // A page key means a rowful: in a two-page spread the page beside the
        // one being read is already on screen, and stepping to it would be a
        // press that moved nothing.
        let across = match self.controls.spread {
            PageSpread::Single => 1,
            PageSpread::Double => 2,
        };
        let wanted = self.controls.page.get() as i64 + windows as i64 * across;
        let last = self.pages.len().saturating_sub(1) as i64;
        let wanted = wanted.clamp(0, last) as usize;
        self.column.pages.get(wanted).map(|placed| placed.top)
    }

    /// Recompute the scale and the column from the pages and the zoom.
    fn relayout(&mut self) {
        let reference = self
            .pages
            .get(self.controls.page.get())
            .or_else(|| self.pages.first())
            .copied()
            .unwrap_or_default();
        // A fit fits what is actually across the window: in a two-page spread
        // that is half of it, less the gap down the middle, or a fit-width
        // page would be as wide as the window and its neighbour off screen.
        let cell = match self.controls.spread {
            PageSpread::Single => self.cell,
            PageSpread::Double => (
                ((self.cell.0 - crate::widgets::document::model::PAGE_GAP) / 2.0).max(0.0),
                self.cell.1,
            ),
        };
        self.scale = self.controls.zoom.scale(&reference, cell);
        self.column = Column::lay_out(&self.pages, self.scale, self.cell.0, self.controls.spread);
        self.controls.offset = self.column.clamp_offset(self.controls.offset, self.cell.1);
        // A zoom out that makes the page fit across the window again takes the
        // sideways offset with it, rather than leaving the sheet parked off
        // centre with nowhere to scroll back to.
        self.controls.offset_x = self
            .column
            .clamp_offset_x(self.controls.offset_x, self.cell.0);
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
            ReadCommand::ScrollTo {
                offset,
                offset_x,
                viewport,
            } => {
                // The window's real height, which the fits and the page keys
                // are computed from. Taken before the offset is clamped: the
                // clamp is against the column this height lays out.
                self.set_viewport(*viewport);
                self.scroll_to(*offset);
                self.controls.offset_x = self.column.clamp_offset_x(*offset_x, self.cell.0);
                true
            }
            ReadCommand::DragScrollHandle(offset) => {
                self.scroll_to(*offset);
                true
            }
            ReadCommand::ScrollByWindows(windows) => {
                // A page that fits in the window is stepped a whole page at a
                // time: at "fit height" the key shows the next page and only
                // the next page, which is the entire point of that fit. A
                // page taller than the window is stepped a screenful less a
                // sliver, so the line the reader stopped on is still on
                // screen after the key.
                if let Some(offset) = self.whole_page_step(*windows) {
                    self.scroll_to(offset);
                    return true;
                }
                let stride = (self.cell.1 - WINDOW_OVERLAP).max(1.0);
                self.scroll_to(self.controls.offset + stride * *windows as f32);
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
                // The box holds the page number alone, but a pasted "12 / 40"
                // means page twelve to whoever pasted it, so the leading
                // number is what counts.
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
            ReadCommand::SetSpread(spread) => {
                if self.controls.spread == *spread {
                    return false;
                }
                self.controls.spread = *spread;
                // The page the reader is on stays the page they are on: the
                // column they were scrolled down is not the column they are
                // about to read, so the offset is recovered from the page
                // rather than kept as a number of points.
                let page = self.controls.page;
                self.relayout();
                if let Some(offset) = self.column.offset_of(page) {
                    self.scroll_to(offset);
                }
                true
            }
            ReadCommand::SetOutlineView(view) => {
                self.controls.outline = *view;
                false
            }
            ReadCommand::SetOutlineCollapsed(collapsed) => {
                self.controls.outline_collapsed = *collapsed;
                false
            }
            ReadCommand::Arm(tool) => {
                self.controls.tool = *tool;
                // The toolbar and the gesture state are one choice, not two:
                // arming through the interaction is also what abandons any
                // open gesture, so changing tools mid-stroke drops the stroke
                // rather than finishing it with the new tool.
                self.interaction.arm(*tool);
                // Choosing a tool is an answer to the toolbar; a popover left
                // open past it would be a question nobody is asking any more.
                self.controls.tool_options = None;
                false
            }
            ReadCommand::ToolOptions(tool) => {
                self.controls.tool_options = *tool;
                false
            }
            ReadCommand::SetToolColor(tool, color) => {
                match tool {
                    AnnotationTool::Ink => {
                        self.controls.ink_color = *color;
                        self.interaction.set_ink_style(pulpit_core::annotate::MarkStyle {
                            color: *color,
                            ..self.interaction.ink_style()
                        });
                    }
                    AnnotationTool::Highlighter => {
                        self.controls.highlight_color = *color;
                        self.interaction
                            .set_highlight_style(pulpit_core::annotate::MarkStyle {
                                color: *color,
                                ..self.interaction.highlight_style()
                            });
                    }
                    AnnotationTool::Text | AnnotationTool::Note => {
                        self.controls.text_color = *color;
                        self.interaction.set_text_style(pulpit_core::annotate::MarkStyle {
                            color: *color,
                            ..self.interaction.text_style()
                        });
                    }
                    // Nothing else lays a colour down; a command naming one is
                    // a stale message, and stale messages do nothing.
                    _ => {}
                }
                false
            }
            ReadCommand::SetInkWidth(width) => {
                self.interaction.set_ink_style(pulpit_core::annotate::MarkStyle {
                    width: *width,
                    ..self.interaction.ink_style()
                });
                // Read back after the engine's repair, so the slider shows the
                // width strokes will actually take.
                self.controls.ink_width = self.interaction.ink_style().width;
                false
            }
            ReadCommand::SetTextSize(size) => {
                self.interaction.set_text_style(pulpit_core::annotate::MarkStyle {
                    font_size: *size,
                    ..self.interaction.text_style()
                });
                // Read back after the engine's repair, so the control shows the
                // size text will actually be set at rather than what was asked.
                self.controls.text_size = self.interaction.text_style().font_size;
                false
            }
            ReadCommand::ClearSelection => {
                self.clear_selection();
                false
            }
            // The rest are the application's to route to the worker: a page
            // gesture, a field edit, an undo or a save is not a viewport
            // change, and this type owns only the viewport.
            ReadCommand::PageCursor { .. }
            | ReadCommand::PagePressed
            | ReadCommand::PageDoubleClicked
            | ReadCommand::PageReleased
            | ReadCommand::PageCancelled
            | ReadCommand::Undo
            | ReadCommand::Redo
            | ReadCommand::SaveAs
            // Writing a mark is the application's too: the text lives with the
            // half-written mark, and placing it is a document mutation.
            | ReadCommand::ComposeMark(_)
            | ReadCommand::ComposeAsTypst(_)
            | ReadCommand::CommitMark
            | ReadCommand::CancelMark
            // Removing a mark and rewriting one are both document mutations,
            // so both go to the worker rather than being answered here.
            | ReadCommand::DeleteSelected
            | ReadCommand::EditSelected => false,
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
    ///
    /// The pictures come from the caller: the frames live in the
    /// application's shared cache beside the slides', and this session knows
    /// only geometry. `frames` answers with the best resident picture for a
    /// page drawn at a given width in layout points — at another width or
    /// from before the last edit if that is what exists, because a soft or
    /// slightly stale sheet beats a blank one (A7).
    pub fn facet<'a>(
        &'a self,
        live: bool,
        frames: &dyn Fn(PageIndex, f32) -> Option<iced::widget::image::Handle>,
        search: &pulpit_core::search::SearchState,
    ) -> ReaderData<'a> {
        let current_hit = search.current().map(pulpit_core::search::Hit::key);
        let visible = if live && self.open {
            self.column
                .visible(self.controls.offset, self.cell.1)
                .into_iter()
                .map(|placed| ReaderPage {
                    // The open gesture, drawn by the UI so the stroke follows
                    // the hand rather than the round trip (A2). Only ever on
                    // the page the gesture is on.
                    preview: self.preview_for(placed.page),
                    // Retained ink only: a retained highlight is composited
                    // into the frame by the caller's `frames`, with the
                    // multiply blend a real `/Highlight` uses, and drawing it
                    // here as well would wash it twice.
                    retained: self
                        .retained
                        .iter()
                        .filter(|mark| mark.page == placed.page && mark.preview.quads.is_empty())
                        .map(|mark| mark.preview.clone())
                        .collect(),
                    canonical: self
                        .pages
                        .get(placed.page.get())
                        .map(|page| (page.width, page.height))
                        .unwrap_or((1.0, 1.0)),
                    // Whatever frame the cache has, even one drawn at another
                    // width or before the last edit: it is replaced when a
                    // newer one arrives (A7). Until the first one does, the
                    // sheet is drawn blank at its full size, so the column
                    // does not move under the reader when it lands.
                    frame: frames(placed.page, placed.width),
                    // The hit the reader is on is drawn differently from the
                    // rest, which is the whole use of an overlay: "there are
                    // six on this page and you are looking at the fourth".
                    found: search
                        .hits_on(placed.page)
                        .filter(|hit| Some(hit.key()) != current_hit)
                        .flat_map(|hit| hit.quads.iter().copied())
                        .collect(),
                    found_current: search
                        .hits_on(placed.page)
                        .filter(|hit| Some(hit.key()) == current_hit)
                        .flat_map(|hit| hit.quads.iter().copied())
                        .collect(),
                    // What the reader has picked up, so a held mark looks
                    // held. Nothing here is in the document (§8.4).
                    selection: self.selection_for(placed.page),
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
            viewport: self.cell.1,
            visible,
            controls: &self.controls,
            scale: self.scale,
            outline: &self.outline,
            level: self.level,
            warnings: &self.warnings,
            dirty: self.dirty,
            page_entry: self.page_entry.clone(),
            can_undo: self.undo_depth > 0,
            can_redo: self.redo_depth > 0,
            selected: !self.interaction.selection().is_empty(),
            panning: self.pan.is_some(),
            // Filled in by the application, which is where the half-written
            // mark lives: this session knows geometry, not editors.
            composing: None,
        }
    }
}

/// The most pixels one page frame may carry.
///
/// Sixteen megapixels of RGBA is sixty-four megabytes, well inside the render
/// pool's shared-memory ceiling and far more detail than a screen can show:
/// past it the frame is drawn at this size and scaled up, which is a slightly
/// soft page rather than a surprise allocation.
const MAX_FRAME_PIXELS: f64 = 16.0 * 1024.0 * 1024.0;

/// The widest a moving reader's frame is rendered, in physical pixels.
///
/// Chosen for latency rather than for legibility: at this size a page comes
/// back in a fraction of the time a full one takes, which is the difference
/// between scrolling over pages and scrolling over white paper. The sharp
/// frame replaces it as soon as the reader stops.
const PREVIEW_MAX_WIDTH: u32 = 480;

/// The coarse size to render `full` at while the reader is moving.
fn preview_size(full: (u32, u32)) -> (u32, u32) {
    let (width, height) = full;
    if width <= PREVIEW_MAX_WIDTH {
        return full;
    }
    let shrink = f64::from(PREVIEW_MAX_WIDTH) / f64::from(width);
    (
        PREVIEW_MAX_WIDTH,
        ((f64::from(height) * shrink).round() as u32).max(1),
    )
}

/// How far two heights or offsets may differ and still count as the same, in
/// layout points.
///
/// The surface and the session both hold a scroll position and a window size,
/// and the two travel through float arithmetic and a widget's own clamping on
/// the way to each other. Without a tolerance they disagree in the last
/// decimal place for ever, and each disagreement is a relayout and a fresh
/// pair of renders: a page that flickers as long as you look at it.
const VIEWPORT_EPSILON: f32 = 0.5;

/// How much of the window a page key keeps, in layout points.
///
/// Every reader that has ever pressed Page Down expects to find the line they
/// were on at the top of the new screenful rather than to have to guess at
/// what fell between the two.
const WINDOW_OVERLAP: f32 = 24.0;

/// The size to rasterise a page of `width` × `height` device pixels at,
/// bounded by what one frame may carry and by what the protocol will accept.
/// How wide the refined frame for a page drawn `width` layout points across
/// is asked for, in physical pixels.
///
/// The lookup that chooses which cached frame to draw has to round exactly as
/// [`renderable_size`] does, or the frame the plan asked for is a pixel wider
/// than the frame the view will accept and the coarse stand-in wins for ever.
/// The area and edge caps in `renderable_size` can only make the real frame
/// *narrower* than this, which a ceiling still admits.
pub fn rendered_width(width: f32, scale: f32) -> u32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let cap = f64::from(pulpit_render::document::protocol::DocumentRenderRequest::MAX_DIMENSION);
    (f64::from(width * scale).max(1.0).round().min(cap)) as u32
}

fn renderable_size(width: f32, height: f32) -> (u32, u32) {
    let width = f64::from(width).max(1.0);
    let height = f64::from(height).max(1.0);
    // Both bounds are ratios on the requested size, so the aspect the column
    // laid out is the aspect that comes back.
    let by_area = (MAX_FRAME_PIXELS / (width * height)).sqrt().min(1.0);
    let cap = f64::from(pulpit_render::document::protocol::DocumentRenderRequest::MAX_DIMENSION);
    let by_edge = (cap / width.max(height)).min(1.0);
    let shrink = by_area.min(by_edge);
    let clamp = |value: f64| value.max(1.0).round().min(cap) as u32;
    (clamp(width * shrink), clamp(height * shrink))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame source with nothing in it: these tests are about geometry and
    /// state, and the pictures live in the application's cache.
    fn no_frames(_: PageIndex, _: f32) -> Option<iced::widget::image::Handle> {
        None
    }

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

    /// The margin either side of the screen is planned, and strictly after
    /// what is on it: a prefetch that made a visible page wait would be
    /// worse than none, so the plan marks the difference for the priorities.
    #[test]
    fn pages_just_off_screen_are_planned_after_the_ones_on_it() {
        let mut session = open(20);
        // 612-wide pages in a 612x400 cell: page 0 intersects the window,
        // and the margin below reaches one window further.
        let plan = session.render_plan(1.0);
        assert_eq!(plan.first().map(|entry| entry.page), Some(PageIndex(0)));
        assert!(plan.first().unwrap().visible);
        let margin = plan
            .iter()
            .find(|entry| entry.page == PageIndex(1))
            .expect("the margin was not planned");
        assert!(!margin.visible);
        let last_visible = plan.iter().rposition(|entry| entry.visible).unwrap();
        let first_margin = plan.iter().position(|entry| !entry.visible).unwrap();
        assert!(last_visible < first_margin, "margin planned before screen");
        // The far end of the document was not asked for.
        assert!(!plan.iter().any(|entry| entry.page == PageIndex(10)));
    }

    /// The width the view will accept a frame at and the width the plan asks
    /// for it at are one number, whatever the scale factor does to the
    /// fraction. Truncating one of the two put the refined frame a pixel over
    /// the ceiling and left the coarse stand-in on screen for good.
    #[test]
    fn the_sharp_frame_is_never_a_pixel_wider_than_the_view_will_take() {
        let mut session = open(3);
        // A cell whose fitted page is an odd width, so the scale factors
        // below land on a half pixel rather than on a whole one: that is the
        // case the two roundings used to disagree about.
        session.set_cell(613.0, 400.0);
        let mut halves = 0;
        for scale in [1.0_f32, 1.25, 1.5, 2.5, 3.5] {
            // Two calls at the same offset: the first settles the reader, the
            // second is the plan that carries the refined entries.
            let _ = session.render_plan(scale);
            let plan = session.render_plan(scale);
            let refined: Vec<&PlannedRender> = plan
                .iter()
                .filter(|entry| entry.quality == pulpit_render::protocol::Quality::Refined)
                .collect();
            assert!(!refined.is_empty(), "a settled reader plans a sharp page");
            for entry in refined {
                let placed = session
                    .column
                    .visible(0.0, 1e9)
                    .into_iter()
                    .find(|placed| placed.page == entry.page)
                    .expect("a planned page is a placed page");
                if (placed.width * scale).fract() >= 0.5 {
                    halves += 1;
                }
                assert_eq!(
                    entry.width,
                    rendered_width(placed.width, scale),
                    "the plan and the lookup disagree at {scale}"
                );
            }
        }
        assert!(halves > 0, "the case that used to fail was never exercised");
    }

    #[test]
    fn a_closed_reader_has_nothing_open_and_says_so() {
        let session = ReaderSession::new();
        assert!(!session.is_open());
        let facet = session.facet(true, &no_frames, &pulpit_core::search::SearchState::new());
        assert!(!facet.open);
        assert_eq!(facet.page_label(), "—");
        assert_eq!(facet.page_total(), "");
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
        assert_eq!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .page_label(),
            "1"
        );
        assert_eq!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .page_total(),
            "/ 5"
        );
    }

    #[test]
    fn a_page_key_moves_a_window_at_a_time_and_stops_at_the_ends() {
        let mut session = open(10);
        // The cell is 400 tall, so a window is 400 less the overlap that
        // keeps the line the reader stopped on in sight.
        assert!(session.apply(&ReadCommand::ScrollByWindows(1)));
        assert!((session.controls().offset - (400.0 - WINDOW_OVERLAP)).abs() < 1e-3);
        session.apply(&ReadCommand::ScrollByWindows(-1));
        assert_eq!(session.controls().offset, 0.0);
        // Past the top is the top, not a negative offset.
        session.apply(&ReadCommand::ScrollByWindows(-1));
        assert_eq!(session.controls().offset, 0.0);
        // …and past the end is the end.
        session.apply(&ReadCommand::ScrollByWindows(1_000));
        assert_eq!(session.controls().page, PageIndex(9));
    }

    #[test]
    fn the_hand_drags_the_document_rather_than_marking_it() {
        let mut session = open(10);
        session.apply(&ReadCommand::Arm(None));
        // Grab a point half way down the first page and pull it upwards:
        // the document follows the hand.
        session.pointer_moved(PageIndex(0), 300.0, 400.0);
        assert!(!session.pointer_pressed(), "the hand starts no gesture");
        assert!(session.is_panning());
        session.pointer_moved(PageIndex(0), 300.0, 300.0);
        assert!(
            (session.controls().offset - 100.0).abs() < 1e-3,
            "a hundred points of drag is a hundred points of scroll: {}",
            session.controls().offset
        );
        // Letting go commits nothing — the document was moved, not marked.
        assert!(matches!(session.pointer_released(), Released::Nothing));
        assert!(!session.is_panning());
        assert!(!session.is_dirty());
    }

    #[test]
    fn the_hand_drags_sideways_too_once_the_page_is_wider_than_the_window() {
        let mut session = open(3);
        session.apply(&ReadCommand::Arm(None));
        // Twice the width of the cell, so there is half a page of room to
        // either side of what is on screen.
        session.apply(&ReadCommand::SetZoom(Zoom::Fixed(2.0)));
        assert_eq!(session.controls().offset_x, 0.0);

        session.pointer_moved(PageIndex(0), 300.0, 100.0);
        session.pointer_pressed();
        // Pull the page leftwards by a hundred page points, which at 2× is two
        // hundred layout points of movement across.
        session.pointer_moved(PageIndex(0), 200.0, 100.0);
        assert!(
            (session.controls().offset_x - 200.0).abs() < 1e-3,
            "the grabbed spot stays under the hand: {}",
            session.controls().offset_x
        );
        // Past the right edge of the page is the right edge of the page.
        session.pointer_moved(PageIndex(0), -10_000.0, 100.0);
        let furthest = session.column.width - 612.0;
        assert!((session.controls().offset_x - furthest).abs() < 1e-3);
        session.pointer_released();

        // Zooming back out to a page that fits takes the sideways offset with
        // it: there is nowhere left to be parked.
        session.apply(&ReadCommand::SetZoom(Zoom::FitWidth));
        assert_eq!(session.controls().offset_x, 0.0);
    }

    #[test]
    fn the_hand_stays_put_sideways_when_the_page_fits_across_the_window() {
        let mut session = open(3);
        session.apply(&ReadCommand::Arm(None));
        session.pointer_moved(PageIndex(0), 300.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        assert_eq!(
            session.controls().offset_x,
            0.0,
            "a fitted page has no room to move across"
        );
    }

    #[test]
    fn at_fit_height_a_page_key_shows_the_next_page_and_only_the_next_page() {
        let mut session = open(6);
        session.apply(&ReadCommand::SetZoom(Zoom::FitHeight));
        // The whole page, top to bottom, in the window.
        let first = session
            .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
            .visible;
        assert_eq!(first.len(), 1, "one page fills the window, not two");
        assert_eq!(session.controls().offset, 0.0);

        session.apply(&ReadCommand::ScrollByWindows(1));
        assert_eq!(session.controls().page, PageIndex(1));
        // Exactly the next page's top: no sliver of the one before it, and
        // nothing of the one after.
        let expected = session.column.offset_of(PageIndex(1)).unwrap();
        assert!((session.controls().offset - expected).abs() < 1e-3);
        assert_eq!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .visible
                .len(),
            1
        );

        session.apply(&ReadCommand::ScrollByWindows(-1));
        assert_eq!(session.controls().page, PageIndex(0));
        assert_eq!(session.controls().offset, 0.0);
    }

    #[test]
    fn a_page_key_from_half_way_down_a_page_moves_a_windowful_forward() {
        let mut session = open(6);
        session.apply(&ReadCommand::SetZoom(Zoom::FitHeight));
        // Half way down the first page, the way a scroll wheel leaves it.
        let half = session.column.pages[1].top / 2.0;
        session.apply(&ReadCommand::DragScrollHandle(half));
        assert!((session.controls().offset - half).abs() < 1e-3);

        session.apply(&ReadCommand::ScrollByWindows(1));
        // A windowful on from where the reader was, not back up to a page top.
        let expected = half + (400.0 - WINDOW_OVERLAP);
        assert!(
            (session.controls().offset - expected).abs() < 1e-3,
            "a page key from mid-page scrolls a window: {}",
            session.controls().offset
        );
    }

    #[test]
    fn the_surface_is_the_authority_on_how_tall_the_window_is() {
        let mut session = open(4);
        // The layout said 400; the surface says it is really 300, and a fit
        // is fitted to the second of those.
        session.apply(&ReadCommand::ScrollTo {
            offset: 0.0,
            offset_x: 0.0,
            viewport: 300.0,
        });
        session.apply(&ReadCommand::SetZoom(Zoom::FitHeight));
        assert!(
            (session.scale - 300.0 / 792.0).abs() < 1e-4,
            "{}",
            session.scale
        );
        // …and the layout does not get to say otherwise afterwards.
        session.set_cell(612.0, 400.0);
        assert!(
            (session.scale - 300.0 / 792.0).abs() < 1e-4,
            "{}",
            session.scale
        );
    }

    #[test]
    fn fit_height_puts_one_whole_page_in_the_window() {
        let mut session = open(4);
        session.apply(&ReadCommand::SetZoom(Zoom::FitHeight));
        // 792 points of page into 400 points of cell.
        assert!(
            (session.scale - 400.0 / 792.0).abs() < 1e-4,
            "{}",
            session.scale
        );
        assert_eq!(session.controls().zoom.label(session.scale), "Fit height");
    }

    #[test]
    fn opening_a_second_document_forgets_the_first() {
        let mut session = open(5);
        session.apply(&ReadCommand::GoToPage(PageIndex(4)));
        let _ = session.applied(&applied(DocumentRevision(3)), AppliedKind::Edit);
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
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .page_entry
                .is_none(),
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
    fn the_page_box_takes_the_leading_number_of_a_pasted_counter() {
        // The box holds the page number alone, but "7 / 40" pasted into it
        // means page seven, not a parse failure.
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
    fn a_page_too_large_to_travel_is_asked_for_smaller_rather_than_not_at_all() {
        // Under the ceiling: asked for exactly as laid out.
        assert_eq!(renderable_size(1_000.0, 1_400.0), (1_000, 1_400));

        // Past it: the same shape, small enough for one message.
        let (width, height) = renderable_size(8_000.0, 11_000.0);
        let pixels = f64::from(width) * f64::from(height);
        assert!(pixels <= MAX_FRAME_PIXELS, "{pixels} pixels is too many");
        assert!(
            (f64::from(width) / f64::from(height) - 8.0 / 11.0).abs() < 1e-3,
            "shrinking must not change the page's shape"
        );

        // …and never past what the protocol will accept in one direction.
        let cap = pulpit_render::document::protocol::DocumentRenderRequest::MAX_DIMENSION;
        let (width, height) = renderable_size(200_000.0, 10.0);
        assert!(width <= cap && height >= 1);
    }

    #[test]
    fn a_moving_reader_plans_a_coarse_page_now_and_a_sharp_one_when_they_stop() {
        use pulpit_render::protocol::Quality;
        let mut session = open(20);
        // Standing still: the full-size refined frame is in the plan, with a
        // coarse one ahead of it for the first paint.
        let settled = session.render_plan(2.0);
        assert!(settled
            .iter()
            .any(|entry| entry.quality == Quality::Refined && entry.width > PREVIEW_MAX_WIDTH));
        assert!(settled
            .iter()
            .any(|entry| entry.quality == Quality::Coarse && entry.width <= PREVIEW_MAX_WIDTH));

        // Scrolling: only what can be drawn quickly, so the reader is not
        // steering by white paper and a worker is never mid-way through a
        // sharp render of a page already left behind.
        session.apply(&ReadCommand::ScrollTo {
            offset: 4_000.0,
            offset_x: 0.0,
            viewport: 400.0,
        });
        let moving = session.render_plan(2.0);
        assert!(!moving.is_empty());
        for entry in &moving {
            assert!(
                entry.width <= PREVIEW_MAX_WIDTH && entry.quality == Quality::Coarse,
                "a page asked for mid-scroll must be one that arrives: {entry:?}"
            );
        }

        // Stopped: the same offset twice is settled, and the plan sharpens.
        let stopped = session.render_plan(2.0);
        assert!(
            stopped.iter().any(|entry| entry.width > PREVIEW_MAX_WIDTH),
            "a reader who has stopped gets the real page: {stopped:?}"
        );
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
        session.apply(&ReadCommand::ScrollTo {
            offset: -1_000.0,
            offset_x: 0.0,
            viewport: 400.0,
        });
        assert_eq!(session.controls().offset, 0.0);
        session.apply(&ReadCommand::ScrollTo {
            offset: 1e9,
            offset_x: 0.0,
            viewport: 400.0,
        });
        assert_eq!(
            session.controls().page,
            PageIndex(2),
            "scrolling to the end is being on the last page"
        );
    }

    #[test]
    fn a_two_page_spread_fits_half_the_window_and_keeps_the_page() {
        let mut session = open(6);
        session.apply(&ReadCommand::GoToPage(PageIndex(3)));
        let single = session.scale;
        assert!(session.apply(&ReadCommand::SetSpread(PageSpread::Double)));
        // Fit width across half the window, so both halves are on screen.
        assert!(session.scale < single);
        // The page the reader was on is still on screen — as the right half
        // of the spread it now shares, so the counter says the left half.
        assert_eq!(session.controls().page, PageIndex(2));
        let visible: Vec<_> = session
            .visible_pages()
            .into_iter()
            .map(|placed| placed.page)
            .collect();
        assert!(visible.contains(&PageIndex(3)));
        // Setting the spread it is already in changes nothing.
        assert!(!session.apply(&ReadCommand::SetSpread(PageSpread::Double)));
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
        let facet = session.facet(true, &no_frames, &pulpit_core::search::SearchState::new());
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
        let facet = session.facet(false, &no_frames, &pulpit_core::search::SearchState::new());
        assert!(!facet.open);
        assert!(facet.visible.is_empty());
    }

    #[test]
    fn the_history_controls_follow_what_the_worker_reported() {
        let mut session = open(2);
        let facet = session.facet(true, &no_frames, &pulpit_core::search::SearchState::new());
        assert!(!facet.can_undo && !facet.can_redo);
        let _ = session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        let facet = session.facet(true, &no_frames, &pulpit_core::search::SearchState::new());
        assert!(facet.can_undo && !facet.can_redo);
        assert!(facet.dirty);
        assert_eq!(session.revision(), DocumentRevision(1));
    }

    #[test]
    fn only_the_window_and_its_margin_are_planned() {
        use pulpit_render::protocol::Quality;
        let mut session = open(20);
        let plan = session.render_plan(1.0);
        assert!(!plan.is_empty());
        let pages: std::collections::HashSet<PageIndex> =
            plan.iter().map(|entry| entry.page).collect();
        assert!(
            pages.len() <= 3,
            "a twenty-page document planned {} pages",
            pages.len()
        );
        assert_eq!(plan[0].page, PageIndex(0));
        // The refined width is the width the column placed the page at.
        let refined = plan
            .iter()
            .find(|entry| entry.quality == Quality::Refined)
            .unwrap();
        assert_eq!(refined.width, 612);
    }

    #[test]
    fn the_plan_follows_the_cell_it_is_drawn_in() {
        use pulpit_render::protocol::Quality;
        let mut session = open(3);
        let narrow = session.render_plan(1.0);
        // A wider cell is a different picture, not the same one stretched.
        session.set_cell(1_224.0, 400.0);
        let wide = session.render_plan(1.0);
        let width_of = |plan: &[PlannedRender]| {
            plan.iter()
                .find(|entry| entry.quality == Quality::Refined)
                .map(|entry| entry.width)
                .unwrap()
        };
        assert!(width_of(&wide) > width_of(&narrow));
    }

    #[test]
    fn an_edit_drops_what_was_on_the_pages_it_touched() {
        // The frames stay — the application replaces them when ones from a
        // newer snapshot arrive (A7) — but the annotation lists the eraser
        // hit-tests against are stale the moment the page is edited.
        let mut session = open(3);
        assert_eq!(session.annotations_wanted(), vec![PageIndex(0)]);
        session.set_annotations(PageIndex(0), &[]);
        assert!(session.annotations_wanted().is_empty());

        let _ = session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        assert_eq!(
            session.annotations_wanted(),
            vec![PageIndex(0)],
            "the edited page's annotations were not re-asked for"
        );
    }

    #[test]
    fn an_annotation_list_is_not_asked_for_again_while_its_answer_is_in_flight() {
        let mut session = open(3);
        assert_eq!(session.annotations_wanted(), vec![PageIndex(0)]);
        // Every tick until the answer lands used to re-ask; on a serial
        // worker those duplicates queued ahead of the page renders.
        assert!(session.annotations_wanted().is_empty());
        // The worker failed instead of answering: askable again.
        session.annotations_abandoned();
        assert_eq!(session.annotations_wanted(), vec![PageIndex(0)]);
    }

    #[test]
    fn undo_and_redo_move_operations_between_the_two_stacks() {
        let mut session = open(2);
        let _ = session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_undo
        );
        assert!(
            !session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_redo
        );

        let operation = session.undo_operation().expect("something to undo");
        let _ = session.applied(&applied(DocumentRevision(2)), AppliedKind::Undo);
        assert!(
            !session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_undo
        );
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_redo
        );
        let _ = operation;

        session.redo_operation().expect("something to redo");
        let _ = session.applied(&applied(DocumentRevision(3)), AppliedKind::Redo);
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_undo
        );
        assert!(
            !session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_redo
        );
    }

    #[test]
    fn a_new_edit_makes_the_taken_back_future_unreachable() {
        // Standard undo semantics, stated because getting it wrong leaves a
        // redo control that puts back an edit from a history the user left.
        let mut session = open(2);
        let _ = session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        session.undo_operation();
        let _ = session.applied(&applied(DocumentRevision(2)), AppliedKind::Undo);
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_redo
        );

        let _ = session.applied(&applied(DocumentRevision(3)), AppliedKind::Edit);
        assert!(
            !session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_redo
        );
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .can_undo
        );
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

    /// A free-text box on page zero, for the selection tests: a kind that is
    /// both movable and resizable, and that says something.
    fn text_box() -> pulpit_render::document::AnnotationSummary {
        use pulpit_core::page::PageRect;
        pulpit_render::document::AnnotationSummary {
            kind: pulpit_core::annotate::AnnotationKind::FreeText,
            bounds: PageRect::new(100.0, 100.0, 300.0, 140.0),
            path: Vec::new(),
            contents: pulpit_render::document::AnnotationContents {
                text: "a first thought".into(),
                ..Default::default()
            },
            ..stroke_at(1.0)
        }
    }

    /// Pick a mark up with the hand — nothing armed, which is how a mark is
    /// picked up (§8.4).
    fn holding(session: &mut ReaderSession, at: (f32, f32)) -> bool {
        session.apply(&ReadCommand::Arm(None));
        session.pointer_moved(PageIndex(0), at.0, at.1);
        session.pointer_pressed()
    }

    /// The one mark drawn as selected on the first page.
    fn drawn_selection(session: &ReaderSession) -> crate::widgets::context::SelectedMark {
        let mut marks = session.selection_for(PageIndex(0));
        assert_eq!(marks.len(), 1, "exactly one mark is drawn as selected");
        marks.remove(0)
    }

    #[test]
    fn a_held_mark_is_drawn_where_it_would_land_rather_than_where_it_is() {
        // Without this the reader drags an invisible thing and finds out where
        // it went a round trip later (§8.4).
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[text_box()]);
        assert!(holding(&mut session, (200.0, 120.0)));

        let selected = drawn_selection(&session);
        assert!(selected.dragging);
        assert_eq!(selected.bounds.left, 100.0, "it has not moved yet");

        session.pointer_moved(PageIndex(0), 240.0, 130.0);
        let selected = drawn_selection(&session);
        assert_eq!(selected.bounds.left, 140.0, "the ghost follows the pointer");
        assert_eq!(selected.bounds.top, 110.0);
        assert_eq!(
            selected.bounds.width(),
            200.0,
            "a move does not change the size"
        );
    }

    #[test]
    fn a_mark_that_cannot_be_reshaped_is_outlined_without_grips() {
        // Offering a grip that does nothing is worse than offering none.
        let mut session = open(2);
        let note = pulpit_render::document::AnnotationSummary {
            kind: pulpit_core::annotate::AnnotationKind::Note,
            ..text_box()
        };
        session.set_annotations(PageIndex(0), &[note]);
        assert!(holding(&mut session, (200.0, 120.0)));
        let selected = drawn_selection(&session);
        assert!(
            selected.handles.is_empty(),
            "a note is drawn at a fixed size whatever its rect says"
        );

        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[text_box()]);
        assert!(holding(&mut session, (200.0, 120.0)));
        assert_eq!(drawn_selection(&session).handles.len(), 4);
    }

    #[test]
    fn dragging_a_corner_resizes_rather_than_moves() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[text_box()]);
        // Select it, let go, then take it by the bottom-right corner.
        assert!(holding(&mut session, (200.0, 120.0)));
        session.pointer_released();
        assert!(holding(&mut session, (300.0, 140.0)));

        session.pointer_moved(PageIndex(0), 360.0, 190.0);
        let held = drawn_selection(&session);
        assert_eq!(held.bounds.left, 100.0, "the anchored corner moved");
        assert_eq!(held.bounds.top, 100.0, "the anchored corner moved");
        assert_eq!(held.bounds.right, 360.0);
        assert_eq!(held.bounds.bottom, 190.0);

        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("a resize commits")
        };
        assert_eq!(transaction.len(), 1, "one drag is one undo entry");
    }

    #[test]
    fn a_selected_mark_can_be_deleted_without_sweeping_an_eraser_over_it() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[text_box()]);
        assert!(holding(&mut session, (200.0, 120.0)));
        session.pointer_released();

        let transaction = session.delete_selected().expect("a held mark is deletable");
        assert_eq!(transaction.len(), 1);
        assert!(
            session.selected().is_none(),
            "what was deleted is no longer held"
        );
        assert!(
            session.delete_selected().is_none(),
            "there is nothing left to delete"
        );
    }

    #[test]
    fn deleting_while_a_drag_is_open_does_not_leave_the_move_to_commit() {
        // Otherwise the release would put back the mark that was just removed.
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[text_box()]);
        assert!(holding(&mut session, (200.0, 120.0)));
        session.pointer_moved(PageIndex(0), 240.0, 130.0);
        assert!(session.delete_selected().is_some());
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn nothing_is_deletable_that_pulpit_only_preserves() {
        // A5: what pulpit does not model it does not rewrite, and deleting is
        // a rewrite.
        let mut session = open(2);
        let foreign = pulpit_render::document::AnnotationSummary {
            support: pulpit_render::document::AnnotationSupport::Unsupported,
            ..text_box()
        };
        session.set_annotations(PageIndex(0), &[foreign]);
        assert!(!holding(&mut session, (200.0, 120.0)), "it does not move");
        assert!(
            session.selected().is_some(),
            "but it is selected, and says so"
        );
        assert!(session.delete_selected().is_none());
    }

    #[test]
    fn the_selection_reaches_the_editor_without_hitting_the_mark_twice() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[text_box()]);
        assert!(holding(&mut session, (200.0, 120.0)));
        let found = session
            .selected_editable()
            .expect("a text box says something");
        assert_eq!(found.text, "a first thought");
        assert_eq!(found.tool, AnnotationTool::Text);

        assert!(session.clear_selection());
        assert!(session.selected_editable().is_none());
    }

    #[test]
    fn placed_text_is_written_at_the_size_the_control_was_left_at() {
        let mut session = open(2);
        session.apply(&ReadCommand::SetTextSize(30.0));
        assert_eq!(session.controls().text_size, 30.0);
        // Repaired on the way in, and read back so the control shows what text
        // will actually be set at.
        session.apply(&ReadCommand::SetTextSize(9_000.0));
        assert!(session.controls().text_size < 9_000.0);
        assert_eq!(
            session.controls().text_size,
            session.interaction.text_style().font_size
        );
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
        let _ = session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
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

        let facet = session.facet(true, &no_frames, &pulpit_core::search::SearchState::new());
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
            .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
            .visible
            .iter()
            .any(|page| page.preview.is_some()));

        let Released::Commit(_) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .visible
                .iter()
                .all(|page| page.preview.is_none()),
            "the preview outlived the gesture"
        );
    }

    #[test]
    fn a_committed_stroke_stays_drawn_until_a_frame_containing_it_arrives() {
        // The retained preview bridges the snapshot round trip (§9.2):
        // without it the stroke follows the hand, vanishes at release, and
        // reappears the better part of a second later.
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        let _ = session.retain_commit(&transaction);

        let shown = |session: &ReaderSession| {
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .visible
                .iter()
                .any(|page| !page.retained.is_empty())
        };
        assert!(shown(&session), "drawn while the commit is in flight");

        // The worker answers, stamping the mark with its revision.
        let _ = session.applied(&applied(DocumentRevision(2)), AppliedKind::Edit);
        assert!(shown(&session), "still drawn until a frame contains it");

        // A frame from before the commit changes nothing…
        session.frame_landed(PageIndex(0), DocumentRevision(1));
        assert!(shown(&session), "an older frame does not contain the mark");
        // …a frame for another page changes nothing…
        session.frame_landed(PageIndex(1), DocumentRevision(2));
        assert!(shown(&session), "another page's frame says nothing");
        // …and the frame that contains it takes it down.
        session.frame_landed(PageIndex(0), DocumentRevision(2));
        assert!(!shown(&session), "the preview outlived its frame");
    }

    #[test]
    fn a_refused_commit_takes_its_retained_preview_down() {
        let mut session = open(1);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 150.0, 120.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        let _ = session.retain_commit(&transaction);
        session.commit_refused();
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .visible
                .iter()
                .all(|page| page.retained.is_empty()),
            "a mark the document refused must not stay drawn"
        );
    }

    #[test]
    fn a_retained_highlight_is_a_wash_for_the_frame_not_an_overlay() {
        // A committed `/Highlight` multiplies with the page; an alpha
        // rectangle drawn over the frame lightens the text under it and
        // settles visibly when the real frame lands. So the highlight goes
        // to the frame compositor, and only ink stays a drawn overlay.
        use pulpit_core::annotate::{
            AnnotationCommand, AnnotationDraft, HighlightDraft, MarkStyle,
        };

        let mut session = open(1);
        let draft = AnnotationDraft::Highlight(HighlightDraft {
            page: PageIndex(0),
            quads: vec![pulpit_core::page::PageQuad::from_rect(
                pulpit_core::page::PageRect::new(72.0, 100.0, 300.0, 114.0),
            )],
            text: "the words".into(),
            style: MarkStyle::highlighter(),
        });
        let transaction = pulpit_render::document::DocumentTransaction::from_annotations([
            AnnotationCommand::Create(draft),
        ]);
        let _ = session.retain_commit(&transaction);

        assert_eq!(session.retained_washes(PageIndex(0)).len(), 1);
        assert!(
            session.retained_washes(PageIndex(1)).is_empty(),
            "the wash is on its own page"
        );
        assert!(
            session
                .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
                .visible
                .iter()
                .all(|page| page.retained.is_empty()),
            "a wash must not also be drawn as an overlay"
        );

        // …and it comes down like any retained mark.
        let _ = session.applied(&applied(DocumentRevision(2)), AppliedKind::Edit);
        session.frame_landed(PageIndex(0), DocumentRevision(2));
        assert!(session.retained_washes(PageIndex(0)).is_empty());
    }

    /// An answer that names the annotation it made, which is what gives a
    /// retained preview its identity.
    fn applied_naming(
        revision: DocumentRevision,
        summary: pulpit_render::document::AnnotationSummary,
    ) -> pulpit_render::document::Applied {
        pulpit_render::document::Applied {
            effects: vec![pulpit_render::document::AppliedEffect::Annotation(
                Box::new(summary),
            )],
            ..applied(revision)
        }
    }

    /// A stroke drawn and taken back before the renderer ever heard of it: the
    /// preview goes, the picture underneath was never wrong, and nothing has
    /// to be rendered to make the screen right. This is what makes undo of a
    /// fresh mark instant.
    #[test]
    fn undoing_a_mark_that_is_still_only_a_preview_costs_no_render() {
        let mut session = open(1);
        let stroke = stroke_at(100.0);
        let id = stroke.id.clone();

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        assert_eq!(
            session.retain_commit(&transaction),
            RasterUrgency::Deferred,
            "a stroke the preview painter can draw needs no render"
        );
        let _ = session.applied(
            &applied_naming(DocumentRevision(2), stroke),
            AppliedKind::Edit,
        );
        assert_eq!(session.retained_count(), 1);

        // The undo comes back as a deletion of that same mark.
        let undone = pulpit_render::document::Applied {
            effects: vec![pulpit_render::document::AppliedEffect::Deleted(id)],
            ..applied(DocumentRevision(3))
        };
        assert_eq!(
            session.applied(&undone, AppliedKind::Undo),
            RasterUrgency::Deferred,
            "unmaking a preview is not a reason to re-render"
        );
        assert_eq!(
            session.retained_count(),
            0,
            "the preview outlived the mark it was drawing"
        );
    }

    /// The other half: taking back something the renderer has already drawn
    /// into the page is an absence no preview can express, so the picture is
    /// wrong until it is rendered again.
    #[test]
    fn undoing_a_mark_the_page_already_shows_asks_for_a_render() {
        let mut session = open(1);
        let undone = pulpit_render::document::Applied {
            effects: vec![pulpit_render::document::AppliedEffect::Deleted(
                stroke_at(100.0).id,
            )],
            ..applied(DocumentRevision(2))
        };
        assert_eq!(
            session.applied(&undone, AppliedKind::Undo),
            RasterUrgency::Prompt
        );
    }

    /// Deleting a mark that is still only a preview is the same case as
    /// undoing one, reached through the eraser instead of the undo button.
    #[test]
    fn erasing_a_mark_that_is_still_only_a_preview_costs_no_render() {
        use pulpit_core::annotate::AnnotationCommand;

        let mut session = open(1);
        let stroke = stroke_at(100.0);
        let id = stroke.id.clone();

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        let _ = session.retain_commit(&transaction);
        let _ = session.applied(
            &applied_naming(DocumentRevision(2), stroke),
            AppliedKind::Edit,
        );

        let erase = pulpit_render::document::DocumentTransaction::from_annotations([
            AnnotationCommand::Delete { id },
        ]);
        assert_eq!(session.retain_commit(&erase), RasterUrgency::Deferred);
        assert_eq!(session.retained_count(), 0);

        // …but erasing something that was already in the picture is not free.
        let other = pulpit_render::document::DocumentTransaction::from_annotations([
            AnnotationCommand::Delete {
                id: stroke_at(400.0).id,
            },
        ]);
        assert_eq!(session.retain_commit(&other), RasterUrgency::Prompt);
    }

    /// A mark the preview painter has no way to draw has to be rendered before
    /// the page is right, however cheap deferring would be.
    #[test]
    fn a_mark_no_preview_can_draw_asks_for_a_render() {
        use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, FreeTextDraft};

        let mut session = open(1);
        let draft = AnnotationDraft::FreeText(FreeTextDraft {
            page: PageIndex(0),
            rect: pulpit_core::page::PageRect::new(100.0, 100.0, 300.0, 140.0),
            text: "a thought".into(),
            source: pulpit_core::annotate::TextSource::Plain,
            style: pulpit_core::annotate::MarkStyle::default(),
        });
        let transaction = pulpit_render::document::DocumentTransaction::from_annotations([
            AnnotationCommand::Create(draft),
        ]);
        assert_eq!(session.retain_commit(&transaction), RasterUrgency::Prompt);
        assert_eq!(
            session.retained_count(),
            0,
            "nothing was drawn, so nothing is retained"
        );
    }

    /// A stroke retained on a page, ready for the patch tests. The stroke runs
    /// from (100, 100) to (200, 140) on a 612 × 792 page.
    fn session_with_a_retained_stroke() -> ReaderSession {
        let mut session = open(1);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        let _ = session.retain_commit(&transaction);
        let _ = session.applied(&applied(DocumentRevision(2)), AppliedKind::Edit);
        session
    }

    /// A partial repaint contains every mark the document holds inside its
    /// rectangle, so a preview wholly inside it has been drawn for real and
    /// comes down — the same rule a full frame follows.
    #[test]
    fn a_patch_takes_down_the_previews_it_contains() {
        let mut session = session_with_a_retained_stroke();
        assert_eq!(session.retained_count(), 1);

        // The top-left quarter of the page, which holds the whole stroke.
        let region = pulpit_core::notes::Region::new(0.0, 0.0, 0.5, 0.5);
        assert!(session.patch_landed(PageIndex(0), region, DocumentRevision(2)));
        assert_eq!(
            session.retained_count(),
            0,
            "the patch drew the mark the preview was standing in for"
        );
    }

    /// …but only if it contains the whole of it. A preview split by the
    /// rectangle's edge can be neither kept nor dropped without showing
    /// something wrong, so the patch is refused and the page waits.
    #[test]
    fn a_patch_that_splits_a_preview_is_refused() {
        let mut session = session_with_a_retained_stroke();

        // A band that cuts the stroke in half down its length.
        let region = pulpit_core::notes::Region::new(0.0, 0.0, 0.25, 1.0);
        assert!(
            !session.patch_landed(PageIndex(0), region, DocumentRevision(2)),
            "half a stroke inside the patch is not a usable patch"
        );
        assert_eq!(
            session.retained_count(),
            1,
            "a refused patch changes nothing"
        );
    }

    /// A patch older than the mark says nothing about it: the mark was
    /// committed after the patch was drawn, so the patch cannot contain it.
    #[test]
    fn a_patch_older_than_a_preview_leaves_it_alone() {
        let mut session = session_with_a_retained_stroke();
        let region = pulpit_core::notes::Region::new(0.0, 0.0, 0.5, 0.5);
        assert!(session.patch_landed(PageIndex(0), region, DocumentRevision(1)));
        assert_eq!(session.retained_count(), 1);
    }

    /// A patch on another page says nothing about this one's previews.
    #[test]
    fn a_patch_on_another_page_leaves_this_ones_previews_alone() {
        let mut session = open(2);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        let _ = session.retain_commit(&transaction);
        let _ = session.applied(&applied(DocumentRevision(2)), AppliedKind::Edit);

        let whole = pulpit_core::notes::Region::FULL;
        assert!(session.patch_landed(PageIndex(1), whole, DocumentRevision(2)));
        assert_eq!(session.retained_count(), 1);
    }

    #[test]
    fn the_toolbar_colours_and_width_reach_the_marks() {
        use pulpit_core::annotation::InkColor;

        let mut session = open(1);
        session.apply(&ReadCommand::SetToolColor(
            AnnotationTool::Ink,
            InkColor::Green,
        ));
        session.apply(&ReadCommand::SetInkWidth(5.0));
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 200.0, 140.0);
        let Released::Commit(transaction) = session.pointer_released() else {
            panic!("the stroke commits")
        };
        let pulpit_render::document::DocumentCommand::Annotation(
            pulpit_core::annotate::AnnotationCommand::Create(
                pulpit_core::annotate::AnnotationDraft::Ink(ink),
            ),
        ) = &transaction.0[0]
        else {
            panic!("one ink creation")
        };
        assert_eq!(ink.style.color, InkColor::Green);
        assert_eq!(ink.style.width, 5.0);
        assert_eq!(session.controls().ink_color, InkColor::Green);

        // The text colour is its own choice, not the pen's.
        session.apply(&ReadCommand::SetToolColor(
            AnnotationTool::Text,
            InkColor::Cyan,
        ));
        assert_eq!(session.controls().text_color, InkColor::Cyan);
        assert_eq!(
            session.interaction.text_style().color,
            InkColor::Cyan,
            "and reaches placed marks"
        );
        assert_eq!(session.interaction.ink_style().color, InkColor::Green);
    }

    #[test]
    fn arming_a_tool_closes_its_options() {
        let mut session = open(1);
        session.apply(&ReadCommand::ToolOptions(Some(AnnotationTool::Ink)));
        assert_eq!(session.controls().tool_options, Some(AnnotationTool::Ink));
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert_eq!(session.controls().tool_options, None);
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
            .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
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
            .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
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
        let facet = session.facet(true, &no_frames, &pulpit_core::search::SearchState::new());
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

        session.apply(&ReadCommand::Arm(None));
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
        session.apply(&ReadCommand::Arm(None));
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
        session.apply(&ReadCommand::Arm(None));
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

        session.apply(&ReadCommand::Arm(None));
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        assert!(!session.pointer_pressed(), "it must not start a move");
        assert_eq!(
            session.selected(),
            Some(&id),
            "it is still selected, so the reader can see what they hit"
        );
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    /// Drag a band over page zero, from one corner to the other.
    fn band(session: &mut ReaderSession, from: (f32, f32), to: (f32, f32)) {
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Select)));
        session.pointer_moved(PageIndex(0), from.0, from.1);
        assert!(session.pointer_pressed(), "the band opened");
        session.pointer_moved(PageIndex(0), to.0, to.1);
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_band_gathers_up_every_mark_it_encloses_and_leaves_the_rest() {
        // The whole point of the tool: one mark at a time is what the hand
        // does, and several at a time is what a band is for (§8.4).
        let mut session = open(2);
        session.set_annotations(
            PageIndex(0),
            &[stroke_at(100.0), stroke_at(200.0), stroke_at(600.0)],
        );

        band(&mut session, (20.0, 40.0), (500.0, 300.0));
        assert_eq!(
            session.selection().len(),
            2,
            "the two strokes inside it, and not the one below"
        );
    }

    #[test]
    fn a_band_that_only_clips_a_mark_does_not_take_it() {
        // Enclosure, not intersection: a band dragged across a page clips the
        // edge of everything it passes, and a selection made of those is one
        // nobody could aim.
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(200.0)]);
        band(&mut session, (20.0, 40.0), (200.0, 300.0));
        assert!(
            session.selection().is_empty(),
            "the stroke runs out of the right-hand side of the band"
        );
    }

    #[test]
    fn a_band_over_bare_page_puts_down_what_was_held() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(200.0)]);
        band(&mut session, (20.0, 40.0), (500.0, 300.0));
        assert_eq!(session.selection().len(), 1);

        band(&mut session, (20.0, 500.0), (500.0, 700.0));
        assert!(
            session.selection().is_empty(),
            "dragging over nothing says 'none of these'"
        );
    }

    #[test]
    fn deleting_a_band_of_marks_is_one_undo_entry() {
        // §9.1: one press of Delete is one thing the reader did, however many
        // marks it took.
        let mut session = open(2);
        session.set_annotations(
            PageIndex(0),
            &[stroke_at(100.0), stroke_at(200.0), stroke_at(600.0)],
        );
        band(&mut session, (20.0, 40.0), (500.0, 300.0));

        let transaction = session.delete_selected().expect("two marks were held");
        assert_eq!(transaction.len(), 2, "both of them, in one transaction");
        assert!(
            session.selection().is_empty(),
            "nothing is still held afterwards"
        );
    }

    #[test]
    fn a_band_holding_several_marks_offers_no_resize_grips() {
        // A corner belongs to the mark it is on; four sets of grips over a
        // band's worth of marks is four sets nobody could aim at.
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[text_box(), stroke_at(300.0)]);
        band(&mut session, (20.0, 40.0), (500.0, 400.0));

        let drawn = session.selection_for(PageIndex(0));
        assert_eq!(drawn.len(), 2, "both are outlined");
        assert!(
            drawn.iter().all(|mark| mark.handles.is_empty()),
            "and neither is offered a grip"
        );
    }

    #[test]
    fn a_mark_dragged_off_the_sheet_is_not_committed() {
        let mut session = open(2);
        session.set_annotations(PageIndex(0), &[stroke_at(200.0)]);
        session.apply(&ReadCommand::Arm(None));
        session.pointer_moved(PageIndex(0), 200.0, 200.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 99_000.0, 200.0);
        assert!(matches!(session.pointer_released(), Released::Nothing));
    }

    #[test]
    fn a_closed_reader_plans_nothing() {
        let mut session = ReaderSession::new();
        assert!(session.render_plan(1.0).is_empty());
    }

    #[test]
    fn a_document_that_cannot_be_annotated_says_so_to_the_toolbar() {
        let mut session = ReaderSession::new();
        session.opened(
            vec![PageGeometry::upright(612.0, 792.0)],
            CompatibilityLevel::Unsupported,
            vec![DocumentWarning::Encrypted],
        );
        assert!(!session
            .facet(true, &no_frames, &pulpit_core::search::SearchState::new())
            .annotatable());
    }

    /// The §13.6 budgets that belong to the session, measured rather than
    /// inferred.
    ///
    /// Three of that section's six bullets are claims about this type: that an
    /// unfinished stroke follows input without waiting for anything, that
    /// pointer-up hands a command over instead of doing the work, and that a
    /// frame handoff does not read the pixels. The remaining three are the
    /// engine's, and are measured in `pulpit-render`.
    ///
    /// The frame-handoff budget is a *ratio*, because "does it read the
    /// buffer" is a question about how cost grows and a ratio reads the same
    /// on a fast machine and a slow one. The other two are absolute, with
    /// thresholds set far above the baseline on the development machines: a
    /// regression threshold is for catching an order of magnitude, not for
    /// failing on a loaded runner. Each prints what it measured, so
    /// `cargo test -p pulpit budgets -- --nocapture` is how the baseline gets
    /// re-read.
    mod budgets {
        use super::*;
        use std::time::{Duration, Instant};

        fn drawing(pages: usize) -> ReaderSession {
            let mut session = open(pages);
            session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
            session
        }

        fn report(what: &str, measured: Duration, budget: Duration) {
            println!("  {what}: {measured:?} (budget {budget:?})");
            assert!(
                measured <= budget,
                "{what} took {measured:?}, over its {budget:?} budget"
            );
        }

        /// The points a transaction would commit, however many commands it is.
        fn ink_points(transaction: &pulpit_render::document::DocumentTransaction) -> usize {
            use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft};
            use pulpit_render::document::DocumentCommand;

            transaction
                .0
                .iter()
                .map(|command| match command {
                    DocumentCommand::Annotation(AnnotationCommand::Create(
                        AnnotationDraft::Ink(ink),
                    )) => ink.points.len(),
                    _ => 0,
                })
                .sum()
        }

        #[test]
        fn unfinished_ink_follows_input_without_waiting_for_anything() {
            // A pointer move during a stroke extends the ephemeral gesture and
            // does nothing else: no engine call, no render, no work that grows
            // with the document. A thousand moves inside one 60 Hz frame; if a
            // move ever starts waiting on the engine this is out by orders of
            // magnitude.
            let mut session = drawing(64);
            session.pointer_moved(PageIndex(0), 100.0, 100.0);
            assert!(session.pointer_pressed());

            let moves = 1_000;
            let start = Instant::now();
            for step in 0..moves {
                let along = step as f32 * 0.5;
                session.pointer_moved(PageIndex(0), 100.0 + along, 100.0 + along * 0.3);
            }
            let elapsed = start.elapsed();
            assert!(session.is_drawing(), "the stroke stopped part way");
            report(
                &format!("{moves} pointer moves during a stroke"),
                elapsed,
                Duration::from_millis(16),
            );
        }

        #[test]
        fn pointer_up_hands_over_a_command_rather_than_doing_the_work() {
            // `pointer_released` runs on the UI thread, so what it must do is
            // produce the transaction and return; the engine is on the far
            // side of a channel. A hundred complete strokes of a hundred
            // points each, in a few frames.
            let mut session = drawing(8);

            let strokes = 100;
            let start = Instant::now();
            let mut committed = 0;
            for stroke in 0..strokes {
                let base = (stroke % 8) as f32 * 40.0;
                session.pointer_moved(PageIndex(0), base, 50.0);
                assert!(session.pointer_pressed());
                for step in 0..100 {
                    session.pointer_moved(PageIndex(0), base + step as f32, 50.0 + step as f32);
                }
                if matches!(session.pointer_released(), Released::Commit(_)) {
                    committed += 1;
                }
            }
            let elapsed = start.elapsed();
            assert_eq!(committed, strokes, "a drawn stroke produced no command");
            report(
                &format!("{strokes} strokes drawn and handed over"),
                elapsed,
                Duration::from_millis(50),
            );
        }

        #[test]
        fn planning_renders_costs_the_window_not_the_document() {
            // The plan is recomputed every tick, so it must cost the pages in
            // the window and their margin — never a walk of the document.
            fn cost(pages: usize) -> Duration {
                let mut session = drawing(pages);
                session.set_cell(612.0, 400.0);
                let rounds = 2_000;
                let start = Instant::now();
                for _ in 0..rounds {
                    assert!(!session.render_plan(1.0).is_empty());
                }
                start.elapsed() / rounds
            }

            let short = cost(4);
            let long = cost(4_000);
            println!("  render plan over a 4-page document: {short:?}");
            println!("  render plan over a 4000-page document: {long:?}");
            assert!(
                long < short.max(Duration::from_nanos(200)) * 8,
                "planning a thousand-times longer document cost {long:?} \
                 against {short:?} — something walks every page"
            );
        }

        #[test]
        fn a_fast_pointer_does_not_produce_proportionally_more_traffic() {
            // A high-frequency pointer generates the most data of any input,
            // and what stops it flooding the audience preview and the
            // committed annotation is that samples closer together than the
            // minimum distance are dropped and the committed stroke is
            // simplified. Neither depends on how fast the input arrived, so
            // the same drawn line commits the same way at any sample rate.
            let mut session = drawing(4);
            session.pointer_moved(PageIndex(0), 100.0, 100.0);
            assert!(session.pointer_pressed());

            // A drawn wave rather than a straight line: a line simplifies to
            // its two ends whatever the sample rate, which would pass this
            // without proving anything. A wave has detail that must survive.
            let samples = 10_000;
            for step in 0..samples {
                let along = step as f32 * (400.0 / samples as f32);
                let across = (along * 0.2).sin() * 40.0;
                session.pointer_moved(PageIndex(0), 100.0 + along, 300.0 + across);
            }
            let Released::Commit(transaction) = session.pointer_released() else {
                panic!("the stroke produced no command")
            };

            let points = ink_points(&transaction);
            println!("  {samples} pointer samples became {points} committed points");
            assert!(points > 1, "the stroke was flattened to nothing");
            assert!(
                points < samples / 20,
                "{samples} samples produced {points} points — sampling and \
                 simplification are not bounding the traffic"
            );
        }
    }
}
