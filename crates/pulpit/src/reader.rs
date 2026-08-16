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
use crate::widgets::document::model::{
    Column, CropChoice, CropState, PageSpread, PlacedPage, ReaderControls, Zoom,
};
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

/// A calendar open over one date field.
#[derive(Debug, Clone, PartialEq)]
pub struct DatePicker {
    /// The field the chosen date goes into, by name — the same name a
    /// `SetField` names, so picking a date is an ordinary edit with an
    /// ordinary undo entry (§9.1).
    pub field: String,
    /// The Acrobat pattern to write the date in. Empty when the field's format
    /// script named a numbered preset instead, in which case an ISO date is
    /// written: PDFium's own keystroke script parses that whatever the pattern
    /// says, so the field is filled rather than left empty.
    pub pattern: String,
    pub page: PageIndex,
    /// The widget, in canonical page space, so the calendar opens beside the
    /// field rather than in the middle of the page.
    pub bounds: pulpit_core::page::PageRect,
    /// The month on show, which the reader can page through.
    pub month: crate::datefield::CalendarMonth,
    /// Today, so the calendar can mark it. Passed in rather than read here:
    /// this type is state, and state that reads a clock cannot be tested.
    pub today: crate::datefield::Date,
}

/// The hour and minute steppers pulpit draws over a time field (§8.6).
///
/// The calendar's counterpart, and the same bargain: pulpit chooses the
/// *text*, PDFium's editor still commits it and still runs the field's own
/// format script over it, so there is one implementation of what a value
/// looks like in a field and it is not this one.
#[derive(Debug, Clone, PartialEq)]
pub struct TimePicker {
    /// The field the chosen time goes into, by name — the same name a
    /// `SetField` names, so it is an ordinary edit with an ordinary undo.
    pub field: String,
    /// The Acrobat pattern to write the time in. Empty when the format script
    /// named a preset outside Acrobat's table, in which case a 24-hour
    /// `HH:MM` is written: PDFium's own keystroke script parses that whatever
    /// the pattern says, so the field is filled rather than left empty.
    pub pattern: String,
    pub page: PageIndex,
    /// The widget, in canonical page space, so the steppers open beside the
    /// field rather than in the middle of the page.
    pub bounds: pulpit_core::page::PageRect,
    /// The time on show, which the reader steps up and down.
    pub time: crate::datefield::TimeOfDay,
}

impl TimePicker {
    /// Whether the pattern's hour is a 12-hour one — `h`, as against `HH`.
    /// What the steppers show has to be what the field will show.
    pub fn twelve_hour(&self) -> bool {
        self.pattern.contains('h')
    }

    /// Whether the pattern carries an am/pm marker, which is the only case
    /// where a toggle for one is worth the room it takes.
    pub fn shows_meridiem(&self) -> bool {
        self.pattern.contains('t')
    }
}

/// The option list pulpit draws over a non-editable choice field (§8.6).
///
/// PDFium draws one of its own into the page bitmap, and that list is the
/// worst client of the partial-repaint path: it arrives as slivers, it has to
/// be guessed at to be composited whole, and every hovered row is a round trip
/// to a serial worker. It is also pure viewer chrome — no saved file contains
/// an open dropdown — so drawing it here breaks no rule: the option chosen is
/// still committed by `FORM_SetIndexSelected`, and the value and its
/// appearance are still PDFium's.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceList {
    /// The field whose list this is, so the caret moving to another field
    /// closes it rather than leaving it open over the wrong widget.
    pub field: String,
    pub page: PageIndex,
    /// The widget, in canonical page space, so the list opens against the
    /// field rather than in the middle of the page.
    pub bounds: pulpit_core::page::PageRect,
    pub options: Vec<String>,
    /// What the field holds now, as the worker last reported it. The first of
    /// [`Self::selections`] when several are chosen.
    pub selected: Option<u32>,
    /// Every option the field holds now, by index. One at most for a combo
    /// box; any number for a multi-select list box, whose rows are each ticked
    /// or not from this.
    pub selections: Vec<u32>,
    /// Whether the field takes several options at once (`/Ff` bit 22).
    ///
    /// It changes what a click means, which is why the list has to know: on a
    /// single-select field a click chooses and the list closes, and on a
    /// multi-select one a click toggles a row and the list stays open, because
    /// a list that shut after every tick could not be used to choose three
    /// things.
    pub multiple: bool,
    /// The row the arrow keys are on. Not a selection: nothing is committed
    /// until Enter or a click, so an arrow key cannot change the document by
    /// passing over an option.
    pub highlighted: u32,
}

impl ChoiceList {
    /// Whether the row at `index` is chosen in the document as it stands.
    pub fn is_selected(&self, index: u32) -> bool {
        if self.multiple {
            self.selections.contains(&index)
        } else {
            self.selected == Some(index)
        }
    }
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
    /// Whether this document has fields that can be filled at all (§8.6).
    ///
    /// False for a deck of slides, which is most of what pulpit opens, and
    /// false for a document whose form-fill environment could not be started.
    /// Nothing forwards a press to the worker's form when this is false: a
    /// click on a slide is not a click on a field, and a round trip per click
    /// on a serial worker queues ahead of the page renders.
    has_form: bool,
    /// The document's fields, as the engine last listed them (§6.4).
    ///
    /// A copy for drawing and for nothing else: the navigator reads it and the
    /// badges read it, and neither writes to it. It changes only when a fresh
    /// list arrives, so nothing here can disagree with PDFium for longer than
    /// the round trip after a commit.
    fields: Vec<pulpit_render::document::FormField>,
    /// Whether a field currently holds the caret, as the *worker* last said.
    ///
    /// Never guessed here. PDFium owns the caret — it decides whether a click
    /// landed in a field — so this is only ever set from a `FormEventResult`.
    /// It is what routes a letter to the field instead of to the shortcut that
    /// letter is bound to, which is the difference between typing a name and
    /// turning the page.
    form_typing: bool,
    /// The combo box holding the focus, when one does (§8.6).
    ///
    /// A closed combo box ignores an arrow key, so the application turns one
    /// into the selection change PDFium does answer to. Knowing which option
    /// is on and how many there are is what makes that possible without
    /// asking for the field list on every press.
    form_choice: Option<pulpit_render::document::protocol::FocusedChoice>,
    /// The option list open over a non-editable choice field, when one is.
    choice_list: Option<ChoiceList>,
    /// What the field holding the caret expects, so it is said once rather
    /// than once per keystroke.
    form_hint: Option<String>,
    /// The widget holding the focus, in canonical page space (§8.6).
    ///
    /// The focus ring is drawn from this rather than taken from the picture:
    /// PDFium's own decoration lives in the patches it invalidates, and a
    /// full frame from the render pool's fresh form environment has none, so
    /// a ring read off the bitmap would blink out at every frame swap (A2).
    form_widget: Option<pulpit_render::document::protocol::FocusedWidget>,
    /// The calendar open over a date field (§8.6).
    ///
    /// A PDF names a date field and its pattern and stops there; the calendar
    /// is the viewer's to draw, which is why Acrobat and PDF Studio each have
    /// one of their own.
    date_picker: Option<DatePicker>,
    /// The steppers open over a time field (§8.6). The calendar's argument one
    /// category along: the file names a time and its shape and offers no way
    /// to enter one.
    time_picker: Option<TimePicker>,
    /// The language a picked date is written in, set once from the
    /// environment. Held here so drawing the calendar needs no argument
    /// threaded through every view function that stands between.
    date_language: crate::datefield::Locale,
    /// What is being typed into the page box, while it is being typed.
    page_entry: Option<String>,
    dirty: bool,
    revision: DocumentRevision,
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
    /// Which history the two stacks belong to. Bumped whenever an ordinary
    /// edit makes a new future, which is what discards the redo stack. An
    /// undo or a redo that was refused may only be put back if this is still
    /// the epoch it was taken from; otherwise it belongs to a history the
    /// reader has left, and pushing it back would offer an action whose
    /// inverse no longer matches the document.
    history_epoch: u64,
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
    /// The marquee in flight: the page it started on, where it started, and
    /// where the pointer is now, in that page's own points.
    ///
    /// Here rather than on the controls because it changes with every pointer
    /// sample and the controls are diffed on every view pass; the rectangle
    /// the reader is asked about *is* on the controls, because by then it has
    /// stopped moving.
    marquee: Option<(PageIndex, PagePoint, PagePoint)>,
    /// Which page the rectangle being chosen about was drawn on, so it is
    /// still drawn where it was drawn while the question is being answered.
    marquee_page: Option<PageIndex>,
    /// Where the reader was before a crop took hold, restored when it is
    /// cleared.
    ///
    /// A crop rewrites the zoom and both offsets, and none of the three is
    /// recoverable from the cropped state — so clearing one without this
    /// would put the reader back at a different place in the document from
    /// the one they cropped at.
    crop_restore: Option<(Zoom, f32, f32)>,
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlannedRender {
    pub page: PageIndex,
    pub width: u32,
    pub height: u32,
    pub quality: pulpit_render::protocol::Quality,
    /// On screen right now, as opposed to warming in the margin.
    pub visible: bool,
    /// Which part of the page to draw: the whole of it, or the crop window
    /// the reader is reading through.
    ///
    /// Asked of the renderer rather than cut out of a finished frame, so the
    /// pixels are spent on what is on the sheet instead of on margins that
    /// are not drawn.
    pub region: pulpit_core::notes::Region,
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
        has_form: bool,
    ) {
        *self = ReaderSession {
            open: true,
            pages,
            level,
            warnings,
            has_form,
            ..ReaderSession::new()
        };
        self.relayout();
    }

    /// Whether this document has fields worth forwarding a press to.
    pub fn has_form(&self) -> bool {
        self.has_form
    }

    /// Take the engine's field list as it stands.
    pub fn set_fields(&mut self, fields: Vec<pulpit_render::document::FormField>) {
        self.fields = fields;
    }

    /// The widgets on `page` that are drawn but can never be filled (§6.4).
    ///
    /// Signature fields and file-selection fields are the two the worker
    /// refuses outright, so they are the two a reader would otherwise click at
    /// and get nothing from. Everything else is either fillable or read-only,
    /// and a read-only field that looks like a printed value is not a fault.
    fn dead_fields_on(&self, page: PageIndex) -> Vec<crate::widgets::context::DeadField> {
        use pulpit_render::document::FieldKind;

        if !self.has_form {
            return Vec::new();
        }
        self.fields
            .iter()
            .filter_map(|field| {
                let label = if field.kind == FieldKind::Signature {
                    "signature — not supported"
                } else if field.file_select {
                    "file field — not fillable"
                } else {
                    return None;
                };
                Some(
                    field
                        .widgets
                        .iter()
                        .filter(move |widget| widget.page == page)
                        .map(move |widget| crate::widgets::context::DeadField {
                            bounds: widget.bounds,
                            label,
                        }),
                )
            })
            .flatten()
            .collect()
    }

