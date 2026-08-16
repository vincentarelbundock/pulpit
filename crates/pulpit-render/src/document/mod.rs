//! The document engine: a long-lived open PDF that can be read, annotated,
//! filled, rendered and saved.
//!
//! `SPEC-document.md` §5.4 and §6. Two things shape the module:
//!
//! * **One committed representation** (A1). A completed annotation is created
//!   in the open document immediately on commit. There is no overlay archive
//!   and no annotation sidecar; [`AnnotationSummary`] is a *view* of the
//!   document, rebuilt from it, never a second copy that could drift.
//! * **No presentation scheduling.** Opening, annotating, filling, rendering
//!   or saving must not require constructing a `RenderGeneration`, a
//!   `Priority` or a `Quality` (§5.4). Nothing in this module names one, and
//!   [`tests::the_document_api_needs_no_presentation_scheduling_types`] is
//!   what keeps it that way now that a feature flag no longer can.
//!
//! The engine is split from its backing store by [`DocumentBackend`], so the
//! revision model, transaction atomicity and undo semantics are ordinary unit
//! tests that run without PDFium — and so that the PDFium implementation has
//! only PDF work left in it.

pub mod limits;
pub mod memory;
pub mod model;
pub mod protocol;
pub mod session;
pub mod worker;

#[cfg(feature = "pdfium")]
pub mod pdfium;

use std::path::Path;

use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, AnnotationId, IdGenerator};
use pulpit_core::page::{PageGeometry, PageIndex, PageRect};

pub use limits::LimitExceeded;
pub use model::{
    AnnotationBeforeImage, AnnotationContents, AnnotationSummary, AnnotationSupport, Applied,
    AppliedEffect, CompatibilityLevel, DocumentCommand, DocumentRevision, DocumentTransaction,
    DocumentUndo, DocumentWarning, FieldKind, FieldWidget, FormField, OpenDocumentInfo,
    SaveOptions, SavedDocument, TextSelection, TextSelectionResult, UndoOperation,
};

