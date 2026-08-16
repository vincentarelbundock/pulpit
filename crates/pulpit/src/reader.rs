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

use pulpit_core::annotation::AnnotationTool;
use pulpit_core::page::{PageGeometry, PageIndex};
use pulpit_render::document::{CompatibilityLevel, DocumentRevision, DocumentWarning, FormField};

use crate::widgets::context::{OutlineRow, ReaderData, ReaderPage};
use crate::widgets::document::model::{Column, ReaderControls, Zoom};
use crate::widgets::event::ReadCommand;

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
}

// The reader session is complete; the parts of it the application has not
// yet been wired to — the page-gesture path and the document-mode worker
// requests — are the remaining steps of `SPEC-document.md` §14.3, and the
// surface they will call is deliberately here and tested rather than added
// later beside its first caller.
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

    pub fn set_outline(&mut self, outline: Vec<OutlineRow>) {
        self.outline = outline;
    }

    pub fn set_fields(&mut self, fields: Vec<FormField>) {
        self.fields = fields;
    }

    /// The worker applied something. The revision is what a frame is matched
    /// against (A7), and the depths are what the two history controls read.
    pub fn applied(&mut self, revision: DocumentRevision, undo_depth: usize, redo_depth: usize) {
        self.revision = revision;
        self.undo_depth = undo_depth;
        self.redo_depth = redo_depth;
        self.dirty = true;
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
                    placed,
                    // Frames arrive from the worker; until one does, the sheet
                    // is drawn blank at its full size so the column does not
                    // move when it lands.
                    frame: None,
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
        session.applied(DocumentRevision(3), 1, 0);
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
        session.applied(DocumentRevision(1), 1, 0);
        let facet = session.facet(true);
        assert!(facet.can_undo && !facet.can_redo);
        assert!(facet.dirty);
        assert_eq!(session.revision(), DocumentRevision(1));
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