    /// Where a named field is drawn, for the jump the navigator makes.
    /// `None` for a field the producer placed nowhere.
    pub fn field_page(&self, name: &str) -> Option<PageIndex> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .and_then(|field| field.widgets.first())
            .map(|widget| widget.page)
    }

    /// The fields Tab travels between, in the order a reader meets them.
    ///
    /// Reading order — page, then down the page, then across it — rather than
    /// the order the file lists its fields in. `/Fields` is an array a
    /// generator writes in whatever order it built the form, and tabbing
    /// through *that* is tabbing at random; a form filled by hand is filled
    /// top to bottom.
    ///
    /// A field with no widget is left out because there is nowhere to put the
    /// caret, and one that cannot be filled is left out because arriving in it
    /// is arriving nowhere: a signature or file-select field is refused by the
    /// worker, and a read-only field is a printed value (§6.4).
    fn fillable_fields(&self) -> Vec<(PageIndex, &str)> {
        if !self.has_form {
            return Vec::new();
        }
        let mut ordered: Vec<(PageIndex, pulpit_core::page::PageRect, &str)> = self
            .fields
            .iter()
            .filter(|field| field.is_editable())
            .filter_map(|field| {
                let widget = field.widgets.first()?;
                Some((widget.page, widget.bounds, field.name.as_str()))
            })
            .collect();
        ordered.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.top.total_cmp(&right.1.top))
                .then(left.1.left.total_cmp(&right.1.left))
        });
        ordered
            .into_iter()
            .map(|(page, _, name)| (page, name))
            .collect()
    }

    /// Where Tab, or Shift-Tab, puts the caret next.
    ///
    /// Wrapping, because a form has no end to fall off: the field after the
    /// last one is the first one, which is what every form in a browser does.
    ///
    /// With nothing focused the walk starts at the first field on or after the
    /// page in view, so the first Tab picks up where the reader is *looking*
    /// rather than at the top of a form they have already scrolled past.
    /// `None` when the document has no field worth reaching.
    pub fn field_to_focus(&self, forward: bool) -> Option<(PageIndex, String)> {
        let ordered = self.fillable_fields();
        if ordered.is_empty() {
            return None;
        }
        // Where the caret is now. The focused *widget* rather than the typing
        // flag, because a checkbox holds the focus without holding a caret and
        // Tab has to leave it just the same.
        let held = self.form_widget.as_ref().and_then(|widget| {
            ordered
                .iter()
                .position(|(_, name)| *name == widget.field.as_str())
        });
        let index = match held {
            Some(index) if forward => (index + 1) % ordered.len(),
            Some(index) => (index + ordered.len() - 1) % ordered.len(),
            None => {
                let page = self.current_page().unwrap_or(PageIndex(0));
                let from = ordered.iter().position(|(at, _)| *at >= page).unwrap_or(0);
                if forward {
                    from
                } else {
                    (from + ordered.len() - 1) % ordered.len()
                }
            }
        };
        let (page, name) = ordered[index];
        Some((page, name.to_string()))
    }

    /// Put a named field's widget on screen, not merely the page it sits on.
    ///
    /// A page jump lands at the top of the page, and for a field near the foot
    /// of a tall one that is a jump to somewhere the field is not: a Tab whose
    /// destination is off screen is a Tab that appears to have done nothing.
    /// The widget is placed a little above centre, so the field and the lines
    /// under it are both in view.
    ///
    /// `false` — and the position untouched — for a field the reader has no
    /// geometry for, where the page jump on its own is still the best answer.
    pub fn reveal_field(&mut self, name: &str) -> bool {
        let Some(widget) = self
            .fields
            .iter()
            .find(|field| field.name == name)
            .and_then(|field| field.widgets.first())
            .map(|widget| (widget.page, widget.bounds))
        else {
            return false;
        };
        let (page, bounds) = widget;
        let Some(geometry) = self.pages.get(page.get()) else {
            return false;
        };
        if geometry.height <= 0.0 {
            return false;
        }
        // A third of a window above the field rather than none, so it does not
        // arrive pinned against the top edge with its own label off screen.
        let above = self.cell.1 / 3.0 / self.scale.max(f32::EPSILON);
        let fraction = ((bounds.top - above) / geometry.height).clamp(0.0, 1.0);
        self.restore_position(page, None, fraction);
        true
    }

    /// What kind of field holds the focus, when one does.
    ///
    /// Space means "toggle" in a box and "a space character" in a text field,
    /// and only the field's kind tells the two apart.
    pub fn focused_field_kind(&self) -> Option<pulpit_render::document::FieldKind> {
        let widget = self.form_widget.as_ref()?;
        self.fields
            .iter()
            .find(|field| field.name == widget.field && field.is_editable())
            .map(|field| field.kind)
    }

    /// Whether a *text* caret sits in a field — the one state in which the
    /// clipboard shortcuts mean what they mean in a text box.
    ///
    /// Narrower than [`Self::form_has_keyboard`], which is also true for a
    /// closed combo box holding the focus: there is no selection to copy out
    /// of one of those and nowhere for a paste to land.
    pub fn form_holds_the_caret(&self) -> bool {
        self.has_form && self.form_typing
    }

    /// Whether the keyboard belongs to a field rather than to the toolbar.
    ///
    /// Two ways in, because PDFium reports them differently. A text field
    /// announces itself through `FFI_SetTextFieldFocus`, which is what
    /// `form_typing` records. A combo box may not — it is not a text field —
    /// so a focused one is evidence in its own right. Missing that second case
    /// meant the arrow keys never reached a combo box at all: they were still
    /// the toolbar's, and scrolled the page.
    pub fn form_has_keyboard(&self) -> bool {
        self.has_form && (self.form_typing || self.form_choice.is_some())
    }

    /// Take the worker's word for where the caret is.
    pub fn set_form_typing(&mut self, typing: bool) {
        self.form_typing = typing;
    }

    /// The calendar open over a date field, if one is.
    pub fn date_picker(&self) -> Option<&DatePicker> {
        self.date_picker.as_ref()
    }

    /// Open, move or close the calendar as the caret moves between fields.
    ///
    /// Opening is not the same as re-opening: a caret that stays in the field
    /// it was already in must not throw away the month the reader navigated
    /// to, or paging back to last December would be undone by the next
    /// keystroke.
    pub fn set_focused_date(
        &mut self,
        focused: Option<&pulpit_render::document::protocol::FocusedDate>,
        today: crate::datefield::Date,
    ) {
        let Some(focused) = focused else {
            self.date_picker = None;
            return;
        };
        if self
            .date_picker
            .as_ref()
            .is_some_and(|open| open.field == focused.field)
        {
            return;
        }
        self.date_picker = Some(DatePicker {
            field: focused.field.clone(),
            pattern: focused.pattern.clone(),
            page: focused.page,
            bounds: focused.bounds,
            month: crate::datefield::CalendarMonth::of(today),
            today,
        });
    }

    /// Step the open calendar a month forward or back.
    pub fn step_date_picker(&mut self, forward: bool) {
        if let Some(picker) = self.date_picker.as_mut() {
            picker.month = if forward {
                picker.month.next()
            } else {
                picker.month.previous()
            };
        }
    }

    /// Set the language dates are written in. Called once, at startup.
    pub fn set_date_language(&mut self, language: crate::datefield::Locale) {
        self.date_language = language;
    }

    pub fn close_date_picker(&mut self) {
        self.date_picker = None;
    }

    /// The steppers open over a time field, if any are.
    pub fn time_picker(&self) -> Option<&TimePicker> {
        self.time_picker.as_ref()
    }

    /// Open, move or close the time steppers as the caret moves between
    /// fields.
    ///
    /// Opening is not re-opening, exactly as for the calendar: a caret that
    /// stays in the field it was already in must not throw away the time the
    /// reader has stepped to, or one keystroke would undo a dozen presses.
    ///
    /// They open on the time the field already holds when that can be read,
    /// and on `now` when it cannot — the wall clock being the answer a time
    /// field is usually asking for. `now` is passed in rather than read here,
    /// because state that reads a clock cannot be tested.
    pub fn set_focused_time(
        &mut self,
        focused: Option<&pulpit_render::document::protocol::FocusedTime>,
        now: crate::datefield::TimeOfDay,
    ) {
        let Some(focused) = focused else {
            self.time_picker = None;
            return;
        };
        if self
            .time_picker
            .as_ref()
            .is_some_and(|open| open.field == focused.field)
        {
            return;
        }
        self.time_picker = Some(TimePicker {
            field: focused.field.clone(),
            pattern: focused.pattern.clone(),
            page: focused.page,
            bounds: focused.bounds,
            time: crate::datefield::TimeOfDay::parse(&focused.value).unwrap_or(now),
        });
    }

    /// Step the open time helper by `minutes`, in either direction.
    pub fn step_time_picker(&mut self, minutes: i32) {
        if let Some(picker) = self.time_picker.as_mut() {
            picker.time = picker.time.stepped(minutes);
        }
    }

    pub fn close_time_picker(&mut self) {
        self.time_picker = None;
    }

    /// Whether `hint` is new, and record it if it is.
    ///
    /// Every form event carries the focused field's hint, including the ones
    /// that changed nothing — so a date field would announce itself once per
    /// keystroke. This says yes the first time the caret is in a field that
    /// wants something, and no until the caret moves to a different one.
    pub fn take_form_hint(&mut self, hint: Option<&str>) -> bool {
        if self.form_hint.as_deref() == hint {
            return false;
        }
        self.form_hint = hint.map(str::to_owned);
        hint.is_some()
    }

    /// The widget the focus ring belongs on, as the worker last reported it.
    pub fn focused_widget(&self) -> Option<&pulpit_render::document::protocol::FocusedWidget> {
        self.form_widget.as_ref()
    }

    /// Take the worker's word for where the focus is, including the answer
    /// that says nowhere: a click on bare page takes the ring off the field
    /// it was on, and that has to reach the view.
    pub fn set_focused_widget(
        &mut self,
        widget: Option<pulpit_render::document::protocol::FocusedWidget>,
    ) {
        self.form_widget = widget;
    }

    /// What the field holding the focus expects, for the tooltip beside it.
    pub fn form_hint(&self) -> Option<&str> {
        self.form_hint.as_deref()
    }

    /// The choice field holding the focus, as the worker last reported it.
    pub fn focused_choice(&self) -> Option<&pulpit_render::document::protocol::FocusedChoice> {
        self.form_choice.as_ref()
    }

    /// Take the worker's word for which choice field has the focus, and keep
    /// any open list in step with it.
    ///
    /// A list open over a field the caret has left is a list over the wrong
    /// widget, so it closes. A list over the field that is still focused stays
    /// open and takes the new selection: committing an option answers with the
    /// field as it now is, and a list that ignored that answer would go on
    /// ticking the option that was chosen before.
    pub fn set_focused_choice(
        &mut self,
        choice: Option<pulpit_render::document::protocol::FocusedChoice>,
    ) {
        match (&choice, &mut self.choice_list) {
            (Some(choice), Some(open)) if open.field == choice.field => {
                open.page = choice.page;
                open.bounds = choice.bounds;
                open.options = choice.labels.clone();
                open.selected = choice.selected;
                open.selections = choice.selections.clone();
                open.multiple = choice.multiple_selection;
                open.highlighted = open
                    .highlighted
                    .min(open.options.len().saturating_sub(1) as u32);
            }
            (_, list) => *list = None,
        }
        self.form_choice = choice;
    }

    /// The option list open over a choice field, if one is.
    pub fn choice_list(&self) -> Option<&ChoiceList> {
        self.choice_list.as_ref()
    }

    /// Open the list for the field that just took the focus, if its list is
    /// pulpit's to draw.
    ///
    /// Data-driven: an *editable* combo box keeps PDFium's own list, because
    /// it has a caret PDFium is drawing and two editing surfaces for one field
    /// is what §8.6 exists to prevent. A field with no options opens nothing —
    /// an empty panel says less than the field it would cover.
    pub fn open_choice_list(&mut self) {
        let Some(choice) = self.form_choice.as_ref() else {
            return;
        };
        if choice.editable || choice.labels.is_empty() {
            self.choice_list = None;
            return;
        }
        self.choice_list = Some(ChoiceList {
            field: choice.field.clone(),
            page: choice.page,
            bounds: choice.bounds,
            options: choice.labels.clone(),
            selected: choice.selected,
            selections: choice.selections.clone(),
            multiple: choice.multiple_selection,
            // Opens on what the field holds, so Enter on a list nobody moved
            // chooses what was already chosen rather than the first row.
            highlighted: choice.selected.unwrap_or(0),
        });
    }

    pub fn close_choice_list(&mut self) {
        self.choice_list = None;
    }

    /// Move the highlighted row of an open list, and say whether anything
    /// moved.
    ///
    /// Stopping at the ends rather than wrapping, for the reason
    /// [`ReaderSession::choice_step`] gives: a list that jumped from its last
    /// row back to its first is a way to choose the wrong option.
    pub fn step_choice_list(&mut self, forward: bool) -> bool {
        let Some(open) = self.choice_list.as_mut() else {
            return false;
        };
        let last = match open.options.len() {
            0 => return false,
            length => length as u32 - 1,
        };
        let moved = if forward {
            open.highlighted.min(last).saturating_add(1).min(last)
        } else {
            open.highlighted.min(last).saturating_sub(1)
        };
        let changed = moved != open.highlighted;
        open.highlighted = moved;
        changed
    }

    /// Whether the open list belongs to a field that takes several options.
    pub fn choice_list_is_multiple(&self) -> bool {
        self.choice_list.as_ref().is_some_and(|open| open.multiple)
    }

    /// The row an open list would commit, and close it. `None` when no list is
    /// open or it has no rows.
    pub fn take_highlighted_option(&mut self) -> Option<u32> {
        let open = self.choice_list.take()?;
        (open.highlighted < open.options.len() as u32).then_some(open.highlighted)
    }

    /// What toggling one row of a multi-select list box asks the engine for:
    /// the index, and whether it should end up chosen.
    ///
    /// The list is deliberately *not* closed and the local selection is
    /// deliberately *not* changed here. The worker's answer re-reports the
    /// field, and [`ReaderSession::set_focused_choice`] takes the new
    /// selection from it, so what the rows show is always what PDFium holds
    /// rather than what this layer hoped it would hold — the same rule the
    /// single-select path follows (§8.6).
    ///
    /// `None` when there is no open list, when the row is not one of its rows,
    /// or when the field is single-select — where a row is chosen, not
    /// toggled, and the caller wants [`ReaderSession::pick_option`] instead.
    pub fn toggle_option(&mut self, index: u32) -> Option<(u32, bool)> {
        let open = self.choice_list.as_ref()?;
        if !open.multiple || index >= open.options.len() as u32 {
            return None;
        }
        let wanted = !open.selections.contains(&index);
        // The highlight follows the row that was pressed, so a click and then
        // Space carries on from where the pointer left off rather than jumping
        // back to wherever the arrow keys had been.
        if let Some(open) = self.choice_list.as_mut() {
            open.highlighted = index;
        }
        Some((index, wanted))
    }

    /// The same, for the row the highlight is on — what Space does.
    pub fn toggle_highlighted_option(&mut self) -> Option<(u32, bool)> {
        let index = self.choice_list.as_ref()?.highlighted;
        self.toggle_option(index)
    }

    /// Which option an arrow key should move a focused combo box to.
    ///
    /// `None` when there is nowhere to go — no combo focused, no options, or
    /// already at the end. Stopping at the ends rather than wrapping is what
    /// every native combo box does, and a list that silently jumped from the
    /// last entry back to the first would be a way to pick the wrong one.
    pub fn choice_step(&self, forward: bool) -> Option<u32> {
        let choice = self.form_choice.as_ref()?;
        if choice.options == 0 {
            return None;
        }
        match choice.selected {
            Some(index) if forward => (index + 1 < choice.options).then_some(index + 1),
            Some(index) => index.checked_sub(1),
            // Nothing chosen yet — which happens for a combo holding a value
            // that is not in its own option list. Either arrow starts at the
            // near end rather than doing nothing.
            None if forward => Some(0),
            None => Some(choice.options - 1),
        }
    }

    /// A field value was committed, at the revision the worker stamped it.
    ///
    /// Not routed through [`ReaderSession::applied`], because a form commit
    /// carries no `Applied`: PDFium owns the edit and hands back a revision
    /// rather than an inverse. The document has still moved and is still
    /// unsaved, which is what this records.
    ///
    /// The undo entry is built here from the field's before-image, and it goes
    /// on the same stack as the annotations in the same order, which is what
    /// §8.6 asks for: a field edit followed by an ink stroke undoes the stroke
    /// first. Like any ordinary edit it makes a new future, so the redo stack
    /// is discarded and the epoch moves.
    pub fn field_committed(
        &mut self,
        committed: &pulpit_render::document::protocol::CommittedField,
    ) {
        use pulpit_render::document::{DocumentUndo, UndoOperation};

        let restores = self.revision;
        self.revision = committed.revision;
        self.dirty = true;
        // A commit the engine could not name has no inverse that could be
        // applied — putting a value back needs a field to put it in. The
        // revision still moves, because the document did.
        if committed.name.is_empty() {
            return;
        }
        self.undo_stack.push(DocumentUndo {
            operations: vec![UndoOperation::SetField {
                name: committed.name.clone(),
                value: committed.previous.clone(),
                selected: committed.previous_selected.clone(),
            }],
            restores,
            label: format!("Fill {}", committed.name),
        });
        self.redo_stack.clear();
        self.history_epoch = self.history_epoch.wrapping_add(1);
    }

    /// Where the pointer last was, on which page.
    pub fn cursor_position(&self) -> Option<(PageIndex, PagePoint)> {
        self.cursor
    }

    /// Where the reader is, in terms that outlive this window and this zoom.
    ///
    /// The page, the zoom, and how far down that page the window sits as a
    /// fraction of the page's own height. `None` before the column has been
    /// laid out against a real cell, when the offset is a number about a
    /// window that never existed.
    pub fn reading_position(&self) -> Option<(PageIndex, Zoom, f32)> {
        if !self.cell_known || self.pages.is_empty() {
            return None;
        }
        let placed = self
            .column
            .pages
            .iter()
            .find(|placed| placed.page == self.controls.page)?;
        // A page of no height is not a page anybody is a fraction of the way
        // down; the top of it is the honest answer.
        let fraction = if placed.height > 0.0 {
            ((self.controls.offset - placed.top) / placed.height).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some((self.controls.page, self.controls.zoom, fraction))
    }

    /// Put the reader back where [`ReaderSession::reading_position`] said it
    /// was.
    ///
    /// `zoom` is optional because a position recovered by path alone carries
    /// only a page number: the caller decides how much of the record it
    /// believes, and this applies exactly what it is given.
    ///
    /// The page is clamped to the document that actually opened, so a record
    /// from a longer draft lands on the last page rather than nowhere.
    pub fn restore_position(&mut self, page: PageIndex, zoom: Option<Zoom>, fraction: f32) {
        if self.pages.is_empty() {
            return;
        }
        let page = PageIndex(page.get().min(self.pages.len() - 1));
        // Before the zoom, not after: a fit is fitted to the page the reader
        // is on, and `set_zoom` holds whatever page that is under the window.
        // Zooming first would fit the page the document opened on and then
        // move away from it.
        self.controls.page = page;
        if let Some(zoom) = zoom {
            self.set_zoom(zoom);
        }
        // After the zoom, because the column's geometry depends on it and the
        // fraction is a fraction of the page as laid out at that zoom.
        let Some(placed) = self
            .column
            .pages
            .iter()
            .find(|placed| placed.page == page)
            .copied()
        else {
            return;
        };
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.controls.offset = self
            .column
            .clamp_offset(placed.top + placed.height * fraction, self.cell.1);
    }

    /// The page a keystroke belongs to when the pointer has wandered off the
    /// surface: the first one on screen.
    pub fn current_page(&self) -> Option<PageIndex> {
        self.visible_pages().first().map(|placed| placed.page)
    }

    /// Whether a press on the page belongs to the document's own fields.
    ///
    /// Only with nothing armed. An armed tool draws on top of a form exactly
    /// as it draws on top of anything else — a reader who picked up the pen
    /// means to write on the page, not into a field (§8.4).
    pub fn press_belongs_to_the_form(&self) -> bool {
        self.has_form && self.controls.tool.is_none()
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
                self.history_epoch = self.history_epoch.wrapping_add(1);
            }
            AppliedKind::Undo => self.redo_stack.push(applied.undo.clone()),
            AppliedKind::Redo => self.undo_stack.push(applied.undo.clone()),
        }

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
    /// A canonical page point as it stands on the *turned* page: the view
    /// rotation applied, still in the page's own points.
    ///
    /// The one direction the session converts in for display; its inverse is
    /// taken once, where pointer positions come in ([`Self::pointer_moved`]).
    fn view_point(&self, page: PageIndex, at: PagePoint) -> PagePoint {
        let Some(geometry) = self.pages.get(page.get()) else {
            return at;
        };
        self.controls
            .rotation
            .rotate_point(at, geometry.width, geometry.height)
    }

    /// Where the drawn sheet's top-left corner is on the *turned* page, in
    /// that page's own points.
    ///
    /// The origin of the crop window as the reader sees it — turned with the
    /// page, or the page's own origin when there is no crop. Everything that
    /// converts between a page point and a place on the sheet goes through
    /// this: without it a crop would leave every pan, every mark and every
    /// hit test off by the margin it trimmed.
    fn crop_origin(&self, page: PageIndex) -> (f32, f32) {
        let window = crate::widgets::document::model::rotated_region(
            self.controls.crop.window(),
            self.controls.rotation,
        );
        let Some(geometry) = self.pages.get(page.get()) else {
            return (0.0, 0.0);
        };
        let turned =
            crate::widgets::document::model::view_rotated(geometry, self.controls.rotation);
        (turned.width * window.x, turned.height * window.y)
    }

    /// The whole grabbed point, when the column can place it: a canonical
    /// (upright) page point's place in the laid-out column, in layout points.
    fn document_point(&self, page: PageIndex, x: f32, y: f32) -> Option<(f32, f32)> {
        let at = self.view_point(page, PagePoint::new(x, y));
        let origin = self.crop_origin(page);
        Some((
            self.column.left_of(page)? + (at.x - origin.0) * self.scale,
            self.column.offset_of(page)? + (at.y - origin.1) * self.scale,
        ))
    }

    /// Is the hand dragging the page about? The cursor says so while it is.
    pub fn is_panning(&self) -> bool {
        self.pan.is_some()
    }

    /// The pointer moved over a page, at a canonical page point (A4).
    ///
    /// The sheet reports the point on the page *as drawn*, which under a view
    /// rotation is the turned page. Everything in this session speaks the
    /// upright canonical space the document's own geometry is written in, so
    /// the rotation is undone here — once, at the boundary — and nothing
    /// below this line knows the page was ever turned.
    pub fn pointer_moved(&mut self, page: PageIndex, x: f32, y: f32) {
        let (x, y) = match self.pages.get(page.get()) {
            Some(geometry) => {
                let upright = self.controls.rotation.unrotate_point(
                    PagePoint::new(x, y),
                    geometry.width,
                    geometry.height,
                );
                (upright.x, upright.y)
            }
            None => (x, y),
        };
        // The marquee owns the pointer while it is armed: the far corner
        // follows the hand, and nothing else on the page hears about it.
        if self.controls.crop.takes_the_pointer() {
            if let Some((started_on, _, corner)) = self.marquee.as_mut() {
                // Clamped to the page the drag started on. A rectangle that
                // ran onto the next sheet would describe a region that exists
                // on neither.
                if *started_on == page {
                    *corner = PagePoint::new(x, y);
                }
            }
            self.cursor = Some((page, PagePoint::new(x, y)));
            return;
        }
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
        // A press with the marquee armed starts a rectangle and reaches
        // nothing else — not a tool, not a link, not a form field. A press
        // while one is being chosen about abandons that choice and starts
        // over, so redrawing a mis-drawn rectangle is one gesture rather than
        // a dismissal followed by a gesture.
        if self.controls.crop.takes_the_pointer() {
            self.controls.crop = CropState::Armed;
            self.marquee = Some((page, at, at));
            return true;
        }
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
        // A marquee comes up as a question rather than as an edit: the
        // rectangle is frozen and the reader is asked what it means. Nothing
        // is committed either way — a crop is a zoom control (§8.1) and never
        // touches the document.
        if self.controls.crop.takes_the_pointer() {
            self.finish_marquee();
            return Released::Nothing;
        }
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
        // The pointer leaving the sheet mid-drag ends the marquee where it
        // was, rather than dropping it: a rectangle dragged off the bottom of
        // a figure is the rectangle the reader meant, and losing it at the
        // page edge would make the tool unusable for anything that reaches
        // one.
        if self.marquee.is_some() {
            self.finish_marquee();
        }
        self.interaction.cancel();
        self.cursor = None;
        self.transform_origin = None;
        self.pan = None;
    }

    /// Take the drawn rectangle as far as a proposal: frozen on the page, and
    /// waiting to be told what it means.
    ///
    /// A rectangle too small to read a page through is a click rather than a
    /// drag, and leaves the tool armed for the drag that was meant.
    fn finish_marquee(&mut self) {
        let Some((page, start, end)) = self.marquee.take() else {
            return;
        };
        let Some(geometry) = self.pages.get(page.get()).copied() else {
            self.controls.crop = CropState::Armed;
            return;
        };
        let region = crate::widgets::document::model::crop_between(
            (start.x, start.y),
            (end.x, end.y),
            &geometry,
        );
        if crate::widgets::document::model::is_usable_crop(&region) {
            self.controls.crop = CropState::Choosing(region);
            self.marquee_page = Some(page);
        } else {
            self.controls.crop = CropState::Armed;
            self.marquee_page = None;
        }
    }

    /// The marquee as the surface draws it: the page it is on and the
    /// rectangle it covers, in that page's own points.
    ///
    /// The open drag first, and the frozen proposal after it — the reader
    /// being asked about a rectangle must still be able to see the rectangle.
    pub fn marquee(&self) -> Option<(PageIndex, pulpit_core::page::PageRect)> {
        if let Some((page, start, end)) = self.marquee {
            return Some((
                page,
                pulpit_core::page::PageRect::new(
                    start.x.min(end.x),
                    start.y.min(end.y),
                    start.x.max(end.x),
                    start.y.max(end.y),
                ),
            ));
        }
        let CropState::Choosing(region) = self.controls.crop else {
            return None;
        };
        let page = self.marquee_page?;
        let geometry = self.pages.get(page.get())?;
        Some((
            page,
            pulpit_core::page::PageRect::new(
                region.x * geometry.width,
                region.y * geometry.height,
                (region.x + region.width) * geometry.width,
                (region.y + region.height) * geometry.height,
            ),
        ))
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
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Which history the stacks currently belong to (see `history_epoch`).
    pub fn history_epoch(&self) -> u64 {
        self.history_epoch
    }

    /// The operation that undoes the last edit, if there is one.
    pub fn undo_operation(&mut self) -> Option<pulpit_render::document::DocumentUndo> {
        self.undo_stack.pop()
    }

    /// …and the one that redoes it.
    pub fn redo_operation(&mut self) -> Option<pulpit_render::document::DocumentUndo> {
        self.redo_stack.pop()
    }

    /// Put back an undo or a redo the worker refused.
    ///
    /// The operation was taken off its stack when the request was sent, so a
    /// refusal that left it there would lose one step of history for good.
    /// `epoch` is the one the operation was taken in: if an ordinary edit has
    /// landed since, it made a new future and this operation belongs to the
    /// history that edit replaced, so it is dropped rather than put back.
    pub fn restore_operation(
        &mut self,
        kind: AppliedKind,
        epoch: u64,
        operation: pulpit_render::document::DocumentUndo,
    ) {
        if epoch != self.history_epoch {
            return;
        }
        match kind {
            AppliedKind::Undo => self.undo_stack.push(operation),
            AppliedKind::Redo => self.redo_stack.push(operation),
            AppliedKind::Edit => {}
        }
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
        // One window for every page: a crop is a statement about margins, and
        // margins are the same fraction of a letter page and of the A4
        // appendix behind it.
        let window = self.controls.crop.window();
        for (placed, visible) in on_screen
            .into_iter()
            .map(|placed| (placed, true))
            .chain(margin.into_iter().map(|placed| (placed, false)))
        {
            // Frames are always rasterised *upright*: the view rotation is a
            // way of drawing them, not a second kind of frame, which is what
            // lets the cache serve every rotation from one picture. Under a
            // quarter turn the placed sheet's width is the upright page's
            // height, so the request swaps back before it is sized.
            let (upright_width, upright_height) = if self.controls.rotation.swaps_axes() {
                (placed.height, placed.width)
            } else {
                (placed.width, placed.height)
            };
            let full = renderable_size(upright_width * scale, upright_height * scale);
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
                region: window,
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
                    region: window,
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
        // Every page as it is read through the crop, which is every page
        // itself until one is taken. The fit fits what is on the sheet: the
        // point of trimming margins is that the text then fills the window.
        let window = self.controls.crop.window();
        // …and then as the reader has turned it: the column is laid out from
        // the pages as they are looked at, and a turned portrait page is
        // landscape for every question the layout answers.
        let pages: Vec<PageGeometry> = self
            .pages
            .iter()
            .map(|page| {
                crate::widgets::document::model::view_rotated(
                    &crate::widgets::document::model::cropped(page, window),
                    self.controls.rotation,
                )
            })
            .collect();
        let reference = pages
            .get(self.controls.page.get())
            .or_else(|| pages.first())
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
        self.column = Column::lay_out(&pages, self.scale, self.cell.0, self.controls.spread);
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
            // Handled above the session: it becomes an ordinary `GoToPage`
            // plus a focus request, and moving the caret is the worker's.
            ReadCommand::GoToField { .. } => false,
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
            ReadCommand::RotateView => {
                self.controls.rotation = self.controls.rotation.next();
                // The page the reader is on stays the page they are on, the
                // same promise a spread change makes: the turned column is
                // not the column they were scrolled down, so the offset is
                // recovered from the page rather than kept as points.
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
            ReadCommand::StepDatePicker(forward) => {
                self.step_date_picker(*forward);
                false
            }
            ReadCommand::CloseDatePicker => {
                self.close_date_picker();
                false
            }
            ReadCommand::StepTimePicker(minutes) => {
                self.step_time_picker(*minutes);
                false
            }
            ReadCommand::CloseTimePicker => {
                self.close_time_picker();
                false
            }
            // The pick itself is a document edit and belongs to the layer that
            // can post one; nothing about the viewport changes here. So is
            // choosing an option, which crosses to PDFium as a `SelectOption`.
            ReadCommand::PickDate(_)
            | ReadCommand::PickTime
            | ReadCommand::PickOption(_)
            | ReadCommand::ToggleOption(_) => false,
            ReadCommand::CloseChoiceList => {
                self.close_choice_list();
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
                // Picking up a pen takes the caret out of whatever field had
                // it. Recorded here so the keyboard goes back to the toolbar
                // in the same beat; the *worker* is told separately, because
                // only it can commit what was half-typed (§8.6).
                self.form_typing = false;
                self.form_choice = None;
                self.form_hint = None;
                self.form_widget = None;
                self.date_picker = None;
                self.time_picker = None;
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
            ReadCommand::ArmCrop(on) => self.arm_crop(*on),
            ReadCommand::TakeCrop(choice) => self.take_crop(*choice),
            ReadCommand::CancelCrop => {
                // Armed, not off: the reader who mis-drew a rectangle wants
                // to draw another one, and dropping the tool would make them
                // press the button again first.
                self.marquee = None;
                self.marquee_page = None;
                if self.controls.crop.takes_the_pointer() {
                    self.controls.crop = CropState::Armed;
                }
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
            // Back and forward are resolved against the application's
            // navigation history and arrive here, if at all, as the
            // `GoToPage` they turned into.
            | ReadCommand::HistoryBack
            | ReadCommand::HistoryForward
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

    /// Arm the marquee, or put it away — clearing whatever crop it left.
    ///
    /// Returns whether the viewport moved, which is what tells the
    /// application to take the surface with it.
    fn arm_crop(&mut self, on: bool) -> bool {
        self.marquee = None;
        self.marquee_page = None;
        if on {
            // One pointer owner at a time: a marquee and an armed pen would
            // both take the same press, and the reader would find out which
            // won by pressing.
            self.controls.tool = None;
            self.controls.tool_options = None;
            self.interaction.cancel();
            self.interaction.arm(None);
            self.controls.crop = CropState::Armed;
            return false;
        }
        let was_cropped = matches!(self.controls.crop, CropState::Cropped(_));
        self.controls.crop = CropState::Off;
        if !was_cropped {
            return false;
        }
        // Back to the zoom and the place the crop replaced, re-clamped
        // against the column the uncropped pages make: the window may have
        // been resized while the crop was on, and the old offset may no
        // longer exist.
        let restore = self.crop_restore.take();
        self.relayout();
        if let Some((zoom, offset, offset_x)) = restore {
            self.controls.zoom = zoom;
            self.relayout();
            self.controls.offset = self.column.clamp_offset(offset, self.cell.1);
            self.controls.offset_x = self.column.clamp_offset_x(offset_x, self.cell.0);
            if let Some(page) = self.column.current(self.controls.offset, self.cell.1) {
                self.controls.page = page;
            }
        }
        true
    }

    /// Take the rectangle the reader drew as a zoom, or as a crop.
    fn take_crop(&mut self, choice: CropChoice) -> bool {
        let CropState::Choosing(region) = self.controls.crop else {
            return false;
        };
        let page = self.marquee_page.unwrap_or(self.controls.page);
        self.marquee = None;
        self.marquee_page = None;
        match choice {
            CropChoice::Zoom => {
                // A one-shot: after this the reader is at an ordinary fixed
                // zoom, the latch is off, and the next press of zoom-out is
                // an ordinary press of zoom-out.
                self.controls.crop = CropState::Off;
                self.zoom_into(page, region);
                true
            }
            CropChoice::Pages => {
                self.crop_restore = Some((
                    self.controls.zoom,
                    self.controls.offset,
                    self.controls.offset_x,
                ));
                self.controls.crop = CropState::Cropped(region);
                // The page the crop was drawn on stays the page being read:
                // every page has just changed size, and a reader who cropped
                // a figure should still be looking at that figure.
                let anchor = self.controls.page;
                self.relayout();
                if let Some(offset) = self.column.offset_of(anchor) {
                    self.controls.offset = self.column.clamp_offset(offset, self.cell.1);
                    self.controls.page = anchor;
                }
                self.controls.offset_x = self
                    .column
                    .clamp_offset_x(self.controls.offset_x, self.cell.0);
                true
            }
        }
    }

    /// Fill the window with `region` of `page`.
    fn zoom_into(&mut self, page: PageIndex, region: pulpit_core::notes::Region) {
        let Some(geometry) = self.pages.get(page.get()).copied() else {
            return;
        };
        let (width, height) = (
            geometry.width * region.width,
            geometry.height * region.height,
        );
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        // The rectangle is filled as the reader sees it: turned with the
        // page, so a wide region on a quarter-turned page fills the window's
        // height.
        let (width, height) = if self.controls.rotation.swaps_axes() {
            (height, width)
        } else {
            (width, height)
        };
        // The smaller of the two fits, so the whole rectangle is on screen:
        // filling the width of a window with a tall rectangle would put its
        // foot below the fold, which is not "zoom here".
        let scale = (self.cell.0 / width).min(self.cell.1 / height);
        self.controls.zoom = Zoom::Fixed(scale.clamp(
            crate::widgets::document::model::MIN_ZOOM,
            crate::widgets::document::model::MAX_ZOOM,
        ));
        self.relayout();
        // The rectangle's centre at the window's centre, in the column the
        // new scale made.
        let centre = (
            (region.x + region.width / 2.0) * geometry.width,
            (region.y + region.height / 2.0) * geometry.height,
        );
        if let Some((x, y)) = self.document_point(page, centre.0, centre.1) {
            self.controls.offset = self.column.clamp_offset(y - self.cell.1 / 2.0, self.cell.1);
            self.controls.offset_x = self
                .column
                .clamp_offset_x(x - self.cell.0 / 2.0, self.cell.0);
        }
        if let Some(page) = self.column.current(self.controls.offset, self.cell.1) {
            self.controls.page = page;
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
        // Everything in the facet is expressed on the page as the reader sees
        // it: turned by the view rotation. The session's own geometry is
        // upright, so this is where each piece is rotated — once, on the way
        // out — with the frame itself left upright for the sheet to turn.
        let rotation = self.controls.rotation;
        let visible = if live && self.open {
            self.column
                .visible(self.controls.offset, self.cell.1)
                .into_iter()
                .map(|placed| {
                    let (width, height) = self
                        .pages
                        .get(placed.page.get())
                        .map(|page| (page.width, page.height))
                        .unwrap_or((1.0, 1.0));
                    let turn_preview =
                        |mut preview: crate::widgets::document::preview::GesturePreview| {
                            for point in &mut preview.points {
                                *point = rotation.rotate_point(*point, width, height);
                            }
                            for quad in &mut preview.quads {
                                *quad = rotation.rotate_quad(*quad, width, height);
                            }
                            preview
                        };
                    ReaderPage {
                        // The open gesture, drawn by the UI so the stroke
                        // follows the hand rather than the round trip (A2).
                        // Only ever on the page the gesture is on.
                        preview: self.preview_for(placed.page).map(turn_preview),
                        // Retained ink only: a retained highlight is
                        // composited into the frame by the caller's `frames`,
                        // with the multiply blend a real `/Highlight` uses,
                        // and drawing it here as well would wash it twice.
                        retained: self
                            .retained
                            .iter()
                            .filter(|mark| {
                                mark.page == placed.page && mark.preview.quads.is_empty()
                            })
                            .map(|mark| turn_preview(mark.preview.clone()))
                            .collect(),
                        canonical: if rotation.swaps_axes() {
                            (height, width)
                        } else {
                            (width, height)
                        },
                        // Whatever frame the cache has, even one drawn at
                        // another width or before the last edit: it is
                        // replaced when a newer one arrives (A7). Until the
                        // first one does, the sheet is drawn blank at its
                        // full size, so the column does not move under the
                        // reader when it lands. Frames are rasterised
                        // upright, so the lookup width is the upright one —
                        // the same width the render plan asked with.
                        frame: frames(
                            placed.page,
                            if rotation.swaps_axes() {
                                placed.height
                            } else {
                                placed.width
                            },
                        ),
                        // The hit the reader is on is drawn differently from
                        // the rest, which is the whole use of an overlay:
                        // "there are six on this page and you are looking at
                        // the fourth".
                        found: search
                            .hits_on(placed.page)
                            .filter(|hit| Some(hit.key()) != current_hit)
                            .flat_map(|hit| hit.quads.iter().copied())
                            .map(|quad| rotation.rotate_quad(quad, width, height))
                            .collect(),
                        found_current: search
                            .hits_on(placed.page)
                            .filter(|hit| Some(hit.key()) == current_hit)
                            .flat_map(|hit| hit.quads.iter().copied())
                            .map(|quad| rotation.rotate_quad(quad, width, height))
                            .collect(),
                        // What the reader has picked up, so a held mark looks
                        // held. Nothing here is in the document (§8.4).
                        selection: self
                            .selection_for(placed.page)
                            .into_iter()
                            .map(|mut mark| {
                                mark.bounds = rotation.rotate_rect(mark.bounds, width, height);
                                mark
                            })
                            .collect(),
                        // The crop window, and the rectangle being drawn or
                        // asked about when it is on this page.
                        window: crate::widgets::document::model::rotated_region(
                            self.controls.crop.window(),
                            rotation,
                        ),
                        marquee: self
                            .marquee()
                            .filter(|(page, _)| *page == placed.page)
                            .map(|(_, rect)| rotation.rotate_rect(rect, width, height)),
                        // The widgets pulpit shows and refuses to fill, turned
                        // with the page like every other rectangle here.
                        dead_fields: self
                            .dead_fields_on(placed.page)
                            .into_iter()
                            .map(|mut field| {
                                field.bounds = rotation.rotate_rect(field.bounds, width, height);
                                field
                            })
                            .collect(),
                        rotation,
                        placed,
                    }
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
            has_form: self.has_form,
            fields: &self.fields,
            date_picker: self.date_picker.as_ref(),
            time_picker: self.time_picker.as_ref(),
            focused_widget: self.form_widget.as_ref(),
            focused_hint: self.form_hint.as_deref(),
            choice_list: self.choice_list.as_ref(),
            date_language: self.date_language,
            level: self.level,
            warnings: &self.warnings,
            dirty: self.dirty,
            page_entry: self.page_entry.clone(),
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            // Filled in by the application, for the same reason as
            // `composing`: the history is not this session's.
            can_go_back: false,
            can_go_forward: false,
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
            false,
        );
        session.set_cell(612.0, 400.0);
        session
    }

    /// A form document with one field of every kind the reader has to treat
    /// differently: one it can fill, one it never can, and one the worker
    /// refuses because it names a file.
    fn form(pages: usize) -> ReaderSession {
        use pulpit_render::document::{FieldKind, FieldWidget, FormField};

        let widget = |page: usize| FieldWidget {
            page: PageIndex(page),
            bounds: pulpit_core::page::PageRect::new(100.0, 100.0, 300.0, 130.0),
            option: None,
        };
        let field = |name: &str, kind, page, file_select| FormField {
            name: name.to_string(),
            kind,
            value: String::new(),
            selected: Vec::new(),
            read_only: false,
            required: false,
            password: false,
            file_select,
            rich_text: false,
            format: Default::default(),
            options: Vec::new(),
            allows_custom_value: false,
            multiple_selection: false,
            widgets: vec![widget(page)],
        };
        let mut session = ReaderSession::new();
        session.opened(
            vec![PageGeometry::upright(612.0, 792.0); pages],
            CompatibilityLevel::Native,
            Vec::new(),
            true,
        );
        session.set_cell(612.0, 400.0);
        session.set_fields(vec![
            field("name", FieldKind::Text, 0, false),
            field("attachment", FieldKind::Text, 0, true),
            field("sign here", FieldKind::Signature, 1, false),
        ]);
        session
    }

    #[test]
    fn only_the_fields_nobody_can_fill_are_badged() {
        let session = form(2);

        let first: Vec<&str> = session
            .dead_fields_on(PageIndex(0))
            .iter()
            .map(|field| field.label)
            .collect();
        assert_eq!(
            first,
            vec!["file field — not fillable"],
            "the plain text field is fillable and says nothing"
        );

        let second: Vec<&str> = session
            .dead_fields_on(PageIndex(1))
            .iter()
            .map(|field| field.label)
            .collect();
        assert_eq!(second, vec!["signature — not supported"]);
    }

    #[test]
    fn a_document_with_no_form_badges_nothing() {
        assert!(open(2).dead_fields_on(PageIndex(0)).is_empty());
    }

    #[test]
    fn the_navigator_finds_the_page_a_field_is_drawn_on() {
        let session = form(2);
        assert_eq!(session.field_page("sign here"), Some(PageIndex(1)));
        assert_eq!(session.field_page("name"), Some(PageIndex(0)));
        assert_eq!(session.field_page("no such field"), None);
    }

    #[test]
    fn a_reading_position_survives_a_different_window_and_a_different_zoom() {
        let mut session = open(10);
        session.apply(&ReadCommand::GoToPage(PageIndex(6)));
        // Half way down page six, the way a wheel leaves it.
        let placed = session.column.pages[6];
        session.apply(&ReadCommand::DragScrollHandle(
            placed.top + placed.height / 2.0,
        ));
        let (page, zoom, fraction) = session.reading_position().expect("a laid-out column");
        assert_eq!(page, PageIndex(6));
        assert!((fraction - 0.5).abs() < 1e-2, "{fraction}");

        // Reopened in a window of another size, at another zoom: the page and
        // how far down it the reader was are the same, and the offset in
        // points is not — which is the whole reason the fraction is stored
        // and the offset is not.
        let mut reopened = ReaderSession::new();
        reopened.opened(
            vec![PageGeometry::upright(612.0, 792.0); 10],
            CompatibilityLevel::Native,
            Vec::new(),
            false,
        );
        reopened.set_cell(900.0, 650.0);
        reopened.restore_position(page, Some(zoom), fraction);

        assert_eq!(reopened.controls().page, PageIndex(6));
        let (again, _, fraction_again) = reopened.reading_position().expect("a laid-out column");
        assert_eq!(again, PageIndex(6));
        assert!((fraction_again - 0.5).abs() < 1e-2, "{fraction_again}");
        assert_eq!(reopened.controls().zoom, zoom);
    }

    #[test]
    fn rotating_the_view_keeps_the_reader_on_their_page() {
        use pulpit_core::page::PageRotation;
        let mut session = open(10);
        session.apply(&ReadCommand::GoToPage(PageIndex(4)));
        assert!(
            session.apply(&ReadCommand::RotateView),
            "a rotation re-renders"
        );
        assert_eq!(session.controls().rotation, PageRotation::Clockwise90);
        assert_eq!(session.controls().page, PageIndex(4));
        // The column now holds landscape sheets: a turned portrait page is
        // wider than it is tall, whatever scale the fit chose.
        let placed = session.column.pages[4];
        assert!(
            (placed.width / placed.height - 792.0 / 612.0).abs() < 1e-3,
            "{placed:?}"
        );
        // Four presses go all the way round.
        for _ in 0..3 {
            session.apply(&ReadCommand::RotateView);
        }
        assert_eq!(session.controls().rotation, PageRotation::None);
        assert_eq!(session.controls().page, PageIndex(4));
    }

    #[test]
    fn a_turned_page_is_still_requested_as_an_upright_frame() {
        use pulpit_render::protocol::Quality;
        let mut session = open(3);
        session.apply(&ReadCommand::RotateView);
        let plan = session.render_plan(1.0);
        let refined = plan
            .iter()
            .find(|planned| planned.quality == Quality::Refined)
            .expect("a settled reader gets a refined frame");
        // The sheet on screen is landscape, but the raster asked for is the
        // upright page: one picture serves every rotation.
        assert!(
            refined.width < refined.height,
            "{} x {}",
            refined.width,
            refined.height
        );
    }

    #[test]
    fn a_pointer_on_the_turned_sheet_lands_on_the_upright_page() {
        let mut session = open(3);
        session.apply(&ReadCommand::RotateView);
        // On the quarter-turned letter page the sheet is 792 x 612; the view
        // reports the pointer in the turned page's own points, and the
        // session's cursor speaks upright canonical space.
        session.pointer_moved(PageIndex(0), 700.0, 100.0);
        let (page, at) = session.cursor_position().expect("a cursor on the page");
        assert_eq!(page, PageIndex(0));
        assert!((at.x - 100.0).abs() < 1e-3, "{at:?}");
        assert!((at.y - 92.0).abs() < 1e-3, "{at:?}");
    }

    #[test]
    fn the_facet_speaks_the_turned_pages_language() {
        let mut session = open(3);
        session.apply(&ReadCommand::RotateView);
        let search = pulpit_core::search::SearchState::default();
        let data = session.facet(true, &no_frames, &search);
        let first = &data.visible[0];
        // The canonical size handed to the sheet is the turned one, so the
        // widget's pointer conversion and every overlay agree with the
        // picture being drawn.
        assert_eq!(first.canonical, (792.0, 612.0));
        assert_eq!(first.rotation, pulpit_core::page::PageRotation::Clockwise90);
    }

    #[test]
    fn a_position_past_the_end_of_a_shorter_document_lands_on_its_last_page() {
        // The record is from a longer draft: forty pages became twelve.
        let mut session = open(12);
        session.restore_position(PageIndex(39), Some(Zoom::FitWidth), 0.5);
        assert_eq!(session.controls().page, PageIndex(11));
        // …and inside the column, not past its foot.
        assert!(session.controls().offset <= session.column.height);
    }

    #[test]
    fn restoring_a_page_alone_leaves_the_zoom_the_document_opened_at() {
        // What a path-only match gets: the page, and no claim about anything
        // else.
        let mut session = open(10);
        session.apply(&ReadCommand::SetZoom(Zoom::FitHeight));
        session.restore_position(PageIndex(4), None, 0.0);
        assert_eq!(session.controls().page, PageIndex(4));
        assert_eq!(session.controls().zoom, Zoom::FitHeight);
        // The top of the page, since a fraction into it was not restored.
        let expected = session.column.offset_of(PageIndex(4)).unwrap();
        assert!((session.controls().offset - expected).abs() < 1e-3);
    }

    #[test]
    fn a_document_with_no_window_yet_has_no_position_to_record() {
        let mut session = ReaderSession::new();
        session.opened(
            vec![PageGeometry::upright(612.0, 792.0); 4],
            CompatibilityLevel::Native,
            Vec::new(),
            false,
        );
        // No cell: the offset is a number about a window that never existed,
        // and writing it down would be recording a guess as an answer.
        assert_eq!(session.reading_position(), None);
    }

    #[test]
    fn a_nonsense_fraction_reads_as_the_top_of_the_page() {
        let mut session = open(10);
        session.restore_position(PageIndex(3), Some(Zoom::FitWidth), f32::NAN);
        let expected = session.column.offset_of(PageIndex(3)).unwrap();
        assert!((session.controls().offset - expected).abs() < 1e-3);
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
            false,
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
    fn a_refused_undo_puts_its_operation_back() {
        // The operation comes off the stack when the request is sent, so a
        // worker that refuses it must not cost the reader a step of history.
        let mut session = open(2);
        let _ = session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        let epoch = session.history_epoch();
        let operation = session.undo_operation().expect("something to undo");
        assert!(!session.can_undo(), "it is in flight, not on the stack");

        session.restore_operation(AppliedKind::Undo, epoch, operation);
        assert!(session.can_undo(), "a refusal leaves the history as it was");
        assert!(!session.can_redo(), "and nothing was undone to redo");
    }

    #[test]
    fn a_refused_redo_from_a_history_the_reader_left_is_not_put_back() {
        // An edit landed while the redo was in flight, which makes a new
        // future; the refused operation belongs to the one it replaced.
        let mut session = open(2);
        let _ = session.applied(&applied(DocumentRevision(1)), AppliedKind::Edit);
        session.undo_operation();
        let _ = session.applied(&applied(DocumentRevision(2)), AppliedKind::Undo);

        let epoch = session.history_epoch();
        let operation = session.redo_operation().expect("something to redo");
        let _ = session.applied(&applied(DocumentRevision(3)), AppliedKind::Edit);
        session.restore_operation(AppliedKind::Redo, epoch, operation);
        assert!(
            !session.can_redo(),
            "the taken-back future stays unreachable"
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

    /// The same fixture as [`open`], for a document that has fields in it.
    fn open_with_form(pages: usize) -> ReaderSession {
        let mut session = ReaderSession::new();
        session.opened(
            vec![PageGeometry::upright(612.0, 792.0); pages],
            CompatibilityLevel::Native,
            Vec::new(),
            true,
        );
        session.set_cell(612.0, 400.0);
        session
    }

    /// One field, placed where the test wants it.
    fn field(
        name: &str,
        kind: pulpit_render::document::FieldKind,
        page: usize,
        top: f32,
    ) -> pulpit_render::document::FormField {
        pulpit_render::document::FormField {
            name: name.into(),
            kind,
            value: String::new(),
            selected: Vec::new(),
            read_only: false,
            format: pulpit_render::document::model::FieldFormat::Plain,
            options: Vec::new(),
            allows_custom_value: false,
            multiple_selection: false,
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            widgets: vec![pulpit_render::document::model::FieldWidget {
                page: PageIndex(page),
                bounds: pulpit_core::page::PageRect::new(20.0, top, 200.0, top + 20.0),
                option: None,
            }],
        }
    }

    #[test]
    fn tab_walks_the_fields_in_reading_order_and_wraps() {
        use pulpit_render::document::FieldKind;

        // Deliberately out of reading order in the list, because `/Fields` is
        // whatever order the generator wrote and tabbing through that order is
        // tabbing at random.
        let mut session = open_with_form(2);
        session.set_fields(vec![
            field("second", FieldKind::Text, 0, 300.0),
            field("third", FieldKind::Checkbox, 1, 100.0),
            field("first", FieldKind::Text, 0, 100.0),
        ]);

        // Nothing focused: the walk starts at the page in view.
        assert_eq!(
            session.field_to_focus(true),
            Some((PageIndex(0), "first".to_string()))
        );

        let focus = |session: &mut ReaderSession, name: &str, page: usize| {
            session.set_focused_widget(Some(pulpit_render::document::protocol::FocusedWidget {
                field: name.into(),
                page: PageIndex(page),
                bounds: pulpit_core::page::PageRect::new(20.0, 0.0, 200.0, 20.0),
            }));
        };
        focus(&mut session, "first", 0);
        assert_eq!(
            session.field_to_focus(true),
            Some((PageIndex(0), "second".to_string()))
        );
        focus(&mut session, "second", 0);
        assert_eq!(
            session.field_to_focus(true),
            Some((PageIndex(1), "third".to_string())),
            "a walk that stopped at the page edge would strand every later field"
        );
        // The end of a form is its beginning: there is no edge to fall off.
        focus(&mut session, "third", 1);
        assert_eq!(
            session.field_to_focus(true),
            Some((PageIndex(0), "first".to_string()))
        );
        // …and Shift-Tab is the same walk the other way, wrap included.
        focus(&mut session, "first", 0);
        assert_eq!(
            session.field_to_focus(false),
            Some((PageIndex(1), "third".to_string()))
        );
    }

    #[test]
    fn the_walk_skips_what_arriving_in_would_reach_nothing() {
        use pulpit_render::document::FieldKind;

        // A signature field, a file-select field and a read-only one are all
        // drawn and none of them can be filled (§6.4), so a Tab that landed in
        // one would be a Tab that appeared to do nothing.
        let mut session = open_with_form(1);
        let mut read_only = field("printed", FieldKind::Text, 0, 100.0);
        read_only.read_only = true;
        let mut file = field("attachment", FieldKind::Text, 0, 200.0);
        file.file_select = true;
        let mut placeless = field("nowhere", FieldKind::Text, 0, 250.0);
        placeless.widgets.clear();
        session.set_fields(vec![
            read_only,
            file,
            field("signed", FieldKind::Signature, 0, 300.0),
            field("button", FieldKind::PushButton, 0, 350.0),
            placeless,
            field("name", FieldKind::Text, 0, 400.0),
        ]);

        assert_eq!(
            session.field_to_focus(true),
            Some((PageIndex(0), "name".to_string()))
        );
        // One fillable field, so it is also what comes before itself.
        session.set_focused_widget(Some(pulpit_render::document::protocol::FocusedWidget {
            field: "name".into(),
            page: PageIndex(0),
            bounds: pulpit_core::page::PageRect::new(20.0, 400.0, 200.0, 420.0),
        }));
        assert_eq!(
            session.field_to_focus(false),
            Some((PageIndex(0), "name".to_string()))
        );
    }

    #[test]
    fn a_document_with_nothing_to_fill_has_nowhere_for_tab_to_go() {
        // No form at all, and a form of unreachable fields, are the same
        // answer: the key is not the document's and goes on to the keymap.
        let mut plain = open(1);
        assert_eq!(plain.field_to_focus(true), None);
        plain.set_fields(vec![field(
            "name",
            pulpit_render::document::FieldKind::Text,
            0,
            100.0,
        )]);
        assert_eq!(
            plain.field_to_focus(true),
            None,
            "a document with no form must not answer with a field"
        );

        let mut empty = open_with_form(1);
        assert_eq!(empty.field_to_focus(true), None);
        empty.set_fields(vec![field(
            "signed",
            pulpit_render::document::FieldKind::Signature,
            0,
            100.0,
        )]);
        assert_eq!(empty.field_to_focus(true), None);
    }

    #[test]
    fn space_is_the_boxs_only_where_a_box_holds_the_focus() {
        use pulpit_render::document::FieldKind;

        let mut session = open_with_form(1);
        session.set_fields(vec![
            field("agree", FieldKind::Checkbox, 0, 100.0),
            field("name", FieldKind::Text, 0, 200.0),
        ]);
        assert_eq!(session.focused_field_kind(), None, "nothing is focused yet");

        let focus = |session: &mut ReaderSession, name: &str| {
            session.set_focused_widget(Some(pulpit_render::document::protocol::FocusedWidget {
                field: name.into(),
                page: PageIndex(0),
                bounds: pulpit_core::page::PageRect::new(20.0, 0.0, 200.0, 20.0),
            }));
        };
        focus(&mut session, "agree");
        assert_eq!(session.focused_field_kind(), Some(FieldKind::Checkbox));
        // A text field's space is a space, which is why the kind is asked for
        // rather than assumed from "something is focused".
        focus(&mut session, "name");
        assert_eq!(session.focused_field_kind(), Some(FieldKind::Text));
    }

    #[test]
    fn a_press_reaches_the_form_only_when_the_document_has_one() {
        // A deck of slides is the common case, and the worker is serial: a
        // round trip per click on a document with no fields would queue in
        // front of the page renders the reader is waiting on.
        let mut plain = open(1);
        plain.pointer_moved(PageIndex(0), 40.0, 40.0);
        assert!(
            !plain.press_belongs_to_the_form(),
            "a document with no fields must not be sent presses"
        );

        let mut form = open_with_form(1);
        form.pointer_moved(PageIndex(0), 40.0, 40.0);
        assert!(form.press_belongs_to_the_form());
    }

    #[test]
    fn an_armed_tool_draws_on_a_form_rather_than_typing_into_it() {
        // A reader who picked up the pen means to write on the page. The
        // fields are still there and fillable the moment it is put down (§8.4).
        let mut session = open_with_form(1);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert!(!session.press_belongs_to_the_form());
        session.apply(&ReadCommand::Arm(None));
        assert!(session.press_belongs_to_the_form());
    }

    #[test]
    fn the_caret_is_the_workers_word_and_arming_a_tool_takes_it_back() {
        let mut session = open_with_form(1);
        assert!(!session.form_has_keyboard(), "nothing has focus at open");

        // Only the worker can say a click landed in a field: PDFium owns the
        // caret, and guessing here is how a letter becomes a page turn.
        session.set_form_typing(true);
        assert!(session.form_has_keyboard());

        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert!(
            !session.form_has_keyboard(),
            "arming a tool must take the keyboard back from the field"
        );
    }

    #[test]
    fn a_document_with_no_form_never_reports_a_caret_in_one() {
        // Belt and braces. A stale focus left over from a previous document
        // would otherwise swallow every keystroke into a form that is not
        // there — silently, since there is nothing on screen to show for it.
        let mut session = open(1);
        session.set_form_typing(true);
        assert!(!session.form_has_keyboard());
    }

    fn committed(
        name: &str,
        value: &str,
        previous: &str,
    ) -> pulpit_render::document::protocol::CommittedField {
        pulpit_render::document::protocol::CommittedField {
            name: name.into(),
            value: value.into(),
            previous: previous.into(),
            revision: DocumentRevision(4),
            selected: Vec::new(),
            previous_selected: Vec::new(),
        }
    }

    #[test]
    fn a_committed_field_moves_the_revision_and_leaves_the_document_unsaved() {
        let mut session = open_with_form(1);
        assert_eq!(session.revision(), DocumentRevision::INITIAL);
        assert!(!session.is_dirty());
        session.field_committed(&committed("name", "Ada", ""));
        assert_eq!(session.revision(), DocumentRevision(4));
        assert!(
            session.is_dirty(),
            "a filled field is an unsaved change like any other"
        );
    }

    #[test]
    fn a_filled_field_joins_the_same_undo_history_as_the_marks() {
        // §8.6: a field edit followed by a stroke undoes the stroke first.
        // This was the one mutation with no inverse, so undo used to reach
        // straight past a typed value to the last annotation edit.
        let mut session = open_with_form(1);
        session.field_committed(&committed("name", "Ada", ""));

        let undo = session
            .undo_operation()
            .expect("a filled field must be undoable");
        assert_eq!(undo.label, "Fill name");
        assert_eq!(
            undo.operations,
            vec![pulpit_render::document::UndoOperation::SetField {
                name: "name".into(),
                // Back to what was there before, which for a first fill is
                // an empty field rather than the value just typed.
                value: String::new(),
                selected: Vec::new(),
            }]
        );
        assert_eq!(
            undo.restores,
            DocumentRevision::INITIAL,
            "the undo names the revision it goes back to"
        );
    }

    #[test]
    fn a_commit_the_engine_could_not_name_moves_the_revision_and_nothing_else() {
        // A toggle or a choice whose field PDFium would not name has no
        // inverse that could be applied — there is no field to put a value
        // back into. The document still moved, so the revision still does.
        let mut session = open_with_form(1);
        session.field_committed(&committed("", "", ""));
        assert_eq!(session.revision(), DocumentRevision(4));
        assert!(session.is_dirty());
        assert!(
            session.undo_operation().is_none(),
            "an unnamed commit must not push an inverse that cannot be applied"
        );
    }

    fn choice(
        selected: Option<u32>,
        options: u32,
    ) -> pulpit_render::document::protocol::FocusedChoice {
        pulpit_render::document::protocol::FocusedChoice {
            field: "country".into(),
            selected,
            selections: selected.into_iter().collect(),
            options,
            labels: (0..options)
                .map(|index| format!("option {index}"))
                .collect(),
            editable: false,
            multiple_selection: false,
            list_box: false,
            page: PageIndex(0),
            bounds: pulpit_core::page::PageRect::new(10.0, 20.0, 120.0, 36.0),
        }
    }

    #[test]
    fn a_focused_combo_box_gives_the_keyboard_to_the_form_without_a_text_caret() {
        // A combo box is not a text field, so PDFium never calls
        // `FFI_SetTextFieldFocus` for one. Waiting for a text caret meant the
        // arrow keys stayed the toolbar's and scrolled the page instead of
        // moving the selection.
        let mut session = open_with_form(1);
        assert!(!session.form_has_keyboard());
        session.set_focused_choice(Some(choice(Some(0), 3)));
        assert!(session.form_has_keyboard());
    }

    #[test]
    fn stepping_through_a_combo_box_stops_at_both_ends() {
        // Native combo boxes stop; they do not wrap. A list that jumped from
        // the last entry back to the first would be a way to pick the wrong
        // one without noticing.
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(Some(0), 3)));
        assert_eq!(session.choice_step(true), Some(1));
        assert_eq!(session.choice_step(false), None, "already at the first");

        session.set_focused_choice(Some(choice(Some(2), 3)));
        assert_eq!(session.choice_step(true), None, "already at the last");
        assert_eq!(session.choice_step(false), Some(1));
    }

    #[test]
    fn a_combo_box_holding_something_off_its_own_list_starts_at_the_near_end() {
        // The corpus carries this: a non-editable combo whose `/V` is not in
        // `/Opt`. Nothing is selected, so neither arrow has a neighbour to
        // move to — and doing nothing would leave the field unusable.
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(None, 4)));
        assert_eq!(session.choice_step(true), Some(0));
        assert_eq!(session.choice_step(false), Some(3));
    }

    #[test]
    fn a_combo_box_with_no_options_has_nowhere_to_step() {
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(None, 0)));
        assert_eq!(session.choice_step(true), None);
        assert_eq!(session.choice_step(false), None);
    }

    #[test]
    fn an_open_option_list_starts_on_what_the_field_already_holds() {
        // Enter on a list nobody has moved must choose the option that is
        // already chosen, not the first row — otherwise opening a list and
        // pressing Enter silently changes the answer.
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(Some(2), 4)));
        session.open_choice_list();
        let open = session.choice_list().expect("the list is open");
        assert_eq!(open.highlighted, 2);
        assert_eq!(open.selected, Some(2));
        assert_eq!(session.take_highlighted_option(), Some(2));
        assert!(session.choice_list().is_none(), "committing closes it");
    }

    #[test]
    fn an_editable_combo_box_keeps_the_engines_own_list() {
        // An editable combo is a text box with a list attached, and PDFium is
        // drawing its caret. Drawing a second editing surface over that is
        // what §8.6 forbids, so the fallback path stays.
        let mut session = open_with_form(1);
        let mut editable = choice(Some(0), 3);
        editable.editable = true;
        session.set_focused_choice(Some(editable));
        session.open_choice_list();
        assert!(session.choice_list().is_none());
        assert_eq!(
            session.choice_step(true),
            Some(1),
            "and the arrow keys still move it, as they did before"
        );
    }

    #[test]
    fn the_highlight_moves_without_committing_and_stops_at_both_ends() {
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(Some(0), 3)));
        session.open_choice_list();
        assert!(session.step_choice_list(true));
        assert!(session.step_choice_list(true));
        assert!(!session.step_choice_list(true), "the last row is the last");
        assert_eq!(
            session.choice_list().map(|open| open.selected),
            Some(Some(0)),
            "nothing is committed by moving the highlight"
        );
        assert!(session.step_choice_list(false));
        assert_eq!(session.choice_list().map(|open| open.highlighted), Some(1));
    }

    #[test]
    fn the_caret_leaving_the_field_takes_its_list_with_it() {
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(Some(0), 3)));
        session.open_choice_list();
        assert!(session.choice_list().is_some());

        // The same field, re-reported after a commit: the list stays and takes
        // the new selection.
        session.set_focused_choice(Some(choice(Some(2), 3)));
        assert_eq!(
            session.choice_list().map(|open| open.selected),
            Some(Some(2))
        );

        let mut elsewhere = choice(Some(0), 3);
        elsewhere.field = "city".into();
        session.set_focused_choice(Some(elsewhere));
        assert!(session.choice_list().is_none(), "another field, no list");

        session.set_focused_choice(None);
        assert!(session.choice_list().is_none());
    }

    /// A multi-select list box, from the state machine's side (§8.6).
    ///
    /// The three things that differ from a single-select list: a tick does not
    /// close it, the tick asked for is the *opposite* of what the row holds,
    /// and nothing is written here — the rows follow the engine's answer.
    #[test]
    fn ticking_a_row_of_a_multi_select_list_leaves_it_open() {
        let mut session = open_with_form(1);
        let mut many = choice(Some(0), 3);
        many.multiple_selection = true;
        many.list_box = true;
        many.selections = vec![0];
        session.set_focused_choice(Some(many.clone()));
        session.open_choice_list();

        let open = session.choice_list().expect("the list is open");
        assert!(open.multiple);
        assert!(open.is_selected(0));
        assert!(!open.is_selected(2));

        // Ticking an unticked row asks for it to be turned on…
        assert_eq!(session.toggle_option(2), Some((2, true)));
        assert!(
            session.choice_list().is_some(),
            "a list that shut after one tick could not choose three things"
        );
        assert_eq!(
            session.choice_list().map(|open| open.selections.clone()),
            Some(vec![0]),
            "and nothing is ticked here: the engine's answer does that"
        );
        assert_eq!(
            session.choice_list().map(|open| open.highlighted),
            Some(2),
            "the highlight follows the row that was pressed"
        );

        // …and the engine's answer is what actually ticks it.
        let mut both = many.clone();
        both.selections = vec![0, 2];
        session.set_focused_choice(Some(both.clone()));
        let open = session.choice_list().expect("the list is still open");
        assert!(open.is_selected(0) && open.is_selected(2));
        assert!(!open.is_selected(1));

        // Ticking a row that is on asks for it to be turned off.
        assert_eq!(session.toggle_option(0), Some((0, false)));

        // Space is the keyboard's half of the same thing, on whichever row
        // the highlight is on — which the press above left on row 0.
        assert_eq!(session.toggle_highlighted_option(), Some((0, false)));
        assert!(session.step_choice_list(true));
        assert_eq!(
            session.toggle_highlighted_option(),
            Some((1, true)),
            "and an arrow key moved it to a row that is not chosen"
        );

        // A row that is not one of the rows is not a row.
        assert_eq!(session.toggle_option(9), None);
    }

    #[test]
    fn a_single_select_list_is_chosen_from_rather_than_ticked() {
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(Some(1), 3)));
        session.open_choice_list();
        assert!(!session.choice_list_is_multiple());
        assert_eq!(
            session.toggle_option(2),
            None,
            "a single-select row is chosen, not toggled"
        );
        assert_eq!(session.toggle_highlighted_option(), None);
        // …and choosing still closes it, exactly as it did before.
        assert_eq!(session.take_highlighted_option(), Some(1));
        assert!(session.choice_list().is_none());
    }

    #[test]
    fn a_multi_select_list_closes_without_choosing_a_row() {
        // Enter on a multi-select list means "done", and closing it is all it
        // does: every tick was committed as it was made, so treating Enter as
        // a choice would toggle whichever row the highlight was resting on.
        let mut session = open_with_form(1);
        let mut many = choice(Some(0), 3);
        many.multiple_selection = true;
        many.selections = vec![0];
        session.set_focused_choice(Some(many));
        session.open_choice_list();
        assert!(session.choice_list_is_multiple());
        session.close_choice_list();
        assert!(session.choice_list().is_none());
        assert!(
            !session.choice_list_is_multiple(),
            "a closed list is no kind of list"
        );
    }

    #[test]
    fn a_choice_field_with_no_options_opens_no_list() {
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(None, 0)));
        session.open_choice_list();
        assert!(session.choice_list().is_none());
        assert_eq!(session.take_highlighted_option(), None);
    }

    #[test]
    fn arming_a_tool_lets_go_of_a_focused_combo_box_too() {
        let mut session = open_with_form(1);
        session.set_focused_choice(Some(choice(Some(1), 3)));
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert!(session.focused_choice().is_none());
        assert!(!session.form_has_keyboard());
    }

    #[test]
    fn a_field_says_what_it_wants_once_and_not_once_per_keystroke() {
        // Every form event carries the focused field's hint, including the
        // ones that changed nothing. Saying it each time would be a line of
        // diagnostics per character typed.
        let mut session = open_with_form(1);
        assert!(session.take_form_hint(Some("date, as dd mmmm yyyy")));
        assert!(
            !session.take_form_hint(Some("date, as dd mmmm yyyy")),
            "the same field must not announce itself twice"
        );

        // A different field is worth saying.
        assert!(session.take_form_hint(Some("a number")));
        // …and a field that wants nothing in particular says nothing, but is
        // still recorded, so returning to the date field announces it again.
        assert!(!session.take_form_hint(None));
        assert!(session.take_form_hint(Some("date, as dd mmmm yyyy")));
    }

    #[test]
    fn arming_a_tool_forgets_what_the_field_wanted() {
        let mut session = open_with_form(1);
        assert!(session.take_form_hint(Some("a number")));
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert!(
            session.take_form_hint(Some("a number")),
            "coming back to the field should say what it wants again"
        );
    }

    fn focused_date(field: &str, pattern: &str) -> pulpit_render::document::protocol::FocusedDate {
        pulpit_render::document::protocol::FocusedDate {
            field: field.into(),
            pattern: pattern.into(),
            page: PageIndex(0),
            bounds: pulpit_core::page::PageRect::new(100.0, 100.0, 300.0, 130.0),
        }
    }

    #[test]
    fn a_calendar_opens_over_a_date_field_and_closes_when_the_caret_leaves() {
        let mut session = open_with_form(1);
        let today = crate::datefield::Date::new(2026, 8, 16);
        assert!(session.date_picker().is_none());

        session.set_focused_date(Some(&focused_date("when", "dd mmmm yyyy")), today);
        let picker = session.date_picker().expect("a calendar over the field");
        assert_eq!(picker.field, "when");
        assert_eq!(picker.pattern, "dd mmmm yyyy");
        assert_eq!(picker.month, crate::datefield::CalendarMonth::new(2026, 8));

        session.set_focused_date(None, today);
        assert!(session.date_picker().is_none());
    }

    #[test]
    fn paging_the_calendar_survives_the_next_keystroke_in_the_same_field() {
        // Every form event reports the focused field, so a picker rebuilt on
        // each one would snap back to this month the moment anything is typed
        // — and paging back to last December would be impossible.
        let mut session = open_with_form(1);
        let today = crate::datefield::Date::new(2026, 8, 16);
        session.set_focused_date(Some(&focused_date("when", "dd mmmm yyyy")), today);
        session.step_date_picker(false);
        session.step_date_picker(false);
        assert_eq!(
            session.date_picker().unwrap().month,
            crate::datefield::CalendarMonth::new(2026, 6)
        );

        session.set_focused_date(Some(&focused_date("when", "dd mmmm yyyy")), today);
        assert_eq!(
            session.date_picker().unwrap().month,
            crate::datefield::CalendarMonth::new(2026, 6),
            "the month the reader navigated to was thrown away"
        );

        // A *different* field is a different calendar, and opens on today.
        session.set_focused_date(Some(&focused_date("other", "dd mmmm yyyy")), today);
        assert_eq!(
            session.date_picker().unwrap().month,
            crate::datefield::CalendarMonth::new(2026, 8)
        );
    }

    #[test]
    fn arming_a_tool_puts_the_calendar_away() {
        let mut session = open_with_form(1);
        session.set_focused_date(
            Some(&focused_date("when", "dd mmmm yyyy")),
            crate::datefield::Date::new(2026, 8, 16),
        );
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert!(
            session.date_picker().is_none(),
            "a calendar left open over a page being drawn on is chrome nobody asked for"
        );
    }

    fn focused_time(
        field: &str,
        pattern: &str,
        value: &str,
    ) -> pulpit_render::document::protocol::FocusedTime {
        pulpit_render::document::protocol::FocusedTime {
            field: field.into(),
            pattern: pattern.into(),
            value: value.into(),
            page: PageIndex(0),
            bounds: pulpit_core::page::PageRect::new(100.0, 100.0, 300.0, 130.0),
        }
    }

    #[test]
    fn the_time_helper_opens_on_the_value_and_falls_back_to_the_clock() {
        let mut session = open_with_form(1);
        let now = crate::datefield::TimeOfDay::new(11, 45);
        assert!(session.time_picker().is_none());

        session.set_focused_time(Some(&focused_time("at", "h:MM tt", "2:05 pm")), now);
        let picker = session.time_picker().expect("steppers over the field");
        assert_eq!(picker.field, "at");
        assert_eq!(picker.time, crate::datefield::TimeOfDay::new(14, 5));
        assert!(picker.twelve_hour() && picker.shows_meridiem());

        // An empty field has nothing to open on, so the wall clock is what a
        // reader is most likely to want and least likely to be annoyed by.
        session.set_focused_time(Some(&focused_time("other", "HH:MM", "")), now);
        let picker = session
            .time_picker()
            .expect("steppers over the other field");
        assert_eq!(picker.time, now);
        assert!(
            !picker.twelve_hour() && !picker.shows_meridiem(),
            "a 24-hour field must not be given an am/pm toggle"
        );

        session.set_focused_time(None, now);
        assert!(session.time_picker().is_none());
    }

    #[test]
    fn stepping_the_time_survives_the_next_event_in_the_same_field() {
        // The calendar's argument exactly: every form event re-reports the
        // focused field, and a helper rebuilt on each one would undo a dozen
        // presses on the first keystroke.
        let mut session = open_with_form(1);
        let now = crate::datefield::TimeOfDay::new(11, 45);
        session.set_focused_time(Some(&focused_time("at", "HH:MM", "09:00")), now);
        session.apply(&ReadCommand::StepTimePicker(60));
        session.apply(&ReadCommand::StepTimePicker(-1));
        assert_eq!(
            session.time_picker().unwrap().time,
            crate::datefield::TimeOfDay::new(9, 59)
        );

        session.set_focused_time(Some(&focused_time("at", "HH:MM", "09:00")), now);
        assert_eq!(
            session.time_picker().unwrap().time,
            crate::datefield::TimeOfDay::new(9, 59),
            "the time the reader stepped to was thrown away"
        );

        session.apply(&ReadCommand::CloseTimePicker);
        assert!(session.time_picker().is_none());
    }

    #[test]
    fn arming_a_tool_puts_the_time_helper_away() {
        let mut session = open_with_form(1);
        session.set_focused_time(
            Some(&focused_time("at", "HH:MM", "09:00")),
            crate::datefield::TimeOfDay::new(11, 45),
        );
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        assert!(session.time_picker().is_none());
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
            false,
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

    /// The marquee is a *view* gesture: while it is armed the pointer draws
    /// rectangles and nothing on the page — no tool, no link, no field —
    /// hears about the press.
    #[test]
    fn an_armed_marquee_takes_the_press_from_the_tools() {
        let mut session = open(3);
        session.apply(&ReadCommand::Arm(Some(AnnotationTool::Ink)));
        session.apply(&ReadCommand::ArmCrop(true));
        // Arming the crop disarmed the pen: one pointer owner at a time.
        assert_eq!(session.controls().tool, None);
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        assert!(
            session.pointer_pressed(),
            "the marquee did not take the press"
        );
        session.pointer_moved(PageIndex(0), 400.0, 500.0);
        assert!(matches!(session.pointer_released(), Released::Nothing));
        assert!(!session.is_dirty(), "a crop touched the document");
    }

    /// Drawn from bottom-right to top-left is the same rectangle as drawn the
    /// other way, and it is measured in fractions of the page.
    #[test]
    fn a_marquee_drawn_backwards_is_the_same_rectangle() {
        let mut session = open(3);
        session.apply(&ReadCommand::ArmCrop(true));
        session.pointer_moved(PageIndex(0), 459.0, 594.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 153.0, 198.0);
        session.pointer_released();
        let CropState::Choosing(region) = session.controls().crop else {
            panic!(
                "the drag did not become a question: {:?}",
                session.controls().crop
            );
        };
        assert!((region.x - 0.25).abs() < 1e-3);
        assert!((region.y - 0.25).abs() < 1e-3);
        assert!((region.width - 0.5).abs() < 1e-3);
        assert!((region.height - 0.5).abs() < 1e-3);
    }

    /// A press that did not travel is a click, not a rectangle: the tool is
    /// left armed for the drag that was meant, and nothing is asked.
    #[test]
    fn a_tap_is_not_a_marquee() {
        let mut session = open(3);
        session.apply(&ReadCommand::ArmCrop(true));
        let before = session.controls().zoom;
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 101.0, 101.0);
        session.pointer_released();
        assert_eq!(session.controls().crop, CropState::Armed);
        assert_eq!(session.controls().zoom, before);
    }

    /// "Zoom here" fills the window with the rectangle and leaves nothing
    /// behind: an ordinary fixed zoom, and the latch off.
    #[test]
    fn zooming_here_fills_the_window_and_leaves_the_tool_off() {
        let mut session = open(3);
        session.apply(&ReadCommand::ArmCrop(true));
        // The middle quarter of the page: 306 x 396 points.
        session.pointer_moved(PageIndex(0), 153.0, 198.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 459.0, 594.0);
        session.pointer_released();
        session.apply(&ReadCommand::TakeCrop(CropChoice::Zoom));
        assert_eq!(session.controls().crop, CropState::Off);
        // The smaller of the two fits, so the whole rectangle is on screen:
        // 400 / 396 rather than 612 / 306.
        let Zoom::Fixed(scale) = session.controls().zoom else {
            panic!("zooming here did not set a scale");
        };
        assert!((scale - 400.0 / 396.0).abs() < 1e-3, "scale was {scale}");
    }

    /// A crop shrinks every page in the column, and a fit then fits what is
    /// left — which is the whole point of trimming margins.
    #[test]
    fn a_crop_shrinks_every_page_in_the_column() {
        let mut session = open(4);
        let full = session.column.pages[1].top;
        session.apply(&ReadCommand::ArmCrop(true));
        session.pointer_moved(PageIndex(0), 0.0, 0.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 612.0, 396.0);
        session.pointer_released();
        session.apply(&ReadCommand::TakeCrop(CropChoice::Pages));
        assert!(matches!(session.controls().crop, CropState::Cropped(_)));
        // Half the height, at the same fit-width scale: the second page now
        // starts half a page plus a gap down the column.
        let cropped = session.column.pages[1].top;
        assert!(
            cropped < full,
            "the column did not shrink: {cropped} vs {full}"
        );
        assert!(
            (cropped - (full - 792.0 / 2.0)).abs() < 1.0,
            "the column shrank by the wrong amount: {cropped} vs {full}"
        );
        // Every page, not just the one the rectangle was drawn on.
        for page in &session.column.pages {
            assert!(
                (page.height - 396.0).abs() < 1.0,
                "page {:?} kept its margins",
                page.page
            );
        }
    }

    /// The same crop trims pages of different sizes in proportion. A
    /// rectangle in points would be right for one of them and wrong for the
    /// other, which is why a crop is fractions.
    #[test]
    fn a_crop_is_proportional_across_page_sizes() {
        let mut session = ReaderSession::new();
        session.opened(
            vec![
                PageGeometry::upright(612.0, 792.0),
                PageGeometry::upright(595.0, 842.0),
            ],
            CompatibilityLevel::Native,
            Vec::new(),
            false,
        );
        session.set_cell(612.0, 400.0);
        session.apply(&ReadCommand::ArmCrop(true));
        session.pointer_moved(PageIndex(0), 61.2, 79.2);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 550.8, 712.8);
        session.pointer_released();
        session.apply(&ReadCommand::TakeCrop(CropChoice::Pages));
        let scale = session.scale;
        assert!((session.column.pages[0].height - 792.0 * 0.8 * scale).abs() < 1.0);
        assert!((session.column.pages[1].height - 842.0 * 0.8 * scale).abs() < 1.0);
    }

    /// Unlatching the button puts the reader back where the crop was taken —
    /// the zoom *and* the place in the document, which is what makes the
    /// round trip one press rather than a jump back to the top.
    #[test]
    fn clearing_a_crop_puts_the_reader_back() {
        let mut session = open(10);
        session.apply(&ReadCommand::SetZoom(Zoom::Fixed(1.5)));
        session.apply(&ReadCommand::GoToPage(PageIndex(4)));
        let (zoom, offset, page) = (
            session.controls().zoom,
            session.controls().offset,
            session.controls().page,
        );
        session.apply(&ReadCommand::ArmCrop(true));
        session.pointer_moved(PageIndex(4), 61.2, 79.2);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(4), 550.8, 712.8);
        session.pointer_released();
        session.apply(&ReadCommand::TakeCrop(CropChoice::Pages));
        assert_ne!(session.controls().offset, offset);
        session.apply(&ReadCommand::ArmCrop(false));
        assert_eq!(session.controls().crop, CropState::Off);
        assert_eq!(session.controls().zoom, zoom);
        assert!((session.controls().offset - offset).abs() < 1.0);
        assert_eq!(session.controls().page, page);
    }

    /// A cropped page asks the renderer for the crop and not for the page:
    /// the pixels go on what is on the sheet, not on margins nobody sees.
    #[test]
    fn a_cropped_page_asks_for_its_region() {
        let mut session = open(3);
        assert!(session
            .render_plan(1.0)
            .iter()
            .all(|entry| entry.region == pulpit_core::notes::Region::FULL));
        session.apply(&ReadCommand::ArmCrop(true));
        session.pointer_moved(PageIndex(0), 0.0, 0.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 612.0, 396.0);
        session.pointer_released();
        session.apply(&ReadCommand::TakeCrop(CropChoice::Pages));
        let plan = session.render_plan(1.0);
        assert!(!plan.is_empty());
        for entry in plan {
            assert!((entry.region.height - 0.5).abs() < 1e-3);
            assert_eq!(entry.region.y, 0.0);
        }
    }

    /// Escape's command drops the rectangle and keeps the tool: the reader
    /// who mis-drew one draws another rather than re-arming first.
    #[test]
    fn cancelling_a_rectangle_leaves_the_tool_armed() {
        let mut session = open(3);
        session.apply(&ReadCommand::ArmCrop(true));
        session.pointer_moved(PageIndex(0), 100.0, 100.0);
        session.pointer_pressed();
        session.pointer_moved(PageIndex(0), 500.0, 600.0);
        session.pointer_released();
        assert!(matches!(session.controls().crop, CropState::Choosing(_)));
        session.apply(&ReadCommand::CancelCrop);
        assert_eq!(session.controls().crop, CropState::Armed);
    }
}