/// Why a document operation failed.
///
/// Separate from the renderer's [`crate::pdf::PdfError`] because the failures
/// are different in kind: a render fails because a page could not be drawn, a
/// mutation fails because the document said no, the revision moved underneath
/// it, or the input was refused before anything was touched.
#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("the document has no page {page} ({count} pages)")]
    NoSuchPage { page: usize, count: usize },
    #[error("no annotation {0} in this document")]
    NoSuchAnnotation(AnnotationId),
    #[error("no field named {0}")]
    NoSuchField(String),
    #[error("{0} cannot be edited without losing what it carries")]
    NotEditable(AnnotationId),
    #[error("the document changed: expected {expected}, the document is at {actual}")]
    RevisionConflict {
        expected: DocumentRevision,
        actual: DocumentRevision,
    },
    #[error("the mark cannot be written: {0}")]
    Rejected(#[from] pulpit_core::annotate::draft::DraftError),
    #[error(transparent)]
    Limit(#[from] LimitExceeded),
    #[error("this document does not allow being changed")]
    MutationForbidden,
    #[error("the source file is not a destination; use Save As")]
    SourceIsDestination,
    #[error("the engine failed: {0}")]
    Backend(String),
    #[error("saving failed: {0}")]
    Save(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DocumentError>;

/// What the engine mutates. One implementation per real store.
///
/// Every method works in canonical page space and identifies annotations by
/// [`AnnotationId`] (A3, A4), so an implementation is free to renumber objects
/// on save without anything above noticing.
pub trait DocumentBackend: Send {
    fn info(&self) -> &OpenDocumentInfo;

    fn page_count(&self) -> usize {
        self.info().page_count
    }

    fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry>;

    /// Every annotation on one page, in `/Annots` order — which is paint
    /// order, and therefore the order hit-testing needs.
    fn annotations(&self, page: PageIndex) -> Result<Vec<AnnotationSummary>>;

    /// One annotation with its full geometry, for the case where enumeration
    /// elided it (§6.1).
    fn annotation(&self, id: &AnnotationId) -> Result<AnnotationSummary>;

    /// Create an annotation carrying `id` in its `/NM`.
    fn create(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary>;

    /// Replace an annotation's modelled content, keeping its identity and its
    /// unrecognised entries.
    fn replace(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary>;

    /// Delete an annotation, handing back everything needed to put it back.
    fn delete(&mut self, id: &AnnotationId) -> Result<AnnotationBeforeImage>;

    /// Put a deleted annotation back under its own identity.
    fn restore(
        &mut self,
        id: &AnnotationId,
        before: &AnnotationBeforeImage,
    ) -> Result<AnnotationSummary>;

    /// A lossless copy of what an annotation is now, for an undo record.
    fn before_image(&self, id: &AnnotationId) -> Result<AnnotationBeforeImage>;

    fn fields(&self) -> Result<Vec<FormField>>;

    /// Write a field, returning the value the document actually took.
    fn set_field(&mut self, name: &str, value: &str) -> Result<String>;

    fn field_value(&self, name: &str) -> Result<String>;

    fn select_text(&self, page: PageIndex, selection: TextSelection)
        -> Result<TextSelectionResult>;

    /// Write the document to `destination`. The caller has already checked
    /// that it is not the source (A6) and has staged a temporary path.
    fn write_to(&mut self, destination: &Path, options: SaveOptions) -> Result<u64>;

    /// The path this document was opened from, so a save can refuse it.
    fn source(&self) -> Option<&Path>;
}

/// A worker-confined open document (§6).
///
/// The lifetime is the engine.s: the PDFium engine borrows a binding that is
/// bound once per process and outlives every document opened through it, and
/// the memory engine borrows nothing and is `'static`.
pub struct PdfDocument<'a> {
    backend: Box<dyn DocumentBackend + 'a>,
    revision: DocumentRevision,
    ids: IdGenerator,
    /// True from the first successful mutation. Visible in the UI (§11.2).
    dirty: bool,
}

impl std::fmt::Debug for PdfDocument<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument")
            .field("revision", &self.revision)
            .field("dirty", &self.dirty)
            .field("pages", &self.backend.page_count())
            .finish()
    }
}

impl<'a> PdfDocument<'a> {
    /// Wrap a backend. `id_seed` is what [`IdGenerator`] mixes into the names
    /// this session writes; the worker passes something session-unique.
    pub fn new(backend: Box<dyn DocumentBackend + 'a>, id_seed: u64) -> PdfDocument<'a> {
        PdfDocument {
            backend,
            revision: DocumentRevision::INITIAL,
            ids: IdGenerator::new(id_seed),
            dirty: false,
        }
    }

    pub fn info(&self) -> &OpenDocumentInfo {
        self.backend.info()
    }

    pub fn revision(&self) -> DocumentRevision {
        self.revision
    }

    /// Has anything been changed since the document was opened?
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn page_count(&self) -> usize {
        self.backend.page_count()
    }

    pub fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry> {
        self.check_page(page)?;
        self.backend.page_geometry(page)
    }

    pub fn annotations(&self, page: PageIndex) -> Result<Vec<AnnotationSummary>> {
        self.check_page(page)?;
        let annotations = self.backend.annotations(page)?;
        limits::within(
            "annotations on a page",
            annotations.len(),
            limits::MAX_ANNOTATIONS_PER_PAGE,
        )?;
        Ok(annotations)
    }

    pub fn annotation(&self, id: &AnnotationId) -> Result<AnnotationSummary> {
        self.backend.annotation(id)
    }

    pub fn select_text(
        &self,
        page: PageIndex,
        selection: TextSelection,
    ) -> Result<TextSelectionResult> {
        self.check_page(page)?;
        let mut result = self.backend.select_text(page, selection)?;
        // A read-only query, so it never increments the revision (§6.3); and
        // it is bounded here rather than by whoever asked, because the page's
        // text layer is document-controlled input.
        if result.quads.len() > limits::MAX_QUADS_PER_SELECTION {
            result.quads.truncate(limits::MAX_QUADS_PER_SELECTION);
            result.truncated = true;
        }
        if result.text.len() > limits::MAX_TEXT_BYTES {
            let cut = floor_char_boundary(&result.text, limits::MAX_TEXT_BYTES);
            result.text.truncate(cut);
            result.truncated = true;
        }
        Ok(result)
    }

    pub fn fields(&self) -> Result<Vec<FormField>> {
        let fields = self.backend.fields()?;
        limits::within("form fields", fields.len(), limits::MAX_FORM_FIELDS)?;
        Ok(fields)
    }

    /// Apply one atomic user action.
    ///
    /// `expected` is the optimistic revision check (§9.5): a request that does
    /// not match the document's current revision fails with
    /// [`DocumentError::RevisionConflict`] and performs no mutation, which is
    /// what stops a delayed UI message from overwriting a later change.
    ///
    /// The transaction is applied whole or not at all. Everything that can be
    /// checked without touching the document is checked first; anything that
    /// fails afterwards is rolled back through the inverse built so far.
    pub fn apply(
        &mut self,
        expected: DocumentRevision,
        transaction: DocumentTransaction,
    ) -> Result<Applied> {
        if expected != self.revision {
            return Err(DocumentError::RevisionConflict {
                expected,
                actual: self.revision,
            });
        }
        if !self.info().level.allows_annotation()
            || self
                .info()
                .warnings
                .contains(&DocumentWarning::MutationForbidden)
        {
            return Err(DocumentError::MutationForbidden);
        }
        if transaction.is_empty() {
            return Err(DocumentError::Backend("an empty transaction".into()));
        }
        transaction.validate()?;
        self.precheck(&transaction)?;

        let label = transaction.label();
        let mut effects: Vec<AppliedEffect> = Vec::with_capacity(transaction.len());
        let mut undo: Vec<UndoOperation> = Vec::with_capacity(transaction.len());
        let mut dirty_pages: Vec<PageIndex> = Vec::new();
        let mut dirty_region: Option<PageRect> = None;

        for command in &transaction.0 {
            match self.apply_one(command) {
                Ok(step) => {
                    if let Some(page) = step.page {
                        if !dirty_pages.contains(&page) {
                            dirty_pages.push(page);
                        }
                    }
                    if let Some(rect) = step.region {
                        dirty_region = Some(match dirty_region {
                            None => rect,
                            Some(current) => current.union(&rect),
                        });
                    }
                    effects.push(step.effect);
                    undo.push(step.undo);
                }
                Err(error) => {
                    // Atomic at the document-model level (§9.5): put back
                    // everything this transaction already did, newest first.
                    self.roll_back(&undo);
                    return Err(error);
                }
            }
        }

        undo.reverse();
        let restores = self.revision;
        self.revision = self.revision.next();
        self.dirty = true;
        Ok(Applied {
            effects,
            document_revision: self.revision,
            dirty_region,
            dirty_pages,
            undo: DocumentUndo {
                operations: undo,
                restores,
                label,
            },
        })
    }

    /// Apply an undo — itself a mutation, so it increments the revision and
    /// returns the operation that redoes it (§6.2).
    pub fn undo(&mut self, expected: DocumentRevision, undo: DocumentUndo) -> Result<Applied> {
        if expected != self.revision {
            return Err(DocumentError::RevisionConflict {
                expected,
                actual: self.revision,
            });
        }
        let mut effects = Vec::with_capacity(undo.operations.len());
        let mut redo = Vec::with_capacity(undo.operations.len());
        let mut dirty_pages: Vec<PageIndex> = Vec::new();
        let mut dirty_region: Option<PageRect> = None;

        for operation in &undo.operations {
            match self.apply_undo_operation(operation) {
                Ok(step) => {
                    if let Some(page) = step.page {
                        if !dirty_pages.contains(&page) {
                            dirty_pages.push(page);
                        }
                    }
                    if let Some(rect) = step.region {
                        dirty_region = Some(match dirty_region {
                            None => rect,
                            Some(current) => current.union(&rect),
                        });
                    }
                    effects.push(step.effect);
                    redo.push(step.undo);
                }
                Err(error) => {
                    self.roll_back(&redo);
                    return Err(error);
                }
            }
        }

        redo.reverse();
        let restores = self.revision;
        self.revision = self.revision.next();
        self.dirty = true;
        Ok(Applied {
            effects,
            document_revision: self.revision,
            dirty_region,
            dirty_pages,
            undo: DocumentUndo {
                operations: redo,
                restores,
                label: undo.label,
            },
        })
    }

    /// Write the document to `destination` (§11.3).
    ///
    /// The source path is rejected outright (A6): document mode saves through
    /// Save As, and an in-place save is a separate specification with a
    /// recoverable atomic replacement of its own.
    pub fn save_as(&mut self, destination: &Path, options: SaveOptions) -> Result<SavedDocument> {
        if let Some(source) = self.backend.source() {
            if same_file(source, destination) {
                return Err(DocumentError::SourceIsDestination);
            }
        }
        let bytes = self.backend.write_to(destination, options)?;
        Ok(SavedDocument {
            path: destination.to_path_buf(),
            revision: self.revision,
            bytes,
            verified_ids: Vec::new(),
        })
    }

    /// The identities this session has handed out, for diagnostics.
    pub fn issued_ids(&self) -> u64 {
        self.ids.issued()
    }

    fn check_page(&self, page: PageIndex) -> Result<()> {
        let count = self.backend.page_count();
        if page.get() >= count {
            return Err(DocumentError::NoSuchPage {
                page: page.get(),
                count,
            });
        }
        Ok(())
    }

    /// Everything that can be refused without touching the document.
    ///
    /// Doing this first is what makes the common failure — a stroke off the
    /// page, a field that does not exist — cost nothing and leave nothing to
    /// roll back.
    fn precheck(&self, transaction: &DocumentTransaction) -> Result<()> {
        for command in &transaction.0 {
            match command {
                DocumentCommand::Annotation(annotation) => {
                    if let Some(draft) = annotation.draft() {
                        self.check_page(draft.page())?;
                        let geometry = self.backend.page_geometry(draft.page())?;
                        draft.validate(&geometry)?;
                    }
                    if let Some(id) = annotation.id() {
                        let summary = self.backend.annotation(id)?;
                        if !summary.editable() {
                            return Err(DocumentError::NotEditable(id.clone()));
                        }
                    }
                }
                DocumentCommand::SetField { name, .. } => {
                    if !self.info().level.allows_form_filling() {
                        return Err(DocumentError::MutationForbidden);
                    }
                    let fields = self.backend.fields()?;
                    let field = fields
                        .iter()
                        .find(|field| field.name == *name)
                        .ok_or_else(|| DocumentError::NoSuchField(name.clone()))?;
                    if !field.is_editable() {
                        return Err(DocumentError::MutationForbidden);
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_one(&mut self, command: &DocumentCommand) -> Result<Step> {
        match command {
            DocumentCommand::Annotation(AnnotationCommand::Create(draft)) => {
                let id = self.ids.next_id();
                let summary = self.backend.create(&id, draft)?;
                Ok(Step {
                    page: Some(summary.page),
                    region: Some(summary.bounds),
                    effect: AppliedEffect::Annotation(Box::new(summary)),
                    undo: UndoOperation::DeleteAnnotation { id },
                })
            }
            DocumentCommand::Annotation(AnnotationCommand::Replace { id, replacement }) => {
                let before = self.backend.before_image(id)?;
                let previous = self.backend.annotation(id)?.bounds;
                let summary = self.backend.replace(id, replacement)?;
                Ok(Step {
                    page: Some(summary.page),
                    // Both rectangles: the mark has to be lifted off where it
                    // was as well as drawn where it now is.
                    region: Some(previous.union(&summary.bounds)),
                    effect: AppliedEffect::Annotation(Box::new(summary)),
                    undo: UndoOperation::ReplaceAnnotation {
                        id: id.clone(),
                        before: Box::new(before),
                    },
                })
            }
            DocumentCommand::Annotation(AnnotationCommand::Delete { id }) => {
                let bounds = self.backend.annotation(id)?.bounds;
                let before = self.backend.delete(id)?;
                Ok(Step {
                    page: Some(before.page),
                    region: Some(bounds),
                    effect: AppliedEffect::Deleted(id.clone()),
                    undo: UndoOperation::RestoreAnnotation {
                        id: id.clone(),
                        before: Box::new(before),
                    },
                })
            }
            DocumentCommand::SetField { name, value } => {
                let previous = self.backend.field_value(name)?;
                let taken = self.backend.set_field(name, value)?;
                let widget = self
                    .backend
                    .fields()?
                    .into_iter()
                    .find(|field| field.name == *name)
                    .and_then(|field| field.widgets.first().cloned());
                Ok(Step {
                    page: widget.as_ref().map(|widget| widget.page),
                    region: widget.map(|widget| widget.bounds),
                    effect: AppliedEffect::Field {
                        name: name.clone(),
                        value: taken,
                    },
                    undo: UndoOperation::SetField {
                        name: name.clone(),
                        value: previous,
                    },
                })
            }
        }
    }

    fn apply_undo_operation(&mut self, operation: &UndoOperation) -> Result<Step> {
        match operation {
            UndoOperation::DeleteAnnotation { id } => {
                let bounds = self.backend.annotation(id)?.bounds;
                let before = self.backend.delete(id)?;
                Ok(Step {
                    page: Some(before.page),
                    region: Some(bounds),
                    effect: AppliedEffect::Deleted(id.clone()),
                    undo: UndoOperation::RestoreAnnotation {
                        id: id.clone(),
                        before: Box::new(before),
                    },
                })
            }
            UndoOperation::RestoreAnnotation { id, before } => {
                let summary = self.backend.restore(id, before)?;
                Ok(Step {
                    page: Some(summary.page),
                    region: Some(summary.bounds),
                    effect: AppliedEffect::Annotation(Box::new(summary)),
                    undo: UndoOperation::DeleteAnnotation { id: id.clone() },
                })
            }
            UndoOperation::ReplaceAnnotation { id, before } => {
                let current = self.backend.before_image(id)?;
                let previous = self.backend.annotation(id)?.bounds;
                let summary = self.backend.restore(id, before)?;
                Ok(Step {
                    page: Some(summary.page),
                    region: Some(previous.union(&summary.bounds)),
                    effect: AppliedEffect::Annotation(Box::new(summary)),
                    undo: UndoOperation::ReplaceAnnotation {
                        id: id.clone(),
                        before: Box::new(current),
                    },
                })
            }
            UndoOperation::SetField { name, value } => {
                let previous = self.backend.field_value(name)?;
                let taken = self.backend.set_field(name, value)?;
                Ok(Step {
                    page: None,
                    region: None,
                    effect: AppliedEffect::Field {
                        name: name.clone(),
                        value: taken,
                    },
                    undo: UndoOperation::SetField {
                        name: name.clone(),
                        value: previous,
                    },
                })
            }
        }
    }

    /// Undo the steps of a transaction that failed part-way, newest first.
    ///
    /// Best effort by necessity: if the backend is refusing writes, the
    /// rollback cannot succeed either. It is still attempted, because the
    /// alternative is leaving half a sweep applied and telling the caller
    /// nothing happened.
    fn roll_back(&mut self, done: &[UndoOperation]) {
        for operation in done.iter().rev() {
            if let Err(error) = self.apply_undo_operation(operation) {
                tracing::error!(
                    %error,
                    "a transaction could not be rolled back; the document may be part-applied"
                );
            }
        }
    }
}

/// What applying one command produced.
struct Step {
    page: Option<PageIndex>,
    region: Option<PageRect>,
    effect: AppliedEffect,
    undo: UndoOperation,
}

/// Are these the same file on disk?
///
/// Compared by canonical path where both resolve, and by literal path
/// otherwise — a destination that does not exist yet cannot be canonicalised,
/// and that is the normal case for a Save As.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The largest index at or below `at` that does not split a character.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut index = at.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::memory::MemoryDocument;
    use super::*;
    use pulpit_core::annotate::{InkDraft, InkPoint, MarkStyle};
    use pulpit_core::page::PagePoint;

    fn document() -> PdfDocument<'static> {
        PdfDocument::new(Box::new(MemoryDocument::letter(3)), 99)
    }

    fn ink(page: usize, x: f32) -> DocumentCommand {
        DocumentCommand::Annotation(AnnotationCommand::Create(AnnotationDraft::Ink(InkDraft {
            page: PageIndex(page),
            points: vec![InkPoint::new(x, 100.0), InkPoint::new(x + 50.0, 140.0)],
            style: MarkStyle::default(),
        })))
    }

    fn created_id(applied: &Applied) -> AnnotationId {
        match &applied.effects[0] {
            AppliedEffect::Annotation(summary) => summary.id.clone(),
            other => panic!("expected an annotation, got {other:?}"),
        }
    }

    #[test]
    fn a_new_document_is_clean_at_revision_zero() {
        let document = document();
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
        assert!(!document.is_dirty());
        assert_eq!(document.page_count(), 3);
    }

    #[test]
    fn a_committed_gesture_is_one_revision_and_one_undo_entry() {
        let mut document = document();
        let applied = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        assert_eq!(applied.document_revision, DocumentRevision(1));
        assert_eq!(document.revision(), DocumentRevision(1));
        assert!(document.is_dirty());
        assert_eq!(applied.effects.len(), 1);
        assert_eq!(applied.undo.operations.len(), 1);
        assert_eq!(applied.dirty_pages, vec![PageIndex(0)]);
        assert!(applied.dirty_region.is_some());
        assert_eq!(document.annotations(PageIndex(0)).unwrap().len(), 1);
    }

    #[test]
    fn a_committed_annotation_is_in_the_document_and_nowhere_else() {
        // A1: the summary is a view of the document, so deleting through the
        // engine removes it from the enumeration with no second store to
        // forget to update.
        let mut document = document();
        let applied = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        let id = created_id(&applied);
        let summary = document.annotation(&id).unwrap();
        assert_eq!(summary.page, PageIndex(0));
        assert!(
            summary.id.looks_generated(),
            "a created mark carries an /NM"
        );

        document
            .apply(
                document.revision(),
                DocumentTransaction::from_annotations([AnnotationCommand::Delete {
                    id: id.clone(),
                }]),
            )
            .unwrap();
        assert!(document.annotations(PageIndex(0)).unwrap().is_empty());
        assert!(document.annotation(&id).is_err());
    }

    #[test]
    fn a_stale_expected_revision_conflicts_and_changes_nothing() {
        let mut document = document();
        document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        let error = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 200.0)),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            DocumentError::RevisionConflict {
                expected: DocumentRevision(0),
                actual: DocumentRevision(1)
            }
        ));
        assert_eq!(document.revision(), DocumentRevision(1));
        assert_eq!(document.annotations(PageIndex(0)).unwrap().len(), 1);
    }

    #[test]
    fn a_failed_command_leaves_no_part_of_its_transaction_applied() {
        let mut document = document();
        // The first stroke is fine; the second names a page that is not there.
        let transaction = DocumentTransaction(vec![ink(0, 100.0), ink(99, 100.0)]);
        let error = document
            .apply(DocumentRevision::INITIAL, transaction)
            .unwrap_err();
        assert!(matches!(error, DocumentError::NoSuchPage { .. }));
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
        assert!(!document.is_dirty(), "a refused action does not dirty");
        assert!(document.annotations(PageIndex(0)).unwrap().is_empty());
    }

    #[test]
    fn a_transaction_that_fails_after_a_write_is_rolled_back() {
        // The precheck cannot catch this one: the delete is legal when the
        // transaction is checked and illegal by the time it runs, because the
        // command before it removed the same annotation.
        let mut document = document();
        let applied = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        let id = created_id(&applied);
        let before = document.annotations(PageIndex(0)).unwrap().len();

        let doomed = DocumentTransaction::from_annotations([
            AnnotationCommand::Delete { id: id.clone() },
            AnnotationCommand::Delete { id: id.clone() },
        ]);
        assert!(document.apply(document.revision(), doomed).is_err());
        assert_eq!(
            document.annotations(PageIndex(0)).unwrap().len(),
            before,
            "the first delete was put back"
        );
        assert_eq!(document.revision(), DocumentRevision(1));
    }

    #[test]
    fn an_undo_restores_the_annotation_rather_than_recreating_it() {
        // A3: an undo/redo cycle must not rename anything.
        let mut document = document();
        let applied = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        let id = created_id(&applied);

        let deleted = document
            .apply(
                document.revision(),
                DocumentTransaction::from_annotations([AnnotationCommand::Delete {
                    id: id.clone(),
                }]),
            )
            .unwrap();
        assert!(document.annotation(&id).is_err());

        let undone = document.undo(document.revision(), deleted.undo).unwrap();
        assert_eq!(
            document.annotation(&id).unwrap().id,
            id,
            "the mark came back under its own name"
        );

        // …and redo is the undo request carrying what the undo returned.
        let redone = document.undo(document.revision(), undone.undo).unwrap();
        assert!(document.annotation(&id).is_err());
        assert_eq!(redone.document_revision, DocumentRevision(4));
    }

    #[test]
    fn undo_and_redo_increment_the_revision_like_any_other_mutation() {
        let mut document = document();
        let applied = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        assert_eq!(document.revision(), DocumentRevision(1));
        let undone = document.undo(document.revision(), applied.undo).unwrap();
        assert_eq!(undone.document_revision, DocumentRevision(2));
        let redone = document.undo(document.revision(), undone.undo).unwrap();
        assert_eq!(redone.document_revision, DocumentRevision(3));
    }

    #[test]
    fn an_undo_with_a_stale_revision_conflicts_and_changes_nothing() {
        let mut document = document();
        let applied = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        let error = document
            .undo(DocumentRevision::INITIAL, applied.undo)
            .unwrap_err();
        assert!(matches!(error, DocumentError::RevisionConflict { .. }));
        assert_eq!(document.annotations(PageIndex(0)).unwrap().len(), 1);
    }

    #[test]
    fn one_sweep_is_one_transaction_one_revision_and_one_undo() {
        let mut document = document();
        let mut ids = Vec::new();
        for x in [100.0, 200.0, 300.0] {
            let applied = document
                .apply(document.revision(), DocumentTransaction::one(ink(0, x)))
                .unwrap();
            ids.push(created_id(&applied));
        }
        let at = document.revision();
        let sweep = document
            .apply(
                at,
                DocumentTransaction::from_annotations(
                    ids.iter()
                        .cloned()
                        .map(|id| AnnotationCommand::Delete { id }),
                ),
            )
            .unwrap();
        assert_eq!(sweep.document_revision, at.next());
        assert_eq!(sweep.effects.len(), 3);
        assert!(document.annotations(PageIndex(0)).unwrap().is_empty());

        document.undo(document.revision(), sweep.undo).unwrap();
        assert_eq!(
            document.annotations(PageIndex(0)).unwrap().len(),
            3,
            "one undo puts the whole sweep back"
        );
    }

    #[test]
    fn a_sweeps_dirty_region_covers_every_mark_it_took() {
        let mut document = document();
        let mut ids = Vec::new();
        for x in [100.0, 400.0] {
            let applied = document
                .apply(document.revision(), DocumentTransaction::one(ink(0, x)))
                .unwrap();
            ids.push(created_id(&applied));
        }
        let sweep = document
            .apply(
                document.revision(),
                DocumentTransaction::from_annotations(
                    ids.into_iter().map(|id| AnnotationCommand::Delete { id }),
                ),
            )
            .unwrap();
        let region = sweep.dirty_region.unwrap();
        assert!(region.left < 105.0 && region.right > 445.0, "{region:?}");
    }

    #[test]
    fn field_and_annotation_edits_share_one_history_in_user_action_order() {
        // §8.6: a field edit followed by an ink stroke undoes the stroke first.
        let mut document = PdfDocument::new(Box::new(MemoryDocument::with_form()), 5);
        let filled = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(DocumentCommand::SetField {
                    name: "name".into(),
                    value: "Ada".into(),
                }),
            )
            .unwrap();
        let stroked = document
            .apply(document.revision(), DocumentTransaction::one(ink(0, 100.0)))
            .unwrap();

        document.undo(document.revision(), stroked.undo).unwrap();
        assert!(
            document.annotations(PageIndex(0)).unwrap().is_empty(),
            "the stroke went first"
        );
        assert_eq!(
            document.fields().unwrap()[0].value,
            "Ada",
            "the field edit is still there"
        );
        document.undo(document.revision(), filled.undo).unwrap();
        assert_eq!(document.fields().unwrap()[0].value, "");
    }

    #[test]
    fn a_field_that_does_not_exist_refuses_before_anything_is_written() {
        let mut document = PdfDocument::new(Box::new(MemoryDocument::with_form()), 5);
        let error = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(DocumentCommand::SetField {
                    name: "nowhere".into(),
                    value: "x".into(),
                }),
            )
            .unwrap_err();
        assert!(matches!(error, DocumentError::NoSuchField(_)));
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
    }

    #[test]
    fn a_read_only_field_is_not_editable() {
        let mut document = PdfDocument::new(Box::new(MemoryDocument::with_form()), 5);
        let error = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(DocumentCommand::SetField {
                    name: "locked".into(),
                    value: "x".into(),
                }),
            )
            .unwrap_err();
        assert!(matches!(error, DocumentError::MutationForbidden));
    }

    #[test]
    fn a_mark_off_the_page_is_refused_and_nothing_is_written() {
        let mut document = document();
        let bad = DocumentCommand::Annotation(AnnotationCommand::Create(AnnotationDraft::Ink(
            InkDraft {
                page: PageIndex(0),
                points: vec![InkPoint::new(90_000.0, 10.0)],
                style: MarkStyle::default(),
            },
        )));
        let error = document
            .apply(DocumentRevision::INITIAL, DocumentTransaction::one(bad))
            .unwrap_err();
        assert!(matches!(error, DocumentError::Rejected(_)));
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
    }

    #[test]
    fn an_empty_transaction_is_not_a_mutation() {
        let mut document = document();
        assert!(document
            .apply(DocumentRevision::INITIAL, DocumentTransaction::default())
            .is_err());
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
    }

    #[test]
    fn a_transaction_past_the_limits_is_refused_before_allocation() {
        let mut document = document();
        let transaction = DocumentTransaction(vec![
            ink(0, 10.0);
            limits::MAX_OPERATIONS_PER_TRANSACTION + 1
        ]);
        assert!(matches!(
            document.apply(DocumentRevision::INITIAL, transaction),
            Err(DocumentError::Limit(_))
        ));
    }

    #[test]
    fn selection_is_read_only_and_never_moves_the_revision() {
        let document = document();
        let result = document
            .select_text(
                PageIndex(0),
                TextSelection::Range {
                    anchor: PagePoint::new(10.0, 10.0),
                    head: PagePoint::new(100.0, 10.0),
                },
            )
            .unwrap();
        // The in-memory document has no text layer, which is an empty result
        // rather than an error (§6.3).
        assert!(result.is_empty());
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
    }

    #[test]
    fn a_page_that_is_not_there_is_named_in_the_error() {
        let document = document();
        let error = document.annotations(PageIndex(9)).unwrap_err();
        assert!(matches!(
            error,
            DocumentError::NoSuchPage { page: 9, count: 3 }
        ));
    }

    #[test]
    fn the_source_file_is_never_a_destination() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("in.pdf");
        std::fs::write(&source, b"%PDF-1.7\n").unwrap();
        let mut document = PdfDocument::new(
            Box::new(MemoryDocument::letter(1).with_source(source.clone())),
            1,
        );
        assert!(matches!(
            document.save_as(&source, SaveOptions::verified()),
            Err(DocumentError::SourceIsDestination)
        ));
        // …and an ordinary destination beside it is fine.
        let out = directory.path().join("out.pdf");
        assert!(document.save_as(&out, SaveOptions::verified()).is_ok());
        assert!(out.exists());
    }

    #[test]
    fn a_document_that_forbids_mutation_refuses_every_edit() {
        let mut document = PdfDocument::new(Box::new(MemoryDocument::locked()), 1);
        assert!(matches!(
            document.apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 10.0))
            ),
            Err(DocumentError::MutationForbidden)
        ));
    }

    #[test]
    fn text_is_truncated_on_a_character_boundary() {
        assert_eq!(floor_char_boundary("héllo", 2), 1);
        assert_eq!(floor_char_boundary("hello", 3), 3);
        assert_eq!(floor_char_boundary("hi", 99), 2);
    }

    /// Acceptance criterion 17, and §5.4's rule that presentation scheduling
    /// stays out of the document API.
    ///
    /// This test exists to fail at *compile* time if the surface ever grows a
    /// scheduling parameter: it opens, annotates, enumerates, fills, undoes
    /// and saves without naming `RenderGeneration`, `Priority` or `Quality`.
    #[test]
    fn the_document_api_needs_no_presentation_scheduling_types() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = PdfDocument::new(Box::new(MemoryDocument::with_form()), 11);
        let applied = document
            .apply(document.revision(), DocumentTransaction::one(ink(0, 40.0)))
            .unwrap();
        document
            .apply(
                document.revision(),
                DocumentTransaction::one(DocumentCommand::SetField {
                    name: "name".into(),
                    value: "Ada".into(),
                }),
            )
            .unwrap();
        let _ = document.annotations(PageIndex(0)).unwrap();
        let _ = document.fields().unwrap();
        document.undo(document.revision(), applied.undo).unwrap();
        let saved = document
            .save_as(&directory.path().join("out.pdf"), SaveOptions::verified())
            .unwrap();
        assert_eq!(saved.revision, document.revision());
    }
}
