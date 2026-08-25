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
pub mod form;
#[cfg(feature = "pdfium")]
pub mod pdfium;

use std::path::Path;

use pulpit_core::annotate::{AnnotationCommand, AnnotationDraft, AnnotationId, IdGenerator};
use pulpit_core::page::{PageGeometry, PageIndex, PageRect};
use pulpit_core::search::HitChunk;

pub use limits::LimitExceeded;
pub use model::describe_page_size;
pub use model::{
    AnnotationBeforeImage, AnnotationContents, AnnotationSummary, AnnotationSupport, Applied,
    AppliedEffect, CivilTime, CompatibilityLevel, DocumentCommand, DocumentDate,
    DocumentPermissions, DocumentProperties, DocumentRevision, DocumentTransaction, DocumentUndo,
    DocumentWarning, Encryption, FieldFormat, FieldKind, FieldWidget, FormField, InfoText,
    OpenDocumentInfo, PageSizes, PdfVersion, SaveOptions, SavedDocument, TextSelection,
    TextSelectionResult, UndoOperation, MAX_PAGES_MEASURED_FOR_PROPERTIES,
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
    /// The document marks this field read-only.
    ///
    /// Its own variant rather than a [`Self::MutationForbidden`] or a
    /// [`Self::Backend`] string, because the two engines used to answer this
    /// one question differently — the fixture with `MutationForbidden`, PDFium
    /// with a formatted `Backend` — and a test written against one would then
    /// pass against an application that behaves differently under the other.
    #[error("the field {0} is read-only")]
    FieldReadOnly(String),
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
    #[error("this document cannot {0}")]
    Unsupported(String),
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
/// Deliberately **not** `Send`. PDFium's form-fill environment has thread
/// affinity — its V8 isolate is entered on the thread that created it, and
/// moving form work to another thread segfaults rather than failing cleanly.
/// One document is owned by one execution context (§6), and leaving this
/// trait un-`Send` is what makes the compiler enforce it.
pub trait DocumentBackend {
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

    /// One field by name, or `None` when the document has no such field.
    ///
    /// Separate from [`Self::fields`] because the cost is different by a
    /// factor of the document: listing every field reads every widget on every
    /// page and builds each one's option list, format script and export
    /// values, and a lookup by name needs one string per widget to know it is
    /// looking at the wrong one. Every commit does at least two of these — the
    /// before-image and the read-back — so on a two-hundred-page form the
    /// difference is what a keystroke costs.
    ///
    /// The default is the honest slow answer, so a backend only overrides it
    /// when it has a faster one.
    fn field(&self, name: &str) -> Result<Option<FormField>> {
        Ok(self.fields()?.into_iter().find(|field| field.name == name))
    }

    /// The bookmark tree. A document without one, or a backend that cannot
    /// read one, reports an empty outline: the rail then has nothing to show,
    /// which is also what a document without bookmarks looks like.
    fn outline(&self) -> Result<pulpit_core::navigation::Outline> {
        Ok(pulpit_core::navigation::Outline::default())
    }

    /// What the document says about itself, for the properties view.
    ///
    /// The default is the shape of the document and nothing else, which is the
    /// honest answer for a backend with no `/Info` dictionary to read: a
    /// folder of images has no title, no producer and no PDF version, and
    /// blanks invented for them would read as a document that left them empty.
    fn properties(&self) -> Result<DocumentProperties> {
        Ok(DocumentProperties::from_info(self.info()))
    }

    /// Write a field, returning the value the document actually took.
    ///
    /// `selected` names the target selection, by option index, for a choice
    /// field that takes several — the one shape of value a string cannot
    /// carry. Empty for everything else, and a backend may derive a single
    /// selection from `value` when it is.
    fn set_field(&mut self, name: &str, value: &str, selected: &[u32]) -> Result<String>;

    fn field_value(&self, name: &str) -> Result<String>;

    /// Forward one raw input event to the engine's form-fill environment
    /// (§8.6).
    ///
    /// A backend that has no such environment refuses, and says what is
    /// missing rather than answering "nothing happened" — a form that silently
    /// swallows keystrokes is worse than one that says it cannot take them.
    ///
    /// The `revision` in any committed field is left at
    /// [`DocumentRevision::INITIAL`]: revisions belong to [`PdfDocument`],
    /// which stamps the real one when it bumps it.
    fn form_event(
        &mut self,
        _page: PageIndex,
        _event: crate::document::protocol::FormInputEvent,
    ) -> Result<crate::document::protocol::FormEventResult> {
        Err(DocumentError::Backend(
            "this engine has no form-fill environment".into(),
        ))
    }

    fn select_text(&self, page: PageIndex, selection: TextSelection)
        -> Result<TextSelectionResult>;

    /// The text inside `rect` on `page`, in the page's own coordinates.
    ///
    /// [`DocumentError::Unsupported`] by default rather than an empty string,
    /// for the same reason [`DocumentBackend::find_text`] is: a backend with
    /// no text layer and a rectangle drawn over a photograph are different
    /// facts, and the person who dragged the band has to be told which one
    /// they got.
    fn area_text(&self, _page: PageIndex, _rect: pulpit_core::page::PageRect) -> Result<String> {
        Err(DocumentError::Unsupported(
            "have its text read by area: this backend has no text layer".into(),
        ))
    }

    /// Find `query` in the text layer of `pages`, a half-open range.
    ///
    /// The default is [`DocumentError::Unsupported`] rather than an empty
    /// answer, because a backend with no text layer at all and a document
    /// with no matches are different facts and the person who typed the
    /// query needs to be told which one they have.
    fn find_text(
        &self,
        _query: &pulpit_core::search::Query,
        _pages: std::ops::Range<usize>,
    ) -> Result<pulpit_core::search::HitChunk> {
        Err(DocumentError::Unsupported(
            "be searched: this backend has no text layer".into(),
        ))
    }

    /// Write the document to `destination`. The caller has already checked
    /// that it is not the source (A6) and has staged a temporary path.
    fn write_to(&mut self, destination: &Path, options: SaveOptions) -> Result<u64>;

    /// The path this document was opened from, so a save can refuse it.
    fn source(&self) -> Option<&Path>;

    /// Rasterise one page into `rgba`, which the caller has sized.
    ///
    /// `full_size` is the full page's pixel size when the caller knows it, so
    /// a crop lands on exactly the scale the frame it is composited into was
    /// drawn at rather than one derived by rounding (§9.4).
    ///
    /// Here rather than in the render worker pool because a frame has to
    /// contain the annotation that was just committed, and only the engine
    /// holding the mutated document can promise that (A7). A backend with no
    /// rasteriser says so rather than drawing a placeholder that the reader
    /// might mistake for their page.
    fn render_page(
        &self,
        _page: PageIndex,
        _region: pulpit_core::notes::Region,
        _width: u32,
        _height: u32,
        _full_size: Option<(u32, u32)>,
        _rgba: &mut [u8],
    ) -> Result<()> {
        Err(DocumentError::Backend(
            "this engine cannot rasterise a page".into(),
        ))
    }
}

/// A worker-confined open document (§6).
///
/// The lifetime is the engine's: the PDFium engine borrows a binding that is
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

    /// The text a rectangle covers, bounded the way a selection's is.
    ///
    /// Returns the text and whether it had to be cut. Read-only, so it never
    /// touches the revision (§6.3), and bounded here rather than by whoever
    /// asked, because a page's text layer is document-controlled input.
    pub fn area_text(
        &self,
        page: PageIndex,
        rect: pulpit_core::page::PageRect,
    ) -> Result<(String, bool)> {
        self.check_page(page)?;
        let mut text = self.backend.area_text(page, rect)?;
        let mut truncated = false;
        if text.len() > limits::MAX_TEXT_BYTES {
            let cut = floor_char_boundary(&text, limits::MAX_TEXT_BYTES);
            text.truncate(cut);
            truncated = true;
        }
        Ok((text, truncated))
    }

    /// Find a string in a run of pages.
    ///
    /// Read-only, so it never touches the revision (§6.3). The range is
    /// clamped to the document rather than refused: the caller's page count
    /// can be one reload behind, and a scan that walks off the end should
    /// stop, not fail.
    pub fn find_text(
        &self,
        query: &pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) -> Result<pulpit_core::search::HitChunk> {
        let count = self.backend.page_count();
        let from = pages.start.min(count);
        let to = pages.end.clamp(from, count);
        limits::within(
            "pages in one search",
            to - from,
            limits::MAX_PAGES_PER_SEARCH,
        )?;
        if query.is_empty() {
            return Ok(HitChunk {
                from_page: from,
                to_page: to,
                ..HitChunk::default()
            });
        }
        let mut chunk = self.backend.find_text(query, from..to)?;
        chunk.from_page = from;
        chunk.to_page = to;
        // Bounded here rather than by whoever asked: the page's text layer is
        // document-controlled input, and these hits are drawn as overlays.
        if chunk.hits.len() > limits::MAX_HITS_PER_SEARCH {
            chunk.hits.truncate(limits::MAX_HITS_PER_SEARCH);
            chunk.truncated = true;
        }
        for hit in &mut chunk.hits {
            if hit.quads.len() > limits::MAX_QUADS_PER_HIT {
                hit.quads.truncate(limits::MAX_QUADS_PER_HIT);
            }
        }
        Ok(chunk)
    }

    /// The document's bookmark tree, for the outline rail.
    pub fn outline(&self) -> Result<pulpit_core::navigation::Outline> {
        self.backend.outline()
    }

    /// What the document says about itself, for the properties view.
    ///
    /// Bounded here as well as in the backend: every string is
    /// document-controlled, and this is the boundary the answer leaves the
    /// engine through.
    pub fn properties(&self) -> Result<DocumentProperties> {
        let properties = self.backend.properties()?;
        for value in properties.strings() {
            limits::within(
                "an /Info string",
                value.text.len(),
                limits::MAX_INFO_TEXT_BYTES,
            )?;
        }
        Ok(properties)
    }

    pub fn fields(&self) -> Result<Vec<FormField>> {
        let fields = self.backend.fields()?;
        limits::within("form fields", fields.len(), limits::MAX_FORM_FIELDS)?;
        Ok(fields)
    }

    /// One field by name, without reading every other one.
    pub fn field(&self, name: &str) -> Result<Option<FormField>> {
        self.backend.field(name)
    }

    /// What one field holds now.
    ///
    /// Read from the engine on every call rather than cached: while a form is
    /// being filled, the value in PDFium's environment is the value, and a
    /// copy of it held anywhere else is the second source of truth §8.6 exists
    /// to avoid.
    pub fn field_value(&self, name: &str) -> Result<String> {
        self.backend.field_value(name)
    }

    /// Write a field without going through the page.
    ///
    /// Present so the trait's surface is reachable, and refused by the engine
    /// that has a form-fill environment (§8.6): values are typed on the page.
    pub fn set_field(&mut self, name: &str, value: &str, selected: &[u32]) -> Result<String> {
        self.backend.set_field(name, value, selected)
    }

    /// Forward one raw input event to the form-fill environment (§8.6).
    ///
    /// Most events change nothing durable — moving the pointer, pressing a
    /// key that the field ignores, putting the caret somewhere. Those return
    /// their invalidated rectangles and leave the revision alone.
    ///
    /// An event that *commits* a value is a document change like any other:
    /// one revision, and one entry in the same undo history as the annotations
    /// so that a field edit followed by an ink stroke undoes the stroke first.
    /// The revision is stamped here, because this is what owns it.
    pub fn form_event(
        &mut self,
        page: PageIndex,
        event: crate::document::protocol::FormInputEvent,
    ) -> Result<crate::document::protocol::FormEventResult> {
        // Fail closed at the boundary, not after the engine has taken the
        // keystroke: a document whose own permissions forbid changing it
        // refuses every event that could commit a value, press a widget or
        // type into a field. The events a *reader* needs — the pointer
        // following the page, focus, copy — still go through, so a locked
        // form is read-only rather than inert.
        if event.can_change_the_document() && !self.allows_mutation() {
            return Err(DocumentError::MutationForbidden);
        }
        let mut result = self.backend.form_event(page, event)?;
        limits::within(
            "invalidated rectangles",
            result.invalidated.len(),
            limits::MAX_DIRTY_RECTS,
        )?;
        if let Some(committed) = result.committed.as_mut() {
            self.revision = DocumentRevision(self.revision.0 + 1);
            self.dirty = true;
            committed.revision = self.revision;
        }
        Ok(result)
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
        self.check_revision(expected)?;
        if !self.allows_mutation() {
            return Err(DocumentError::MutationForbidden);
        }
        if transaction.is_empty() {
            return Err(DocumentError::Backend("an empty transaction".into()));
        }
        transaction.validate()?;
        self.precheck(&transaction)?;

        let label = transaction.label();
        let mut steps = AppliedSteps::with_capacity(transaction.len());

        for command in &transaction.0 {
            match self.apply_one(command) {
                Ok(step) => steps.push(step),
                Err(error) => {
                    // Atomic at the document-model level (§9.5): put back
                    // everything this transaction already did, newest first.
                    self.roll_back(steps.inverse());
                    return Err(error);
                }
            }
        }

        Ok(self.commit(steps, label))
    }

    /// Apply an undo — itself a mutation, so it increments the revision and
    /// returns the operation that redoes it (§6.2).
    pub fn undo(&mut self, expected: DocumentRevision, undo: DocumentUndo) -> Result<Applied> {
        self.check_revision(expected)?;
        let mut steps = AppliedSteps::with_capacity(undo.operations.len());

        for operation in &undo.operations {
            match self.apply_undo_operation(operation) {
                Ok(step) => steps.push(step),
                Err(error) => {
                    self.roll_back(steps.inverse());
                    return Err(error);
                }
            }
        }

        Ok(self.commit(steps, undo.label))
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

    /// Rasterise one page at `width` × `height` physical pixels.
    ///
    /// Allocates the buffer here rather than taking one, because the caller is
    /// a protocol handler answering a request rather than a frame cache with
    /// storage of its own; the size is bounded before the allocation (A8).
    pub fn render_page(
        &self,
        page: PageIndex,
        region: pulpit_core::notes::Region,
        width: u32,
        height: u32,
        full_size: Option<(u32, u32)>,
    ) -> Result<Vec<u8>> {
        self.check_page(page)?;
        if !region.is_valid() {
            return Err(DocumentError::Backend(
                "a render region that is not part of a page".into(),
            ));
        }
        let bytes = u64::from(width) * u64::from(height) * 4;
        // The same ceiling the render path keeps. Checked before the
        // allocation, not after it.
        if width == 0 || height == 0 || bytes > 16_384 * 16_384 * 4 {
            return Err(DocumentError::Backend(format!(
                "a {width}×{height} render is not a page"
            )));
        }
        let mut rgba = vec![0u8; bytes as usize];
        self.backend
            .render_page(page, region, width, height, full_size, &mut rgba)?;
        Ok(rgba)
    }

    /// The identities this session has handed out, for diagnostics.
    pub fn issued_ids(&self) -> u64 {
        self.ids.issued()
    }

    /// May this document be changed at all?
    ///
    /// One answer, consulted by every path that writes — a transaction and a
    /// form event alike — so that an encrypted file's permissions cannot be
    /// enforced on one of them and forgotten on the other.
    fn allows_mutation(&self) -> bool {
        self.info().level.allows_annotation()
            && !self
                .info()
                .warnings
                .contains(&DocumentWarning::MutationForbidden)
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
                    let field = self
                        .backend
                        .field(name)?
                        .ok_or_else(|| DocumentError::NoSuchField(name.clone()))?;
                    if field.truncated {
                        // Writing back what was read would cut off everything
                        // past the read bound, which is an edit nobody asked
                        // for attached to one they did.
                        return Err(DocumentError::Unsupported(format!(
                            "change {name}: its value is longer than pulpit can read"
                        )));
                    }
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
            DocumentCommand::SetField {
                name,
                value,
                selected,
            } => {
                // The whole before-image, not only the value: a multi-select
                // list box's state is its selection, and the string alone
                // restores at most one of three choices.
                let before = self
                    .backend
                    .field(name)?
                    .ok_or_else(|| DocumentError::NoSuchField(name.clone()))?;
                let taken = self.backend.set_field(name, value, selected)?;
                let widget = before.widgets.first().cloned();
                Ok(Step {
                    page: widget.as_ref().map(|widget| widget.page),
                    region: widget.map(|widget| widget.bounds),
                    effect: AppliedEffect::Field {
                        name: name.clone(),
                        value: taken,
                    },
                    undo: UndoOperation::SetField {
                        name: name.clone(),
                        value: before.value,
                        selected: before.selected,
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
            UndoOperation::SetField {
                name,
                value,
                selected,
            } => {
                let current = self
                    .backend
                    .field(name)?
                    .ok_or_else(|| DocumentError::NoSuchField(name.clone()))?;
                let taken = self.backend.set_field(name, value, selected)?;
                Ok(Step {
                    page: None,
                    region: None,
                    effect: AppliedEffect::Field {
                        name: name.clone(),
                        value: taken,
                    },
                    undo: UndoOperation::SetField {
                        name: name.clone(),
                        value: current.value,
                        selected: current.selected,
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
    /// The optimistic revision check (§9.5), made the same way by everything
    /// that mutates through a transaction.
    fn check_revision(&self, expected: DocumentRevision) -> Result<()> {
        if expected != self.revision {
            return Err(DocumentError::RevisionConflict {
                expected,
                actual: self.revision,
            });
        }
        Ok(())
    }

    /// Finish a transaction that every step survived.
    ///
    /// This is the only place the revision moves: a run that failed part way
    /// has been rolled back and returned its error long before here, so a
    /// document that refused a transaction still reads as the revision the
    /// caller had.
    fn commit(&mut self, steps: AppliedSteps, label: String) -> Applied {
        let restores = self.revision;
        self.revision = self.revision.next();
        self.dirty = true;
        steps.into_applied(self.revision, restores, label)
    }

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

/// The steps a transaction has taken so far.
///
/// Applying and undoing differ in which step function they call, not in what a
/// successful step means: the page it touched joins the dirty set once, the
/// rectangle it touched widens the dirty region, and its inverse is stacked in
/// command order so that a failure can be put back newest first. Keeping that
/// bookkeeping here is what lets each loop show only its own semantics.
struct AppliedSteps {
    effects: Vec<AppliedEffect>,
    /// The inverses in *command* order. Reversed exactly once, when the
    /// transaction succeeds and this becomes an undo the caller can replay.
    inverse: Vec<UndoOperation>,
    dirty_pages: Vec<PageIndex>,
    dirty_region: Option<PageRect>,
}

impl AppliedSteps {
    fn with_capacity(steps: usize) -> Self {
        Self {
            effects: Vec::with_capacity(steps),
            inverse: Vec::with_capacity(steps),
            dirty_pages: Vec::new(),
            dirty_region: None,
        }
    }

    /// Record one step that succeeded.
    fn push(&mut self, step: Step) {
        if let Some(page) = step.page {
            if !self.dirty_pages.contains(&page) {
                self.dirty_pages.push(page);
            }
        }
        if let Some(rect) = step.region {
            self.dirty_region = Some(match self.dirty_region {
                None => rect,
                Some(current) => current.union(&rect),
            });
        }
        self.effects.push(step.effect);
        self.inverse.push(step.undo);
    }

    /// What to hand [`PdfDocument::roll_back`], which walks it backwards.
    fn inverse(&self) -> &[UndoOperation] {
        &self.inverse
    }

    /// Turn the accumulated steps into the result of a committed transaction.
    fn into_applied(
        mut self,
        document_revision: DocumentRevision,
        restores: DocumentRevision,
        label: String,
    ) -> Applied {
        self.inverse.reverse();
        Applied {
            effects: self.effects,
            document_revision,
            dirty_region: self.dirty_region,
            dirty_pages: self.dirty_pages,
            undo: DocumentUndo {
                operations: self.inverse,
                restores,
                label,
            },
        }
    }
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
    fn several_steps_that_fail_at_the_end_are_all_put_back() {
        // The accumulator's ordering, at the length where it matters: two
        // commands write, the third cannot, and both writes come back newest
        // first. The precheck cannot see this one coming — the second delete
        // is legal until the first one runs. The revision never moves,
        // because it moves only once every step has succeeded.
        let mut document = document();
        let applied = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(ink(0, 100.0)),
            )
            .unwrap();
        let id = created_id(&applied);
        let revision = document.revision();

        let doomed = DocumentTransaction(vec![
            ink(1, 100.0),
            DocumentCommand::Annotation(AnnotationCommand::Delete { id: id.clone() }),
            DocumentCommand::Annotation(AnnotationCommand::Delete { id: id.clone() }),
        ]);
        assert!(document.apply(revision, doomed).is_err());
        assert_eq!(document.revision(), revision, "a refused transaction");
        assert_eq!(
            document.annotation(&id).unwrap().page,
            PageIndex(0),
            "the delete was put back"
        );
        assert!(
            document.annotations(PageIndex(1)).unwrap().is_empty(),
            "the stroke was put back"
        );
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
                    selected: Vec::new(),
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

    /// A document whose form carries a list box that takes several rows.
    ///
    /// The fixture's own form has no multi-select, and it is the one shape of
    /// value a string cannot carry — which is exactly what the tests below are
    /// about.
    fn document_with_a_multi_select() -> PdfDocument<'static> {
        let mut memory = MemoryDocument::with_form();
        memory.add_field(FormField {
            name: "langues".into(),
            kind: FieldKind::ListBox,
            value: "Français".into(),
            selected: vec![0],
            read_only: false,
            format: Default::default(),
            options: vec![
                "Français".into(),
                "English".into(),
                "Deutsch".into(),
                "Nederlands".into(),
            ],
            allows_custom_value: false,
            multiple_selection: true,
            required: false,
            password: false,
            file_select: false,
            rich_text: false,
            truncated: false,
            hidden: false,
            widgets: vec![FieldWidget {
                page: PageIndex(0),
                bounds: PageRect::new(100.0, 300.0, 400.0, 380.0),
                option: None,
            }],
        });
        PdfDocument::new(Box::new(memory), 5)
    }

    #[test]
    fn a_transaction_that_fills_a_multi_select_names_every_row_it_chose() {
        // The command carries `selected` because the value cannot: a list box
        // holding three choices reports one of them, and a fill recorded as
        // that string alone replays as one tick where there were three. This
        // is the path a journal replay takes after a crash.
        let mut document = document_with_a_multi_select();
        let filled = document
            .apply(
                DocumentRevision::INITIAL,
                DocumentTransaction::one(DocumentCommand::SetField {
                    name: "langues".into(),
                    value: "Français".into(),
                    selected: vec![1, 2, 3],
                }),
            )
            .unwrap();

        let field = document.field("langues").unwrap().expect("the field");
        assert_eq!(
            field.selected,
            vec![1, 2, 3],
            "the rows the command named were not the rows that were chosen"
        );

        // …and the undo puts back the one row that was there before, for the
        // same reason: its before-image carried the selection too.
        document.undo(document.revision(), filled.undo).unwrap();
        assert_eq!(
            document.field("langues").unwrap().unwrap().selected,
            vec![0]
        );
    }

    #[test]
    fn a_field_looked_up_by_name_is_the_field_the_listing_lists() {
        // Two paths to one answer. The default implementation of `field` is
        // the listing filtered, and a backend may override it with something
        // faster — as the PDFium one does — so the contract worth stating is
        // that the two agree, including about a field that is not there.
        let document = document_with_a_multi_select();
        for field in document.fields().unwrap() {
            assert_eq!(
                document.field(&field.name).unwrap().as_ref(),
                Some(&field),
                "{} reads differently one at a time",
                field.name
            );
        }
        assert!(document.field("nowhere").unwrap().is_none());
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
                    selected: Vec::new(),
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
                    selected: Vec::new(),
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
    fn a_document_that_forbids_mutation_refuses_a_value_changing_form_event() {
        use crate::document::protocol::{FormInputEvent, FormKey, KeyModifiers};
        let mut document = PdfDocument::new(Box::new(MemoryDocument::locked()), 1);
        for event in [
            FormInputEvent::Char { character: 'a' },
            FormInputEvent::KeyDown {
                key: FormKey::Backspace,
                modifiers: KeyModifiers::NONE,
            },
            FormInputEvent::ReplaceSelection { text: "Ada".into() },
            FormInputEvent::SelectOption {
                index: 0,
                selected: true,
            },
            FormInputEvent::PointerDown {
                at: PagePoint::new(10.0, 10.0),
            },
            FormInputEvent::PointerUp {
                at: PagePoint::new(10.0, 10.0),
            },
        ] {
            assert!(
                matches!(
                    document.form_event(PageIndex(0), event.clone()),
                    Err(DocumentError::MutationForbidden)
                ),
                "{event:?} was not refused"
            );
        }
        assert_eq!(document.revision(), DocumentRevision::INITIAL);
        assert!(!document.is_dirty());
    }

    #[test]
    fn a_locked_document_still_lets_a_reader_move_the_pointer_and_take_focus() {
        // The refusal is drawn at what can *change* the document. Reading one
        // — hover, focus, copy — is not a mutation and reaches the backend,
        // which here has no form-fill environment and says so.
        use crate::document::protocol::FormInputEvent;
        let mut document = PdfDocument::new(Box::new(MemoryDocument::locked()), 1);
        for event in [
            FormInputEvent::PointerMove {
                at: PagePoint::new(10.0, 10.0),
            },
            FormInputEvent::Focus { gained: true },
            FormInputEvent::CopySelection,
        ] {
            assert!(
                matches!(
                    document.form_event(PageIndex(0), event.clone()),
                    Err(DocumentError::Backend(_))
                ),
                "{event:?} should have reached the backend"
            );
        }
    }

    #[test]
    fn an_unlocked_document_does_not_refuse_a_form_event_on_permissions() {
        // The same event on a document with no such permission is refused by
        // the backend for having no form-fill environment, not by the gate.
        use crate::document::protocol::FormInputEvent;
        let mut document = PdfDocument::new(Box::new(MemoryDocument::with_form()), 5);
        assert!(matches!(
            document.form_event(PageIndex(0), FormInputEvent::Char { character: 'a' }),
            Err(DocumentError::Backend(_))
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
                    selected: Vec::new(),
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
