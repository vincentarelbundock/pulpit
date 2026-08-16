//! The PDFium document engine: native annotations in a real PDF.
//!
//! The other half of [`crate::pdf::pdfium::PdfiumBackend`], which renders. A
//! document is opened once, held by one worker execution context, mutated in
//! place and written out by Save As — never over its own source (A6).
//!
//! # What "editable" means here (§10.2)
//!
//! PDFium exposes annotation dictionary entries by *name*: it can be asked
//! whether `/InkList` is present and what it holds, but it cannot enumerate
//! the keys an annotation actually has. That has one consequence, and this
//! module is shaped around it:
//!
//! * **In-place edits are lossless.** Changing colour, opacity, geometry or
//!   contents mutates the existing dictionary, so every entry pulpit does not
//!   model survives untouched. Imported annotations of a modelled subtype are
//!   therefore [`AnnotationSupport::Editable`].
//! * **Deleting and restoring is lossless only for what can be read back.**
//!   The before-image carries the modelled content plus the annotation's own
//!   appearance stream, which is the entry that actually matters for how the
//!   mark looks in another viewer. A private entry of a kind PDFium cannot
//!   name is lost across a delete/undo cycle, and
//!   [`AnnotationBeforeImage::preserved`] carries what could be recovered
//!   rather than pretending it carried everything.
//!
//! Widening this is a matter of a dictionary-walking binding, not of a
//! different design; until there is one, the classification above is what the
//! engine can honestly promise.

use std::cell::RefCell;
use std::collections::HashMap;

use std::path::{Path, PathBuf};

use pdfium_render::prelude::*;
use pulpit_core::annotate::{
    AnnotationDraft, AnnotationId, AnnotationKind, FreeTextDraft, HighlightDraft, InkDraft,
    MarkStyle, StampDraft, StampMark, TextSource,
};
use pulpit_core::annotation::InkColor;
use pulpit_core::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect};

use crate::pdf::capabilities::{ActionKind, FormType};
use crate::pdf::pdfium::{PdfiumBackend, FPDF_BITMAP_BGRA, FPDF_REVERSE_BYTE_ORDER};
use crate::pdf::{BackendDocumentId, PdfBackend, PdfError};

use super::limits;
use super::model::{
    AnnotationBeforeImage, AnnotationContents, AnnotationSummary, AnnotationSupport,
    CompatibilityLevel, DocumentWarning, FieldKind, FieldWidget, FormField, OpenDocumentInfo,
    SaveOptions, TextSelection, TextSelectionResult,
};
use super::{DocumentBackend, DocumentError, DocumentRevision, Result};

/// The key pulpit's own metadata lives under.
///
/// Namespaced because a second-class key in an annotation dictionary is shared
/// with every other producer that ever touched the file, and a bare `/Source`
/// would be somebody else's within a year.
const PULPIT_KEY: &str = "pulpit_Source";

/// PDFium's `/Subtype` enumerants, named rather than spelled as integers at
/// every call site.
mod subtype {
    use pdfium_render::prelude::*;

    pub const TEXT: FPDF_ANNOTATION_SUBTYPE = 1;
    pub const FREETEXT: FPDF_ANNOTATION_SUBTYPE = 3;
    pub const HIGHLIGHT: FPDF_ANNOTATION_SUBTYPE = 9;
    pub const INK: FPDF_ANNOTATION_SUBTYPE = 15;
    pub const STAMP: FPDF_ANNOTATION_SUBTYPE = 13;
    pub const WIDGET: FPDF_ANNOTATION_SUBTYPE = 20;
}

/// `FPDFANNOT_COLORTYPE_Color`: the annotation's own colour, `/C`, as opposed
/// to its interior colour. Spelled here because the binding crate's prelude
/// does not re-export the enumerants.
const COLOR_TYPE_COLOR: std::os::raw::c_uint = 0;

/// `FPDF_ANNOT_APPEARANCEMODE_NORMAL`: the `/AP` `/N` stream, the one every
/// viewer draws.
const APPEARANCE_NORMAL: std::os::raw::c_int = 0;

/// One open PDF, mutated in place.
///
/// The binding is *borrowed*, not owned: PDFium is bound once per process
/// (see [`PdfiumBackend::bind`]), and the worker that renders is the worker
/// that mutates — §5.1 makes document mutation a capability of the existing
/// worker rather than a new helper. Rendering and mutation therefore go
/// through the same open handle, which is what makes a frame drawn after a
/// commit contain the commit (A7).
pub struct PdfiumDocument<'a> {
    backend: &'a PdfiumBackend,
    document: BackendDocumentId,
    info: OpenDocumentInfo,
    source: Option<PathBuf>,
    /// Page geometry, measured once. A page's crop box and rotation do not
    /// change under an annotation edit, and re-measuring on every hit-test
    /// would reload the page each time.
    /// Interior mutability because measuring is a read: a hit-test, an
    /// enumeration and a precheck all need a page geometry through `&self`,
    /// and the alternative is measuring every page at open on the chance one
    /// of them is looked at.
    geometry: RefCell<HashMap<usize, PageGeometry>>,
    /// PDFium's interactive form-fill environment, and the handle it gave back
    /// (§8.6).
    ///
    /// Initialised for every document that has a form, and *not* only when
    /// events are being forwarded: `FPDF_RenderPageBitmap` alone does not draw
    /// live field contents, so a page with a form needs an `FPDF_FFLDraw` pass
    /// over every render or the values a person has typed are simply absent
    /// from the picture.
    ///
    /// The box must not move and must outlive the handle: PDFium keeps the
    /// address of the environment's first field and calls back through it. It
    /// is dropped in [`PdfiumDocument::close`], after the handle.
    form: Option<FormBinding>,
}

/// The form-fill environment together with the handle that borrows it.
///
/// One struct so the two cannot be separated: dropping the environment while
/// PDFium still holds a handle would leave it calling through freed memory.
struct FormBinding {
    /// Boxed and never moved after `attach`.
    environment: Box<crate::document::form::FormEnvironment>,
    handle: FPDF_FORMHANDLE,
    /// The page a form interaction is currently open on, held loaded.
    ///
    /// This is the one place in the codebase where a native page handle
    /// deliberately outlives the call that made it, and it is worth saying why.
    /// PDFium's form-fill environment keys a field's editing state — which
    /// widget has focus, where the caret is, what has been typed and not yet
    /// committed — to the `FPDF_PAGE` it was told about in
    /// `FORM_OnAfterLoadPage`. Loading the page for each event and closing it
    /// again hands PDFium a different pointer every time, and the focus, the
    /// caret and the half-typed value go with the old one: every keystroke
    /// lands on a form that has just been told nothing is selected.
    ///
    /// So the page stays loaded for the length of an interaction. It is closed
    /// when the interaction moves to another page, when focus is dropped, and
    /// when the document closes — and it is uncommitted state by definition,
    /// which is exactly what §11.5 says a worker crash mid-fill is allowed to
    /// lose.
    open_page: Option<(usize, FPDF_PAGE)>,
}

// PDFium is not thread safe; the whole point of the worker process is that one
// document is owned by one execution context (§6).
unsafe impl Send for PdfiumDocument<'_> {}

impl<'a> PdfiumDocument<'a> {
    /// Open `source` for reading and annotating.
    ///
    /// Opening needs the binding mutably — it registers a handle — and
    /// everything afterwards does not, which is why this takes `&mut` and the
    /// document keeps a shared borrow.
    pub fn open(backend: &'a mut PdfiumBackend, source: &Path) -> Result<PdfiumDocument<'a>> {
        let document = backend
            .open(source)
            .map_err(|error| DocumentError::Backend(error.to_string()))?;
        let mut engine = PdfiumDocument {
            backend,
            document,
            info: OpenDocumentInfo {
                page_count: 0,
                level: CompatibilityLevel::AnnotateOnly,
                warnings: Vec::new(),
                first_page: PageGeometry::default(),
                has_form: false,
            },
            source: Some(source.to_path_buf()),
            geometry: RefCell::new(HashMap::new()),
            form: None,
        };
        engine.info = engine.survey()?;
        engine.open_form_environment();
        Ok(engine)
    }

    /// Start PDFium's form-fill environment, if this document has a form.
    ///
    /// A document without one gets nothing: the environment is not free, and a
    /// deck of slides has no fields to fill. A document *with* one gets it
    /// whether or not anyone intends to type into it, because rendering needs
    /// it — see [`PdfiumDocument::form`].
    ///
    /// Failure here is not failure to open. A form that could not be
    /// initialised is a document that reads and annotates normally and whose
    /// fields are not editable, which is a compatibility level pulpit already
    /// has a word for rather than a reason to refuse the file.
    fn open_form_environment(&mut self) {
        if !self.info.has_form {
            return;
        }
        let Ok(handle) = self.backend.document_handle(self.document) else {
            return;
        };
        let mut environment = crate::document::form::FormEnvironment::new();
        // Safety: the environment is boxed and stored beside the handle it is
        // attached to, and neither outlives the other — `close` exits the
        // environment before the box is dropped.
        let attached = unsafe { environment.attach(self.backend.bindings(), handle) };
        match attached {
            Some(handle) => {
                self.form = Some(FormBinding {
                    environment,
                    handle,
                    open_page: None,
                })
            }
            None => tracing::warn!("this document's form fields cannot be filled"),
        }
    }

    /// The form handle, for the calls that need one.
    fn form_handle(&self) -> Option<FPDF_FORMHANDLE> {
        self.form.as_ref().map(|form| form.handle)
    }

    /// The loaded page a form interaction is happening on, opening it if this
    /// is the first event, and moving it if the interaction changed page.
    ///
    /// Moving page ends the previous interaction properly:
    /// `FORM_OnBeforeClosePage` is what tells PDFium to commit and tear down
    /// the editing state on the page being left, and skipping it would leak
    /// both the state and the page handle.
    fn open_form_page(&mut self, page: PageIndex) -> Result<FPDF_PAGE> {
        let form = self
            .form_handle()
            .ok_or_else(|| DocumentError::Backend("this document has no fillable form".into()))?;
        if let Some((open, handle)) = self.form.as_ref().and_then(|form| form.open_page) {
            if open == page.get() {
                return Ok(handle);
            }
        }
        self.release_form_page();

        let bindings = self.backend.bindings();
        let document = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;
        let count = self.info.page_count;
        if page.get() >= count {
            return Err(DocumentError::NoSuchPage {
                page: page.get(),
                count,
            });
        }
        let handle = unsafe { bindings.FPDF_LoadPage(document, page.get() as i32) };
        if handle.is_null() {
            return Err(DocumentError::Backend(format!(
                "cannot load page {} for form input",
                page.get()
            )));
        }
        unsafe { bindings.FORM_OnAfterLoadPage(handle, form) };
        if let Some(binding) = self.form.as_mut() {
            binding.open_page = Some((page.get(), handle));
        }
        Ok(handle)
    }

    /// End the open form interaction, if there is one.
    fn release_form_page(&mut self) {
        let Some(form) = self.form_handle() else {
            return;
        };
        let Some((_, handle)) = self.form.as_mut().and_then(|form| form.open_page.take()) else {
            return;
        };
        let bindings = self.backend.bindings();
        unsafe {
            bindings.FORM_OnBeforeClosePage(handle, form);
            bindings.FPDF_ClosePage(handle);
        }
    }

    /// Draw the live form field contents over a rendered page.
    ///
    /// This pass is not optional and not an optimisation. `FPDF_RenderPageBitmap`
    /// draws a page's *content*, which for a form field is the appearance
    /// stream the file was saved with — so a value someone typed a second ago,
    /// which PDFium is holding in its form-fill environment and has not yet
    /// written into an appearance, is simply not in the picture. `FPDF_FFLDraw`
    /// is what puts it there, and §8.6 requires it over every render of a
    /// document that has a form.
    ///
    /// A document with no form environment returns immediately, which is every
    /// slide deck.
    fn composite_form_fields(
        &self,
        page: PageIndex,
        width: u32,
        height: u32,
        rgba: &mut [u8],
    ) -> Result<()> {
        let Some(form) = self.form_handle() else {
            return Ok(());
        };
        let expected = (width as usize) * (height as usize) * 4;
        if rgba.len() < expected || width == 0 || height == 0 {
            return Ok(());
        }
        let bindings = self.backend.bindings();
        // The page PDFium is *already* editing, when it is this one.
        //
        // This matters more than it looks. A field's uncommitted value belongs
        // to the page view PDFium built in `FORM_OnAfterLoadPage`, and a page
        // loaded again gets a different handle and a different, empty view —
        // so compositing through a fresh handle draws the value the file was
        // saved with, and the characters someone just typed are missing from
        // the picture even though the environment is holding them.
        let open = self
            .form
            .as_ref()
            .and_then(|form| form.open_page)
            .filter(|(open, _)| *open == page.get())
            .map(|(_, handle)| handle);

        let mut draw = |handle: FPDF_PAGE| -> crate::pdf::Result<()> {
            {
                // The same buffer the page was just drawn into, wrapped rather
                // than copied: `FPDF_FFLDraw` composites over what is already
                // there, which is exactly the page under the fields.
                let bitmap = unsafe {
                    bindings.FPDFBitmap_CreateEx(
                        width as i32,
                        height as i32,
                        FPDF_BITMAP_BGRA,
                        rgba.as_mut_ptr() as *mut std::os::raw::c_void,
                        width as i32 * 4,
                    )
                };
                if bitmap.is_null() {
                    // The page is drawn and the fields are not. Better than
                    // failing the render: a form whose values are missing is
                    // visibly wrong, and a blank page tells the reader less.
                    return Ok(());
                }
                unsafe {
                    bindings.FPDF_FFLDraw(
                        form,
                        bitmap,
                        handle,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        0,
                        // The same byte-order flag the page render used, so
                        // the two passes agree about what a pixel is. Without
                        // it the fields arrive with red and blue swapped.
                        FPDF_REVERSE_BYTE_ORDER,
                    );
                    bindings.FPDFBitmap_Destroy(bitmap);
                }
                Ok(())
            }
        };

        match open {
            // Already being edited: draw through that view, so what is on
            // screen is what the person typing has typed.
            Some(handle) => draw(handle).map_err(to_document_error),
            // Not being edited: any view will do, and PDFium needs telling
            // about this one before it will draw fields into it.
            None => self
                .backend
                .on_page(self.document, page.get(), |handle| {
                    unsafe { bindings.FORM_OnAfterLoadPage(handle, form) };
                    let outcome = draw(handle);
                    unsafe { bindings.FORM_OnBeforeClosePage(handle, form) };
                    outcome
                })
                .map_err(to_document_error),
        }
    }

    /// Which field PDFium just committed a value for.
    ///
    /// Read back from the focused widget rather than deduced from the event:
    /// PDFium knows which field it was editing, and reconstructing that from a
    /// click position and a keystroke history is exactly the second
    /// implementation §8.6 exists to avoid.
    ///
    /// `None` when nothing has focus any more — which is the ordinary case for
    /// a commit *caused* by focus loss, and is why the field is looked for on
    /// the page rather than only under the caret.
    fn committed_field(
        &self,
        page: PageIndex,
    ) -> Option<crate::document::protocol::CommittedField> {
        use crate::document::protocol::CommittedField;

        let form = self.form_handle()?;
        let bindings = self.backend.bindings();
        let geometry = self.measure(page).ok()?;

        // The focused widget first: during typing that is the field being
        // edited, and it is exact.
        let focused = self
            .backend
            .on_page(self.document, page.get(), |handle| {
                let mut index = 0;
                let annotation = std::ptr::null_mut();
                let mut annotation = annotation;
                let found =
                    unsafe { bindings.FORM_GetFocusedAnnot(form, &mut index, &mut annotation) }
                        != 0;
                if !found || annotation.is_null() {
                    return Ok(None);
                }
                let field =
                    read_form_field(bindings, form, annotation, PageIndex(page.get()), &geometry);
                unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                let _ = handle;
                Ok(field.map(|field| CommittedField {
                    name: field.name,
                    value: field.value,
                    revision: DocumentRevision::INITIAL,
                }))
            })
            .ok()
            .flatten();
        if focused.is_some() {
            return focused;
        }

        // Nothing focused: the commit was a focus loss, a toggle or a choice.
        // The document changed and pulpit knows it did, so the honest answer
        // is a change without a name rather than no change at all — the caller
        // still has to bump a revision and mark the document unsaved.
        Some(CommittedField {
            name: String::new(),
            value: String::new(),
            revision: DocumentRevision::INITIAL,
        })
    }

    /// The backend, for rendering the same open document.
    pub fn backend(&self) -> &PdfiumBackend {
        self.backend
    }

    pub fn backend_document(&self) -> BackendDocumentId {
        self.document
    }

    /// Close the open document.
    ///
    /// The binding outlives it: PDFium is bound once per process, so closing
    /// a file and opening another is a document lifetime, not a library one.
    /// Taking `&mut PdfiumBackend` is what proves nothing else is still
    /// reading this document when it goes.
    pub fn close(mut self, backend: &mut PdfiumBackend) {
        // The form environment first, and in this order: PDFium holds the
        // address of the environment's callback struct for as long as the
        // handle lives, so exiting the environment is what makes it safe for
        // the box to be dropped when `self` goes.
        self.release_form_page();
        if let Some(form) = self.form.take() {
            unsafe {
                backend
                    .bindings()
                    .FPDFDOC_ExitFormFillEnvironment(form.handle)
            };
            drop(form.environment);
        }
        backend.close(self.document);
    }

    /// What pulpit can tell the user about this document before they start
    /// (§3.4). Every warning here is evidence-based: a finding is never
    /// invented, and a document with none is reported as clean.
    fn survey(&mut self) -> Result<OpenDocumentInfo> {
        let bindings = self.backend.bindings();
        let handle = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;
        let page_count = unsafe { bindings.FPDF_GetPageCount(handle) }.max(0) as usize;

        let mut warnings = Vec::new();
        // A non-zero security-handler revision means the file is encrypted,
        // whether or not it needed a password to open.
        if unsafe { bindings.FPDF_GetSecurityHandlerRevision(handle) } >= 0 {
            warnings.push(DocumentWarning::Encrypted);
            // Bit 6 (value 32) of the permissions is "modify annotations".
            // PDFium reports all bits set for an unencrypted document, so this
            // is only consulted once encryption is established.
            let permissions = unsafe { bindings.FPDF_GetDocPermissions(handle) };
            if permissions & 0b10_0000 == 0 {
                warnings.push(DocumentWarning::MutationForbidden);
            }
        }

        // The same evidence the presenter's capability scan collects — read
        // once, interpreted here for a different question. A finding is never
        // invented: a document with no evidence of a feature is reported as
        // not having it.
        let evidence = PdfBackend::evidence(self.backend, self.document).unwrap_or_default();
        let mut has_form = false;
        let mut level = CompatibilityLevel::AnnotateOnly;
        match evidence.form_type {
            FormType::None => {}
            FormType::AcroForm => {
                has_form = true;
                level = CompatibilityLevel::Native;
            }
            FormType::Xfa => {
                has_form = true;
                warnings.push(DocumentWarning::XfaForm);
                level = CompatibilityLevel::NativeWithLimitations;
            }
        }
        let has_javascript = !evidence.document_javascript.is_empty()
            || evidence.pages.iter().any(|page| {
                page.annotations.iter().any(|annotation| {
                    annotation.has_additional_actions
                        || annotation.action == ActionKind::Unrecognised
                })
            });
        if has_javascript {
            warnings.push(DocumentWarning::JavaScript);
            if level == CompatibilityLevel::Native {
                level = CompatibilityLevel::NativeWithLimitations;
            }
        }

        // A9: an existing signature is detected and warned about *before* the
        // first mutation, not discovered after a save. PDFium exposes no
        // accessor for the signature dictionary without a form-fill
        // environment, so this is a bounded scan of the file's own bytes —
        // the same technique the capability scan already uses for `/Trans`.
        if self
            .source
            .as_deref()
            .map(carries_a_signature)
            .unwrap_or(false)
        {
            warnings.push(DocumentWarning::Signed);
        }

        let first_page = if page_count > 0 {
            self.measure(PageIndex(0))?
        } else {
            PageGeometry::default()
        };
        if page_count == 0 {
            level = CompatibilityLevel::Unsupported;
        }

        Ok(OpenDocumentInfo {
            page_count,
            level,
            warnings,
            first_page,
            has_form,
        })
    }

    /// Measure a page's canonical geometry from its crop box and `/Rotate`.
    fn measure(&self, page: PageIndex) -> Result<PageGeometry> {
        if let Some(geometry) = self.geometry.borrow().get(&page.get()) {
            return Ok(*geometry);
        }
        let bindings = self.backend.bindings();
        let geometry = self
            .backend
            .on_page(self.document, page.get(), |handle| {
                // The one reading of a crop box and a `/Rotate`, shared with
                // the render backend: two of them could disagree about where
                // a page's origin is, and every mark on it would move.
                Ok(crate::pdf::search::geometry_of(bindings, handle))
            })
            .map_err(to_document_error)?;
        if !geometry.is_valid() {
            return Err(DocumentError::Backend(format!(
                "page {page} has no usable geometry"
            )));
        }
        self.geometry.borrow_mut().insert(page.get(), geometry);
        Ok(geometry)
    }

    /// A page's geometry, measured if this is the first time it is asked for.
    fn geometry_of(&self, page: PageIndex) -> Result<PageGeometry> {
        self.measure(page)
    }

    /// Run `f` over every annotation on a page, in `/Annots` order.
    fn on_annotations<T>(
        &self,
        page: PageIndex,
        mut f: impl FnMut(usize, FPDF_ANNOTATION, &PageGeometry) -> Option<T>,
    ) -> Result<Vec<T>> {
        let geometry = self.geometry_of(page)?;
        let bindings = self.backend.bindings();
        self.backend
            .on_page(self.document, page.get(), |handle| {
                let count = unsafe { bindings.FPDFPage_GetAnnotCount(handle) }.max(0) as usize;
                let count = count.min(limits::MAX_ANNOTATIONS_PER_PAGE);
                let mut collected = Vec::new();
                for index in 0..count {
                    let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index as i32) };
                    if annotation.is_null() {
                        continue;
                    }
                    if let Some(value) = f(index, annotation, &geometry) {
                        collected.push(value);
                    }
                    unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                }
                Ok(collected)
            })
            .map_err(to_document_error)
    }

    /// Find an annotation by identity, returning the page and index it sits
    /// at. Linear, because `/Annots` is an array and there is no index by
    /// `/NM` in a PDF; bounded by the page limit above.
    fn locate(&self, id: &AnnotationId) -> Result<(PageIndex, usize)> {
        for page in 0..self.info.page_count {
            let page = PageIndex(page);
            self.measure(page)?;
            let found = self.on_annotations(page, |index, annotation, _| {
                let name = self.string_value(annotation, "NM")?;
                (AnnotationId::imported(&name).as_ref() == Some(id)).then_some(index)
            })?;
            if let Some(index) = found.first() {
                return Ok((page, *index));
            }
        }
        Err(DocumentError::NoSuchAnnotation(id.clone()))
    }

    /// Every page measured, so [`Self::locate`] can search them.
    ///
    /// Measuring is what fills the geometry cache, and an annotation on a page
    /// nobody has looked at yet would otherwise be invisible to a search.
    fn measure_all(&self) -> Result<()> {
        for page in 0..self.info.page_count {
            self.measure(PageIndex(page))?;
        }
        Ok(())
    }

    fn string_value(&self, annotation: FPDF_ANNOTATION, key: &str) -> Option<String> {
        let bindings = self.backend.bindings();
        let length =
            unsafe { bindings.FPDFAnnot_GetStringValue(annotation, key, std::ptr::null_mut(), 0) };
        // Two bytes is the terminator alone: the key is absent or empty.
        if length <= 2 {
            return None;
        }
        let length = (length as usize).min(limits::MAX_TEXT_BYTES * 2 + 2);
        let mut buffer = vec![0u8; length];
        unsafe {
            bindings.FPDFAnnot_GetStringValue(
                annotation,
                key,
                buffer.as_mut_ptr() as *mut FPDF_WCHAR,
                length as std::os::raw::c_ulong,
            )
        };
        Some(decode_utf16(&buffer))
    }

    fn rect_of(&self, annotation: FPDF_ANNOTATION, geometry: &PageGeometry) -> PageRect {
        let bindings = self.backend.bindings();
        let mut rect = FS_RECTF {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        if unsafe { bindings.FPDFAnnot_GetRect(annotation, &mut rect) } == 0 {
            return PageRect::default();
        }
        geometry.rect_from_user_space([
            rect.left.min(rect.right),
            rect.bottom.min(rect.top),
            rect.left.max(rect.right),
            rect.bottom.max(rect.top),
        ])
    }

    fn style_of(&self, annotation: FPDF_ANNOTATION) -> MarkStyle {
        let bindings = self.backend.bindings();
        let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 255u32);
        let read = unsafe {
            bindings.FPDFAnnot_GetColor(
                annotation,
                COLOR_TYPE_COLOR,
                &mut r,
                &mut g,
                &mut b,
                &mut a,
            )
        } != 0;
        let mut style = MarkStyle::default();
        if read {
            // Through `from_rgb` rather than straight into `Custom`, so a
            // mark made with one of the named swatches comes back named. A PDF
            // has no field for which swatch was used — `/C` is three numbers —
            // and reading it as always-custom would make every named colour
            // turn custom the first time it went through a file.
            style.color = InkColor::from_rgb(
                f32::from(r.min(255) as u8) / 255.0,
                f32::from(g.min(255) as u8) / 255.0,
                f32::from(b.min(255) as u8) / 255.0,
            );
            style.opacity = (a.min(255) as f32) / 255.0;
        }
        let (mut horizontal, mut vertical, mut width) = (0.0f32, 0.0f32, 0.0f32);
        if unsafe {
            bindings.FPDFAnnot_GetBorder(annotation, &mut horizontal, &mut vertical, &mut width)
        } != 0
            && width > 0.0
        {
            style.width = width;
        }
        style.sanitised()
    }

    fn ink_path(&self, annotation: FPDF_ANNOTATION, geometry: &PageGeometry) -> Vec<PagePoint> {
        let bindings = self.backend.bindings();
        let paths = unsafe { bindings.FPDFAnnot_GetInkListCount(annotation) } as usize;
        let mut points = Vec::new();
        for path in 0..paths.min(64) {
            let count = unsafe {
                bindings.FPDFAnnot_GetInkListPath(
                    annotation,
                    path as std::os::raw::c_ulong,
                    std::ptr::null_mut(),
                    0,
                )
            } as usize;
            if count == 0 || count > limits::MAX_POINTS_PER_INK {
                continue;
            }
            let mut buffer = vec![FS_POINTF { x: 0.0, y: 0.0 }; count];
            unsafe {
                bindings.FPDFAnnot_GetInkListPath(
                    annotation,
                    path as std::os::raw::c_ulong,
                    buffer.as_mut_ptr(),
                    count as std::os::raw::c_ulong,
                )
            };
            points.extend(
                buffer
                    .into_iter()
                    .map(|point| geometry.from_user_space(point.x, point.y)),
            );
            if points.len() >= limits::MAX_POINTS_PER_INK {
                points.truncate(limits::MAX_POINTS_PER_INK);
                break;
            }
        }
        points
    }

    fn quads_of(&self, annotation: FPDF_ANNOTATION, geometry: &PageGeometry) -> Vec<PageQuad> {
        let bindings = self.backend.bindings();
        if unsafe { bindings.FPDFAnnot_HasAttachmentPoints(annotation) } == 0 {
            return Vec::new();
        }
        let count = unsafe { bindings.FPDFAnnot_CountAttachmentPoints(annotation) };
        let count = count.min(limits::MAX_QUADS_PER_ANNOTATION);
        let mut quads = Vec::with_capacity(count);
        for index in 0..count {
            let mut quad = FS_QUADPOINTSF {
                x1: 0.0,
                y1: 0.0,
                x2: 0.0,
                y2: 0.0,
                x3: 0.0,
                y3: 0.0,
                x4: 0.0,
                y4: 0.0,
            };
            if unsafe { bindings.FPDFAnnot_GetAttachmentPoints(annotation, index, &mut quad) } == 0
            {
                continue;
            }
            quads.push(geometry.quad_from_user_space([
                quad.x1, quad.y1, quad.x2, quad.y2, quad.x3, quad.y3, quad.x4, quad.y4,
            ]));
        }
        quads
    }

    /// Turn one annotation into a summary, or `None` for a form widget, which
    /// is classified separately and never offered to the annotation editor
    /// (§8.6).
    fn summarise(
        &self,
        annotation: FPDF_ANNOTATION,
        page: PageIndex,
        geometry: &PageGeometry,
    ) -> Option<AnnotationSummary> {
        let bindings = self.backend.bindings();
        let subtype = unsafe { bindings.FPDFAnnot_GetSubtype(annotation) };
        if subtype == subtype::WIDGET {
            return None;
        }
        let kind = match subtype {
            subtype::INK => AnnotationKind::Ink,
            subtype::HIGHLIGHT => AnnotationKind::Highlight,
            subtype::FREETEXT => AnnotationKind::FreeText,
            subtype::TEXT => AnnotationKind::Note,
            subtype::STAMP => AnnotationKind::Stamp,
            _ => AnnotationKind::Other,
        };
        let name = self.string_value(annotation, "NM");
        let id = name
            .as_deref()
            .and_then(AnnotationId::imported)
            // A missing or unusable `/NM` gets a session identity, and the
            // annotation is written a real one the first time it is modified
            // or saved (A3). The session identity is derived from where it
            // sits, which is stable for as long as nothing renumbers.
            .unwrap_or_else(|| {
                AnnotationId::imported(&format!("session-{}-{:p}", page.get(), annotation))
                    .expect("a derived name is well formed")
            });

        let text = self
            .string_value(annotation, "Contents")
            .unwrap_or_default();
        let truncated = text.len() > limits::MAX_TEXT_BYTES;
        let mut text = text;
        if truncated {
            text.truncate(
                (0..=limits::MAX_TEXT_BYTES)
                    .rev()
                    .find(|at| text.is_char_boundary(*at))
                    .unwrap_or(0),
            );
        }

        let support = match kind {
            // A modelled subtype is edited *in place*, so every entry pulpit
            // does not know about survives the edit (see the module note).
            AnnotationKind::Ink
            | AnnotationKind::Highlight
            | AnnotationKind::FreeText
            | AnnotationKind::Note
            | AnnotationKind::Stamp => AnnotationSupport::Editable,
            AnnotationKind::Other => AnnotationSupport::Unsupported,
        };

        Some(AnnotationSummary {
            id,
            page,
            kind,
            bounds: self.rect_of(annotation, geometry),
            style: self.style_of(annotation),
            contents: AnnotationContents {
                text,
                truncated,
                pulpit_source: self.string_value(annotation, PULPIT_KEY),
            },
            support,
            revision: DocumentRevision::INITIAL,
            path: if kind == AnnotationKind::Ink {
                self.ink_path(annotation, geometry)
            } else {
                Vec::new()
            },
            quads: if kind == AnnotationKind::Highlight {
                self.quads_of(annotation, geometry)
            } else {
                Vec::new()
            },
            geometry_elided: false,
        })
    }

    /// Write a draft's content into a freshly created or existing annotation.
    fn write_draft(
        &self,
        page_handle: FPDF_PAGE,
        annotation: FPDF_ANNOTATION,
        draft: &AnnotationDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        let style = draft.style();
        let (red, green, blue) = style.color.rgb();
        let alpha = (style.opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
        // `/C` is not "the colour of the mark" for every kind. On an ink
        // stroke or a highlight it is what is drawn; on a free text it is the
        // *background of the box* (PDF 12.5.2), and on a stamp whose
        // appearance is a picture it is nothing at all. Setting it everywhere
        // put an opaque slab of ink behind every text mark — the text colour
        // there is `/DA`'s, and the background of a text mark is nothing.
        let colours_the_mark = !matches!(
            draft,
            AnnotationDraft::FreeText(_) | AnnotationDraft::Stamp(_)
        );
        if colours_the_mark {
            unsafe {
                bindings.FPDFAnnot_SetColor(
                    annotation,
                    COLOR_TYPE_COLOR,
                    (red * 255.0).round().clamp(0.0, 255.0) as u32,
                    (green * 255.0).round().clamp(0.0, 255.0) as u32,
                    (blue * 255.0).round().clamp(0.0, 255.0) as u32,
                    alpha,
                )
            };
        }

        if let Some(bounds) = draft.bounds() {
            let rect = geometry.rect_to_user_space(bounds);
            let rect = FS_RECTF {
                left: rect[0],
                bottom: rect[1],
                right: rect[2],
                top: rect[3],
            };
            unsafe { bindings.FPDFAnnot_SetRect(annotation, &rect) };
        }

        match draft {
            AnnotationDraft::Ink(ink) => self.write_ink(annotation, ink, geometry)?,
            AnnotationDraft::Highlight(highlight) => {
                self.write_highlight(annotation, highlight, geometry)?
            }
            AnnotationDraft::FreeText(free) => self.write_free_text(annotation, free, geometry)?,
            AnnotationDraft::Note(note) => {
                set_string(bindings, annotation, "Contents", &note.text)?;
            }
            AnnotationDraft::Stamp(stamp) => {
                self.write_stamp(page_handle, annotation, stamp, geometry)?
            }
        }
        // The border width is what a viewer regenerating an appearance uses,
        // so it is written even though pulpit supplies its own `/AP`.
        unsafe { bindings.FPDFAnnot_SetBorder(annotation, 0.0, 0.0, style.width) };
        Ok(())
    }

    fn write_ink(
        &self,
        annotation: FPDF_ANNOTATION,
        ink: &InkDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        // A replace rewrites the geometry rather than appending to it: an
        // `/InkList` that grew by one path per edit would draw every earlier
        // version of the stroke as well as the current one.
        unsafe { bindings.FPDFAnnot_RemoveInkList(annotation) };
        let points: Vec<FS_POINTF> = ink
            .points
            .iter()
            .map(|point| {
                let (x, y) = geometry.to_user_space(point.at);
                FS_POINTF { x, y }
            })
            .collect();
        if points.is_empty() {
            return Err(DocumentError::Backend("an empty stroke".into()));
        }
        let added =
            unsafe { bindings.FPDFAnnot_AddInkStroke(annotation, points.as_ptr(), points.len()) };
        if added < 0 {
            return Err(DocumentError::Backend(
                "PDFium refused the ink stroke".into(),
            ));
        }
        Ok(())
    }

    fn write_highlight(
        &self,
        annotation: FPDF_ANNOTATION,
        highlight: &HighlightDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        let quads: Vec<FS_QUADPOINTSF> = highlight
            .quads
            .iter()
            .map(|quad| {
                let v = geometry.quad_to_user_space(*quad);
                FS_QUADPOINTSF {
                    x1: v[0],
                    y1: v[1],
                    x2: v[2],
                    y2: v[3],
                    x3: v[4],
                    y3: v[5],
                    x4: v[6],
                    y4: v[7],
                }
            })
            .collect();
        if quads.is_empty() {
            return Err(DocumentError::Backend("a highlight with no quads".into()));
        }
        // PDFium can set an existing quad by index and append past the end,
        // but it cannot shorten the list. So the runs the annotation already
        // has are overwritten in place and the rest appended — which is exact
        // for a new annotation (none existing) and for a re-marked one whose
        // selection grew, and leaves a stale tail only when a selection
        // shrank. The tail is collapsed onto the last real run rather than
        // left describing text that is no longer selected.
        let existing = unsafe { bindings.FPDFAnnot_CountAttachmentPoints(annotation) };
        let last = *quads.last().expect("checked above");
        for (index, quad) in quads.iter().enumerate() {
            let ok = if index < existing {
                unsafe { bindings.FPDFAnnot_SetAttachmentPoints(annotation, index, quad) }
            } else {
                unsafe { bindings.FPDFAnnot_AppendAttachmentPoints(annotation, quad) }
            };
            if ok == 0 {
                return Err(DocumentError::Backend(
                    "PDFium refused the highlight geometry".into(),
                ));
            }
        }
        for index in quads.len()..existing {
            unsafe { bindings.FPDFAnnot_SetAttachmentPoints(annotation, index, &last) };
        }
        // §7.2: the selected text goes into `/Contents` as an accessibility
        // and search fallback, so the mark survives re-extraction.
        set_string(bindings, annotation, "Contents", &highlight.text)?;
        Ok(())
    }

    fn write_free_text(
        &self,
        annotation: FPDF_ANNOTATION,
        free: &FreeTextDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        set_string(bindings, annotation, "Contents", &free.text)?;
        // `/DA` names the font and colour a viewer uses when it regenerates
        // the appearance. Helvetica because it is one of the fourteen standard
        // faces every conforming viewer has without embedding.
        let (r, g, b) = free.style.color.rgb();
        let da = format!(
            "/Helv {:.2} Tf {r:.3} {g:.3} {b:.3} rg",
            free.style.font_size
        );
        set_string(bindings, annotation, "DA", &da)?;
        // `/Q` 0 is left-aligned, which is what the editor writes.
        if free.source == TextSource::Typst {
            // §7.4: the source is kept in pulpit's namespaced entry so the
            // annotation reopens for editing, while other viewers show the
            // appearance and are not asked to understand Typst.
            set_string(bindings, annotation, PULPIT_KEY, &free.text)?;
        }
        self.write_free_text_appearance(annotation, free, geometry)
    }

    /// Draw the text into the annotation, so the mark says what it says.
    ///
    /// PDFium generates appearances for the kinds it models — a highlight, an
    /// ink stroke, a note icon — and `/FreeText` is not one of them. Left to
    /// `/DA` alone the mark is whatever each viewer decides to regenerate, and
    /// in pulpit's own render it is nothing at all. The appearance is written
    /// here for the same reason a Typst mark carries a picture: what the
    /// reader sees when they place a mark is what every other viewer sees
    /// (§7.4).
    ///
    /// One text object per line, from the top of the box down. No background
    /// is drawn, and none is wanted: a comment written over a page must not
    /// hide the page.
    fn write_free_text_appearance(
        &self,
        annotation: FPDF_ANNOTATION,
        free: &FreeTextDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        let document = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;

        let size = free.style.font_size.max(1.0);
        // The leading the editor's own box was sized for, so the line spacing
        // on the page matches the line spacing that was typed into.
        let leading = size * 1.2;
        let (r, g, b) = free.style.color.rgb();
        let alpha = (free.style.opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
        let rect = geometry.rect_to_user_space(free.rect);
        let (left, top) = (rect[0], rect[3]);

        for (index, line) in free.text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            // Helvetica because it is one of the fourteen standard faces every
            // conforming viewer has without embedding, and the same face
            // `/DA` names: a viewer that regenerates the appearance from `/DA`
            // and one that draws this one agree about what the mark looks
            // like.
            let text = unsafe { bindings.FPDFPageObj_NewTextObj(document, "Helvetica", size) };
            if text.is_null() {
                return Err(DocumentError::Backend(
                    "PDFium refused to make a text object".into(),
                ));
            }
            let placed = (|| {
                if unsafe { bindings.FPDFText_SetText_str(text, line) } == 0 {
                    return Err(DocumentError::Backend(
                        "PDFium refused the mark's text".into(),
                    ));
                }
                unsafe {
                    bindings.FPDFPageObj_SetFillColor(
                        text,
                        (r * 255.0).round().clamp(0.0, 255.0) as u32,
                        (g * 255.0).round().clamp(0.0, 255.0) as u32,
                        (b * 255.0).round().clamp(0.0, 255.0) as u32,
                        alpha,
                    )
                };
                // A text object sits at the origin until it is moved, and the
                // baseline of the first line is one line down from the top of
                // the box. In PDF user space, which is where a page object
                // lives and where y grows upwards.
                let baseline = top - leading * (index as f32 + 1.0);
                if unsafe {
                    bindings.FPDFPageObj_Transform(
                        text,
                        1.0,
                        0.0,
                        0.0,
                        1.0,
                        f64::from(left),
                        f64::from(baseline),
                    );
                    bindings.FPDFAnnot_AppendObject(annotation, text)
                } == 0
                {
                    return Err(DocumentError::Backend(
                        "PDFium refused to put the text in the annotation".into(),
                    ));
                }
                Ok(())
            })();
            // The annotation owns the object once it has been appended; before
            // that it is this function's to destroy.
            if placed.is_err() {
                unsafe { bindings.FPDFPageObj_Destroy(text) };
                return placed;
            }
        }
        Ok(())
    }

    fn write_stamp(
        &self,
        page_handle: FPDF_PAGE,
        annotation: FPDF_ANNOTATION,
        stamp: &StampDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        // The `/Contents` of a stamp is its description, which is what a
        // screen reader announces; §7.6 says these are called marks and never
        // cryptographic signatures.
        //
        // A generated mark says what it says instead — §7.4 asks for a plain
        // fallback where a meaningful one exists, and for markup the source is
        // the closest thing to one.
        match &stamp.source {
            Some(source) => set_string(bindings, annotation, "Contents", source)?,
            None => set_string(bindings, annotation, "Contents", stamp.mark.label())?,
        }
        if let Some(source) = &stamp.source {
            // The markup itself, in pulpit's own namespaced entry: other
            // viewers show the appearance and are not asked to understand
            // Typst, and pulpit reopens the source for editing (§7.4).
            set_string(bindings, annotation, PULPIT_KEY, source)?;
        }

        if let StampMark::Image {
            pixel_width,
            pixel_height,
            rgba,
        } = &stamp.mark
        {
            self.write_stamp_image(
                page_handle,
                annotation,
                Picture {
                    rect: stamp.rect,
                    pixel_width: *pixel_width,
                    pixel_height: *pixel_height,
                    rgba,
                },
                geometry,
            )?;
        }
        Ok(())
    }

    /// Put a decoded picture into a `/Stamp` as an image XObject.
    ///
    /// The bytes are carried in and embedded; no path to the source file goes
    /// into the annotation (§7.6, §12). An annotation that pointed at a file
    /// on disk would either break when the file moved or read a file the
    /// *document* chose, and neither is something a signature should do.
    fn write_stamp_image(
        &self,
        page_handle: FPDF_PAGE,
        annotation: FPDF_ANNOTATION,
        picture: Picture<'_>,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let Picture {
            rect,
            pixel_width,
            pixel_height,
            rgba,
        } = picture;
        let bindings = self.backend.bindings();
        let document = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;

        // Bounded before anything is allocated (A8). The draft's own
        // validation has already checked this; checking again here is the
        // rule that a value crossing into a C library is checked at the
        // boundary it crosses.
        let pixels = u64::from(pixel_width) * u64::from(pixel_height);
        if pixel_width == 0
            || pixel_height == 0
            || pixels > StampMark::MAX_PIXELS
            || rgba.len() != pixel_width as usize * pixel_height as usize * 4
        {
            return Err(DocumentError::Backend("a malformed stamp picture".into()));
        }

        // PDFium reads BGRA for `FPDFBitmap_BGRA`, and the caller hands over
        // RGBA, so the channels are swapped into a buffer this function owns
        // for as long as PDFium is looking at it.
        let mut bgra = Vec::with_capacity(rgba.len());
        for chunk in rgba.chunks_exact(4) {
            bgra.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
        }

        /// `FPDFBitmap_BGRA`.
        const BGRA: std::os::raw::c_int = 4;
        let bitmap = unsafe {
            bindings.FPDFBitmap_CreateEx(
                pixel_width as i32,
                pixel_height as i32,
                BGRA,
                bgra.as_mut_ptr() as *mut std::ffi::c_void,
                pixel_width as i32 * 4,
            )
        };
        if bitmap.is_null() {
            return Err(DocumentError::Backend(
                "PDFium refused the stamp picture".into(),
            ));
        }

        let image = unsafe { bindings.FPDFPageObj_NewImageObj(document) };
        if image.is_null() {
            unsafe { bindings.FPDFBitmap_Destroy(bitmap) };
            return Err(DocumentError::Backend(
                "PDFium refused to make an image object".into(),
            ));
        }

        let outcome = (|| {
            // The page the object will live on: PDFium wants it while the
            // bitmap is attached, and passing nothing here is what made this
            // fall over rather than fail.
            let mut pages = [page_handle];
            if unsafe { bindings.FPDFImageObj_SetBitmap(pages.as_mut_ptr(), 1, image, bitmap) } == 0
            {
                return Err(DocumentError::Backend(
                    "PDFium refused the stamp's bitmap".into(),
                ));
            }
            // An image object is a unit square until it is told otherwise, so
            // the matrix is what places and sizes it. In PDF user space, which
            // is where a page object lives.
            let rect = geometry.rect_to_user_space(rect);
            let matrix = FS_MATRIX {
                a: rect[2] - rect[0],
                b: 0.0,
                c: 0.0,
                d: rect[3] - rect[1],
                e: rect[0],
                f: rect[1],
            };
            if unsafe { bindings.FPDFPageObj_SetMatrix(image, &matrix) } == 0 {
                return Err(DocumentError::Backend(
                    "PDFium refused the stamp's placement".into(),
                ));
            }
            if unsafe { bindings.FPDFAnnot_AppendObject(annotation, image) } == 0 {
                return Err(DocumentError::Backend(
                    "PDFium refused to put the picture in the annotation".into(),
                ));
            }
            Ok(())
        })();

        // The annotation owns the object once it has been appended; before
        // that it is this function's to destroy.
        if outcome.is_err() {
            unsafe { bindings.FPDFPageObj_Destroy(image) };
        }
        unsafe { bindings.FPDFBitmap_Destroy(bitmap) };
        outcome
    }
}

/// Does this file carry a cryptographic signature?
///
/// A byte scan, because the alternative is a form-fill environment that this
/// engine does not otherwise need. It errs towards saying yes: a false warning
/// costs the user one dismissal, and a missed one costs them the belief that a
/// signature survived their edits, which A9 exists to prevent.
fn carries_a_signature(source: &Path) -> bool {
    /// Signature dictionaries live in the trailer's neighbourhood, but an
    /// incrementally updated file can carry them anywhere; 32 MiB covers every
    /// document a person opens by hand and bounds the read.
    const MAX_SCAN_BYTES: u64 = 32 << 20;

    let Ok(metadata) = std::fs::metadata(source) else {
        return false;
    };
    if metadata.len() > MAX_SCAN_BYTES {
        return false;
    }
    let Ok(bytes) = std::fs::read(source) else {
        return false;
    };
    // `/Type /Sig` with any amount of whitespace between the two names, and
    // `/FT /Sig` for the field that carries it.
    contains_pdf_name_pair(&bytes, b"/Type", b"/Sig")
        || contains_pdf_name_pair(&bytes, b"/FT", b"/Sig")
}

fn contains_pdf_name_pair(bytes: &[u8], first: &[u8], second: &[u8]) -> bool {
    let mut at = 0usize;
    while let Some(found) = find(&bytes[at..], first) {
        let after = at + found + first.len();
        let tail = &bytes[after..(after + 8).min(bytes.len())];
        let trimmed = tail
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .unwrap_or(0);
        if tail[trimmed..].starts_with(second) {
            return true;
        }
        at = after;
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A decoded picture and where it goes, so the call that embeds one takes a
/// subject rather than a list of loose measurements.
struct Picture<'a> {
    rect: PageRect,
    pixel_width: u32,
    pixel_height: u32,
    rgba: &'a [u8],
}

/// Enough of the annotation to put it back, as far as PDFium can be asked.
fn capture_appearance(
    bindings: &dyn PdfiumLibraryBindings,
    annotation: FPDF_ANNOTATION,
) -> Vec<u8> {
    let length =
        unsafe { bindings.FPDFAnnot_GetAP(annotation, APPEARANCE_NORMAL, std::ptr::null_mut(), 0) };
    if length <= 2 || length as usize > limits::MAX_APPEARANCE_BYTES {
        return Vec::new();
    }
    let mut buffer = vec![0u8; length as usize];
    unsafe {
        bindings.FPDFAnnot_GetAP(
            annotation,
            APPEARANCE_NORMAL,
            buffer.as_mut_ptr() as *mut FPDF_WCHAR,
            length as std::os::raw::c_ulong,
        )
    };
    buffer
}

fn set_string(
    bindings: &dyn PdfiumLibraryBindings,
    annotation: FPDF_ANNOTATION,
    key: &str,
    value: &str,
) -> Result<()> {
    if value.len() > limits::MAX_TEXT_BYTES {
        return Err(DocumentError::Limit(limits::LimitExceeded {
            what: "annotation text",
            limit: limits::MAX_TEXT_BYTES,
        }));
    }
    if unsafe { bindings.FPDFAnnot_SetStringValue_str(annotation, key, value) } == 0 {
        return Err(DocumentError::Backend(format!(
            "PDFium refused to write /{key}"
        )));
    }
    Ok(())
}

/// PDFium hands text back as UTF-16LE with a terminator.
/// `FPDF_ANNOT_WIDGET` from `fpdf_annot.h`: the subtype every form field is
/// drawn with.
const FPDF_ANNOT_WIDGET: std::os::raw::c_int = 20;

/// Form field types from `fpdf_formfill.h`, in the order PDFium numbers them.
const FIELD_TYPE_PUSHBUTTON: std::os::raw::c_int = 1;
const FIELD_TYPE_CHECKBOX: std::os::raw::c_int = 2;
const FIELD_TYPE_RADIOBUTTON: std::os::raw::c_int = 3;
const FIELD_TYPE_COMBOBOX: std::os::raw::c_int = 4;
const FIELD_TYPE_LISTBOX: std::os::raw::c_int = 5;
const FIELD_TYPE_TEXTFIELD: std::os::raw::c_int = 6;
const FIELD_TYPE_SIGNATURE: std::os::raw::c_int = 7;

/// `FPDF_FORMFLAG_READONLY` from `fpdf_annot.h`.
const FORMFLAG_READONLY: std::os::raw::c_int = 1;
/// `FPDF_FORMFLAG_CHOICE_*` from `fpdf_annot.h`: an editable combo box, and a
/// list box that takes more than one selection.
const FPDF_FORMFLAG_CHOICE_EDIT: u32 = 262_144;
const FPDF_FORMFLAG_CHOICE_MULTI_SELECT: u32 = 2_097_152;

/// The ASCII control code a key is, for the keys PDFium's form-fill
/// environment handles as characters rather than as key events.
///
/// PDFium edits text in `FORM_OnChar`. `FORM_OnKeyDown` moves the caret and
/// changes the selection; it does not delete. A backspace sent as a key down
/// is accepted and does nothing at all, which is why this exists.
fn control_character(key: crate::document::protocol::FormKey) -> Option<std::os::raw::c_int> {
    use crate::document::protocol::FormKey;
    match key {
        FormKey::Backspace => Some(8),
        FormKey::Tab | FormKey::ShiftTab => Some(9),
        FormKey::Enter => Some(13),
        FormKey::Escape => Some(27),
        // The caret keys, and delete-forward, which PDFium does take as key
        // events.
        FormKey::Delete
        | FormKey::Left
        | FormKey::Right
        | FormKey::Up
        | FormKey::Down
        | FormKey::Home
        | FormKey::End => None,
    }
}

/// The virtual key codes PDFium's form-fill environment expects, from
/// `fpdf_fwlevent.h`. They are the Windows ones, which is what PDFium's
/// interface was modelled on.
fn key_code(key: crate::document::protocol::FormKey) -> std::os::raw::c_int {
    use crate::document::protocol::FormKey;
    match key {
        FormKey::Backspace => 8,
        FormKey::Tab => 9,
        // PDFium reads shift from the modifier argument rather than from a
        // key of its own, and the caller sends the modifier with the event.
        FormKey::ShiftTab => 9,
        FormKey::Enter => 13,
        FormKey::Escape => 27,
        FormKey::End => 35,
        FormKey::Home => 36,
        FormKey::Left => 37,
        FormKey::Up => 38,
        FormKey::Right => 39,
        FormKey::Down => 40,
        FormKey::Delete => 46,
    }
}

/// One widget annotation, read as the field it draws.
///
/// Returns the field's name, kind and value together with the one rectangle
/// this widget occupies; the caller groups widgets that name the same field.
#[allow(clippy::type_complexity)]
fn read_form_field(
    bindings: &dyn PdfiumLibraryBindings,
    form: FPDF_FORMHANDLE,
    annotation: FPDF_ANNOTATION,
    page: PageIndex,
    geometry: &PageGeometry,
) -> Option<FormField> {
    let name = form_string(bindings, |buffer, length| unsafe {
        bindings.FPDFAnnot_GetFormFieldName(form, annotation, buffer, length)
    })?;
    // A field with no name cannot be navigated to, listed or reported on, and
    // is not something a listing can say anything useful about.
    if name.is_empty() {
        return None;
    }
    let kind = match unsafe { bindings.FPDFAnnot_GetFormFieldType(form, annotation) } {
        FIELD_TYPE_PUSHBUTTON => FieldKind::PushButton,
        FIELD_TYPE_CHECKBOX => FieldKind::Checkbox,
        FIELD_TYPE_RADIOBUTTON => FieldKind::RadioGroup,
        FIELD_TYPE_COMBOBOX => FieldKind::ComboBox,
        FIELD_TYPE_LISTBOX => FieldKind::ListBox,
        FIELD_TYPE_TEXTFIELD => FieldKind::Text,
        FIELD_TYPE_SIGNATURE => FieldKind::Signature,
        // Including PDFium's -1 for "not a form field", which a widget
        // annotation with a broken parent reports.
        _ => FieldKind::Unknown,
    };
    let value = form_string(bindings, |buffer, length| unsafe {
        bindings.FPDFAnnot_GetFormFieldValue(form, annotation, buffer, length)
    })
    .unwrap_or_default();
    // A multiline field's lines are separated by CRLF in the file, which is
    // what PDF says and what PDFium reports. Everything above this module
    // works in LF, and a value that came back with carriage returns in it
    // would compare unequal to the same text typed into it — so the newline is
    // normalised here, at the boundary, rather than in every caller.
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    let flags = unsafe { bindings.FPDFAnnot_GetFormFieldFlags(form, annotation) };
    let read_only = flags >= 0 && flags & FORMFLAG_READONLY != 0;

    // What a choice field offers, in the order the file lists it — which is
    // the order PDFium indexes it by, and therefore the order a selection
    // event names. A field of any other kind has none.
    let mut options = Vec::new();
    if matches!(kind, FieldKind::ComboBox | FieldKind::ListBox) {
        let count = unsafe { bindings.FPDFAnnot_GetOptionCount(form, annotation) };
        for index in 0..count.clamp(0, limits::MAX_FIELD_OPTIONS as i32) {
            let label = form_string(bindings, |buffer, length| unsafe {
                bindings.FPDFAnnot_GetOptionLabel(form, annotation, index, buffer, length)
            })
            .unwrap_or_default();
            options.push(label);
        }
    }
    let allows_custom_value =
        flags >= 0 && flags & (FPDF_FORMFLAG_CHOICE_EDIT as std::os::raw::c_int) != 0;
    let multiple_selection =
        flags >= 0 && flags & (FPDF_FORMFLAG_CHOICE_MULTI_SELECT as std::os::raw::c_int) != 0;

    // Which options are chosen, asked of PDFium rather than parsed out of the
    // field's value. A multi-select list box is the case that needs it: its
    // `/V` names one entry at most, and PDFium does not rewrite it as the
    // selection changes, so reading the value alone reports the first choice
    // for ever and every later one looks like it did not take.
    let selected: Vec<u32> = (0..options.len())
        .filter(|index| unsafe {
            bindings.FPDFAnnot_IsOptionSelected(form, annotation, *index as std::os::raw::c_int)
                != 0
        })
        .map(|index| index as u32)
        .collect();

    // A choice field reports what a person chose, not the code it is stored
    // under.
    //
    // A PDF choice can pair each option with an export value — `France` shown,
    // `FR` written — and `FPDFAnnot_GetFormFieldValue` gives the export value,
    // because that is what the field holds. But this list is what "is this
    // filled in" is judged from,
    // and `Country: FR` is not what the person filling the form chose; it is
    // the code their choice is filed under. So the selected option's label is
    // reported when there is one, and the raw value only when there is not —
    // which covers a value typed into an editable combo box, and a field whose
    // stored value matches no option at all.
    let value =
        if matches!(kind, FieldKind::ComboBox | FieldKind::ListBox) && !options.contains(&value) {
            let selected = (0..options.len()).find(|index| unsafe {
                bindings.FPDFAnnot_IsOptionSelected(form, annotation, *index as std::os::raw::c_int)
                    != 0
            });
            // The *first* selected option, for a list that takes several. Which
            // one PDFium calls "the" value of a multiple selection is its own
            // business; what this promises is that the value names something the
            // person actually chose.
            selected
                .map(|index| options[index].clone())
                .unwrap_or(value)
        } else {
            value
        };

    let mut rect = pdfium_render::prelude::FS_RECTF {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };
    let bounds = if unsafe { bindings.FPDFAnnot_GetRect(annotation, &mut rect) } != 0 {
        let corner = geometry.from_user_space(rect.left, rect.top);
        let opposite = geometry.from_user_space(rect.right, rect.bottom);
        PageRect::new(
            corner.x.min(opposite.x),
            corner.y.min(opposite.y),
            corner.x.max(opposite.x),
            corner.y.max(opposite.y),
        )
    } else {
        // A widget whose rectangle cannot be read is still a field worth
        // listing; what is lost is the ability to jump to it.
        PageRect::new(0.0, 0.0, 0.0, 0.0)
    };

    Some(FormField {
        name,
        kind,
        value,
        read_only,
        options,
        allows_custom_value,
        multiple_selection,
        selected,
        // One widget: this call reads one annotation. The caller collects the
        // widgets that name the same field, because a field can be drawn in
        // several places and each of them is a separate annotation.
        widgets: vec![FieldWidget {
            page,
            bounds,
            // Which of the field's values *this rectangle* stands for.
            //
            // Only a radio group and a checkbox have one: their widgets are
            // the options, and pressing one is how that option is chosen. For
            // everything else the widget is the field and this is `None` —
            // which is also what PDFium answers, so the call is made for every
            // kind rather than guarded by one.
            option: matches!(kind, FieldKind::RadioGroup | FieldKind::Checkbox)
                .then(|| {
                    form_string(bindings, |buffer, length| unsafe {
                        bindings.FPDFAnnot_GetFormFieldExportValue(form, annotation, buffer, length)
                    })
                })
                .flatten()
                .filter(|option| !option.is_empty()),
        }],
    })
}

/// PDFium's two-call string dance, for the form getters.
///
/// Every one of them returns the length in bytes including a two-byte
/// terminator, then fills a buffer of that size. A length of two or less is
/// the terminator alone, which is an absent or empty value rather than an
/// error.
fn form_string(
    _bindings: &dyn PdfiumLibraryBindings,
    mut call: impl FnMut(*mut FPDF_WCHAR, std::os::raw::c_ulong) -> std::os::raw::c_ulong,
) -> Option<String> {
    let length = call(std::ptr::null_mut(), 0);
    if length <= 2 {
        return Some(String::new());
    }
    let length = (length as usize).min(limits::MAX_FIELD_VALUE_BYTES * 2 + 2);
    let mut buffer = vec![0u8; length];
    call(
        buffer.as_mut_ptr() as *mut FPDF_WCHAR,
        length as std::os::raw::c_ulong,
    );
    Some(decode_utf16(&buffer))
}

fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn to_document_error(error: PdfError) -> DocumentError {
    match error {
        PdfError::PageOutOfRange { page, count } => DocumentError::NoSuchPage { page, count },
        other => DocumentError::Backend(other.to_string()),
    }
}

impl DocumentBackend for PdfiumDocument<'_> {
    fn info(&self) -> &OpenDocumentInfo {
        &self.info
    }

    fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry> {
        if page.get() >= self.info.page_count {
            return Err(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.info.page_count,
            });
        }
        self.measure(page)
    }

    fn annotations(&self, page: PageIndex) -> Result<Vec<AnnotationSummary>> {
        self.on_annotations(page, |_, annotation, geometry| {
            self.summarise(annotation, page, geometry)
        })
    }

    fn annotation(&self, id: &AnnotationId) -> Result<AnnotationSummary> {
        let (page, index) = self.locate(id)?;
        let summaries = self.annotations(page)?;
        summaries
            .into_iter()
            .nth(index)
            .filter(|summary| summary.id == *id)
            // The index is an `/Annots` index and the summary list skips form
            // widgets, so the two can disagree; the name is the identity.
            .or_else(|| {
                self.annotations(page)
                    .ok()?
                    .into_iter()
                    .find(|summary| summary.id == *id)
            })
            .ok_or_else(|| DocumentError::NoSuchAnnotation(id.clone()))
    }

    fn create(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary> {
        let page = draft.page();
        let geometry = self.measure(page)?;
        let subtype = match draft.kind() {
            AnnotationKind::Ink => subtype::INK,
            AnnotationKind::Highlight => subtype::HIGHLIGHT,
            AnnotationKind::FreeText => subtype::FREETEXT,
            AnnotationKind::Note => subtype::TEXT,
            AnnotationKind::Stamp => subtype::STAMP,
            AnnotationKind::Other => {
                return Err(DocumentError::Backend(
                    "an annotation of no known kind".into(),
                ))
            }
        };
        let bindings = self.backend.bindings();
        let document = self.document;
        // All of it inside one loaded page: creating the annotation, writing
        // it and generating the page's content. A stamp's picture is a page
        // object and PDFium wants the page it belongs to while it is being
        // attached, so the handle has to be in scope for the whole of it.
        self.backend
            .on_page(document, page.get(), |handle| {
                let annotation = unsafe { bindings.FPDFPage_CreateAnnot(handle, subtype) };
                if annotation.is_null() {
                    return Err(PdfError::Render(
                        "PDFium refused to create the annotation".into(),
                    ));
                }
                let outcome = (|| {
                    set_string(bindings, annotation, "NM", id.as_str())?;
                    self.write_draft(handle, annotation, draft, &geometry)
                })();
                unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                outcome.map_err(|error| PdfError::Render(error.to_string()))?;

                // Content generation is what commits the new annotation's
                // appearance into the page; without it the mark is in
                // `/Annots` and invisible.
                unsafe { bindings.FPDFPage_GenerateContent(handle) };
                Ok(())
            })
            .map_err(to_document_error)?;

        self.annotation(id)
    }

    fn replace(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary> {
        self.measure_all()?;
        let (page, index) = self.locate(id)?;
        if draft.page() != page {
            return Err(DocumentError::Backend(
                "an annotation cannot be replaced onto another page".into(),
            ));
        }
        let geometry = self.measure(page)?;
        let bindings = self.backend.bindings();
        let document = self.document;
        self.backend
            .on_page(document, page.get(), |handle| {
                let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index as i32) };
                if annotation.is_null() {
                    return Err(PdfError::Render("the annotation went away".into()));
                }
                let outcome = self.write_draft(handle, annotation, draft, &geometry);
                unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                unsafe { bindings.FPDFPage_GenerateContent(handle) };
                outcome.map_err(|error| PdfError::Render(error.to_string()))
            })
            .map_err(to_document_error)?;
        self.annotation(id)
    }

    fn delete(&mut self, id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        self.measure_all()?;
        let before = self.before_image(id)?;
        let (page, index) = self.locate(id)?;
        let bindings = self.backend.bindings();
        let document = self.document;
        self.backend
            .on_page(document, page.get(), |handle| {
                if unsafe { bindings.FPDFPage_RemoveAnnot(handle, index as i32) } == 0 {
                    return Err(PdfError::Render("PDFium refused the deletion".into()));
                }
                unsafe { bindings.FPDFPage_GenerateContent(handle) };
                Ok(())
            })
            .map_err(to_document_error)?;
        Ok(before)
    }

    fn restore(
        &mut self,
        id: &AnnotationId,
        before: &AnnotationBeforeImage,
    ) -> Result<AnnotationSummary> {
        let draft = before
            .draft
            .clone()
            .ok_or_else(|| DocumentError::NotEditable(id.clone()))?;
        self.measure_all()?;
        // Restoring over an annotation that is still there is the undo of a
        // replace; restoring one that is gone is the undo of a delete.
        if self.locate(id).is_ok() {
            return self.replace(id, &draft);
        }
        self.create(id, &draft)
    }

    fn before_image(&self, id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        let (page, _) = self.locate(id)?;
        let summary = self.annotation(id)?;
        let geometry = self.geometry_of(page)?;
        let _ = geometry;
        // The same conversion the editor uses to build a replacement, so an
        // undo puts back exactly what a replace would have written.
        let draft = summary.to_draft();
        // The appearance stream is the entry that decides how the mark looks
        // elsewhere, so it is the one worth carrying even though the general
        // dictionary cannot be walked (see the module note).
        let preserved = self
            .on_annotations(page, |_, annotation, _| {
                let name = self.string_value(annotation, "NM")?;
                (AnnotationId::imported(&name).as_ref() == Some(id))
                    .then(|| capture_appearance(self.backend.bindings(), annotation))
            })?
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(AnnotationBeforeImage {
            page,
            draft,
            preserved,
        })
    }

    fn outline(&self) -> Result<pulpit_core::navigation::Outline> {
        PdfBackend::outline(self.backend, self.document).map_err(to_document_error)
    }

    /// Every field in the document, gathered from the widget annotations that
    /// draw them.
    ///
    /// A *field* is a thing with a name and a value; a *widget* is a rectangle
    /// on a page that shows it. They are not one to one — a radio group has one
    /// field and several widgets, and a "sign here" field can be mirrored on
    /// every page — so widgets are collected under the field name they belong
    /// to rather than listed as if each were its own field (§8.6).
    ///
    /// This is a listing. Nothing here edits anything: it says what a document
    /// asks for and how much of it is filled. Values are typed on the page, by
    /// PDFium, and this list is read back afterwards.
    fn fields(&self) -> Result<Vec<FormField>> {
        let Some(form) = self.form_handle() else {
            // No environment, so no form, or a form that failed to initialise.
            // Either way an empty list is the truth rather than a silence.
            return Ok(Vec::new());
        };
        let bindings = self.backend.bindings();
        let mut fields: Vec<FormField> = Vec::new();

        for page in 0..self.info.page_count {
            if fields.len() >= limits::MAX_FORM_FIELDS {
                break;
            }
            let geometry = self.measure(PageIndex(page))?;
            let collected = self
                .backend
                .on_page(self.document, page, |handle| {
                    let count = unsafe { bindings.FPDFPage_GetAnnotCount(handle) }.max(0);
                    let mut found = Vec::new();
                    for index in 0..count.min(limits::MAX_ANNOTATIONS_PER_PAGE as i32) {
                        let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index) };
                        if annotation.is_null() {
                            continue;
                        }
                        let widget = unsafe { bindings.FPDFAnnot_GetSubtype(annotation) }
                            == FPDF_ANNOT_WIDGET;
                        if widget {
                            if let Some(field) = read_form_field(
                                bindings,
                                form,
                                annotation,
                                PageIndex(page),
                                &geometry,
                            ) {
                                found.push(field);
                            }
                        }
                        unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                    }
                    Ok(found)
                })
                .map_err(to_document_error)?;

            for mut found in collected {
                match fields.iter_mut().find(|field| field.name == found.name) {
                    // Another rectangle for a field already seen: a radio
                    // group's other option, or the same field repeated on
                    // another page. The widgets join the field that is already
                    // there; everything else about it is the same field's and
                    // was read the first time.
                    Some(field) => {
                        for widget in found.widgets.drain(..) {
                            if field.widgets.len() < limits::MAX_FIELD_WIDGETS {
                                field.widgets.push(widget);
                            }
                        }
                        // …except which option is chosen, which a radio group
                        // states on the *selected* kid and not on the group.
                        // Whichever widget knows is the one that is believed.
                        if field.selected.is_empty() {
                            field.selected = std::mem::take(&mut found.selected);
                        }
                    }
                    None => fields.push(found),
                }
            }
        }
        Ok(fields)
    }

    /// Setting a value from outside the page is not how a field is filled.
    ///
    /// §8.6 is explicit that there is exactly one editing surface: values are
    /// typed on the page, by PDFium, under the field's own `/DA`. A second way
    /// in — an inspector that writes a value directly — is the thing that
    /// design exists to avoid, because it is where an application's idea of a
    /// field's value and PDFium's start to disagree.
    fn set_field(&mut self, name: &str, _value: &str) -> Result<String> {
        Err(DocumentError::Backend(format!(
            "the field {name} is filled on the page, not set from outside it"
        )))
    }

    fn field_value(&self, name: &str) -> Result<String> {
        self.fields()?
            .into_iter()
            .find(|field| field.name == name)
            .map(|field| field.value)
            .ok_or_else(|| DocumentError::NoSuchField(name.to_string()))
    }

    /// Forward one raw input event to PDFium's form-fill environment.
    ///
    /// This is the whole of form editing (§8.6). PDFium does the hit-testing,
    /// the focus, the caret, the text editing under the field's `/DA`, the
    /// comb spacing, the toggling and the choice list; what comes back is the
    /// rectangles it wants redrawn and whether a value was committed.
    ///
    /// The event arrives in canonical page space (A4) and is converted to
    /// PDFium's user space here, which is the only place that conversion is
    /// correct — the caller has no page geometry and should not need one.
    fn form_event(
        &mut self,
        page: PageIndex,
        event: crate::document::protocol::FormInputEvent,
    ) -> Result<crate::document::protocol::FormEventResult> {
        use crate::document::protocol::{FormEventResult, FormInputEvent};

        let Some(form) = self.form_handle() else {
            return Err(DocumentError::Backend(
                "this document has no fillable form".into(),
            ));
        };
        let geometry = self.measure(page)?;
        let bindings = self.backend.bindings();

        // The page the interaction is on, loaded once and kept — see
        // `FormBinding::open_page` for why this cannot be per-event.
        let handle = self.open_form_page(page)?;
        let at = |point: PagePoint| geometry.to_user_space(point);
        let dropping_focus = matches!(&event, FormInputEvent::Focus { gained: false });
        unsafe {
            match event {
                FormInputEvent::PointerDown { at: point } => {
                    let (x, y) = at(point);
                    bindings.FORM_OnLButtonDown(form, handle, 0, f64::from(x), f64::from(y));
                }
                FormInputEvent::PointerUp { at: point } => {
                    let (x, y) = at(point);
                    bindings.FORM_OnLButtonUp(form, handle, 0, f64::from(x), f64::from(y));
                }
                FormInputEvent::PointerMove { at: point } => {
                    let (x, y) = at(point);
                    bindings.FORM_OnMouseMove(form, handle, 0, f64::from(x), f64::from(y));
                }
                FormInputEvent::Char { character } => {
                    // One UTF-16 code unit per call. PDFium's `FORM_OnChar`
                    // takes a `nChar` that is a code *unit*, not a code point,
                    // so a character outside the basic multilingual plane —
                    // an emoji, most of the historic scripts — has to arrive
                    // as its surrogate pair. Passing the code point straight
                    // through truncates it: U+1F642 lands in the field as
                    // U+F642, a private-use character that renders as nothing.
                    let mut units = [0u16; 2];
                    for unit in character.encode_utf16(&mut units) {
                        bindings.FORM_OnChar(form, handle, i32::from(*unit), 0);
                    }
                }
                // Backspace, enter, escape and tab are *characters* to
                // PDFium's form-fill environment, not key events: it edits
                // text in `FORM_OnChar` and uses `FORM_OnKeyDown` for the
                // keys that move the caret without changing anything.
                // Sending backspace as a key down is accepted and does
                // nothing, which is the worst of both — the field simply
                // fails to delete, with no error anywhere.
                FormInputEvent::KeyDown { key } => match control_character(key) {
                    Some(character) => {
                        bindings.FORM_OnChar(form, handle, character, 0);
                    }
                    None => {
                        bindings.FORM_OnKeyDown(form, handle, key_code(key), 0);
                    }
                },
                FormInputEvent::KeyUp { key } => {
                    bindings.FORM_OnKeyUp(form, handle, key_code(key), 0);
                }
                FormInputEvent::Focus { gained: false } => {
                    // Losing focus is what *commits* an in-progress edit,
                    // which is why it is an event rather than a hint.
                    bindings.FORM_ForceToKillFocus(form);
                }
                FormInputEvent::Focus { gained: true } => {}
                FormInputEvent::SelectOption { index, selected } => {
                    bindings.FORM_SetIndexSelected(form, handle, index as i32, i32::from(selected));
                }
                FormInputEvent::FocusField { ref name } => {
                    // Find the widget by name and hand it to PDFium to focus.
                    // A click cannot do this when widgets overlap, which is
                    // exactly when the navigation list is the only way in.
                    let count = bindings.FPDFPage_GetAnnotCount(handle).max(0);
                    for index in 0..count.min(limits::MAX_ANNOTATIONS_PER_PAGE as i32) {
                        let annotation = bindings.FPDFPage_GetAnnot(handle, index);
                        if annotation.is_null() {
                            continue;
                        }
                        let matches = bindings.FPDFAnnot_GetSubtype(annotation)
                            == FPDF_ANNOT_WIDGET
                            && form_string(bindings, |buffer, length| {
                                bindings
                                    .FPDFAnnot_GetFormFieldName(form, annotation, buffer, length)
                            })
                            .as_deref()
                                == Some(name.as_str());
                        if matches {
                            bindings.FORM_SetFocusedAnnot(form, annotation);
                        }
                        bindings.FPDFPage_CloseAnnot(annotation);
                        if matches {
                            break;
                        }
                    }
                }
            }
        }

        // Everything PDFium asked for while the event was being handled, in
        // canonical page space so the caller can composite it without knowing
        // anything about PDF coordinates.
        let form = self
            .form
            .as_mut()
            .expect("the handle above came from this binding");
        let invalidated = form
            .environment
            .take_dirty()
            .into_iter()
            .map(|rect| {
                let top_left = geometry.from_user_space(rect.left as f32, rect.top as f32);
                let bottom_right = geometry.from_user_space(rect.right as f32, rect.bottom as f32);
                PageRect::new(
                    top_left.x.min(bottom_right.x),
                    top_left.y.min(bottom_right.y),
                    top_left.x.max(bottom_right.x),
                    top_left.y.max(bottom_right.y),
                )
            })
            .collect();
        let changed = form.environment.take_changed();

        // Which field changed is read back from the document rather than
        // guessed from the event: PDFium knows what it committed and the
        // caller of this does not. Read *before* the page is released, while
        // the focus that names the field is still there.
        let committed = if changed {
            self.committed_field(page)
        } else {
            None
        };
        if dropping_focus {
            // The interaction is over: the page goes, and with it the
            // uncommitted in-field state, which has just been committed.
            self.release_form_page();
        }

        Ok(FormEventResult {
            invalidated,
            committed,
        })
    }

    fn select_text(
        &self,
        page: PageIndex,
        selection: TextSelection,
    ) -> Result<TextSelectionResult> {
        let geometry = self.geometry_of(page)?;
        let bindings = self.backend.bindings();
        self.backend
            .on_page(self.document, page.get(), |handle| {
                let text_page = unsafe { bindings.FPDFText_LoadPage(handle) };
                if text_page.is_null() {
                    // No text layer is an empty result, not a failure (§6.3).
                    return Ok(TextSelectionResult::default());
                }
                let result = resolve_selection(bindings, text_page, &geometry, selection);
                unsafe { bindings.FPDFText_ClosePage(text_page) };
                Ok(result)
            })
            .map_err(to_document_error)
    }

    fn find_text(
        &self,
        query: &pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) -> Result<pulpit_core::search::HitChunk> {
        let mut chunk = pulpit_core::search::HitChunk {
            from_page: pages.start,
            to_page: pages.end,
            ..Default::default()
        };
        if query.is_empty() {
            return Ok(chunk);
        }
        let bindings = self.backend.bindings();
        for page in pages {
            let geometry = self.geometry_of(PageIndex(page))?;
            let hits = self
                .backend
                .on_page(self.document, page, |handle| {
                    let text_page = unsafe { bindings.FPDFText_LoadPage(handle) };
                    if text_page.is_null() {
                        // A page with no text layer — a scan, a poster — is
                        // not a failure. It simply has nothing to find.
                        return Ok(Vec::new());
                    }
                    let hits = crate::pdf::search::find_on_page(
                        bindings,
                        text_page,
                        &geometry,
                        PageIndex(page),
                        query,
                        limits::MAX_HITS_PER_SEARCH,
                        limits::MAX_QUADS_PER_HIT,
                    );
                    unsafe { bindings.FPDFText_ClosePage(text_page) };
                    Ok(hits)
                })
                .map_err(to_document_error)?;
            chunk.hits.extend(hits);
            if chunk.hits.len() >= limits::MAX_HITS_PER_SEARCH {
                chunk.hits.truncate(limits::MAX_HITS_PER_SEARCH);
                chunk.truncated = true;
                break;
            }
        }
        Ok(chunk)
    }

    fn write_to(&mut self, destination: &Path, options: SaveOptions) -> Result<u64> {
        let handle = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;
        let bytes = self
            .backend
            .save_to_memory(handle, options.incremental)
            .map_err(|error| DocumentError::Save(error.to_string()))?;
        crate::pdf::pdfium::write_atomically(destination, &bytes)
            .map_err(|error| DocumentError::Save(error.to_string()))?;
        Ok(bytes.len() as u64)
    }

    fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Rasterise through the same backend that renders the presenter's
    /// slides, from the document this engine holds.
    ///
    /// The same handle, deliberately: a frame drawn after a commit contains
    /// the commit (A7), which a separate read-only copy of the file could not
    /// promise. Annotations are drawn, because a page rendered without them
    /// would be a page with the user's marks missing.
    fn render_page(&self, page: PageIndex, width: u32, height: u32, rgba: &mut [u8]) -> Result<()> {
        let request = crate::pdf::RenderRequest {
            document: self.document,
            page: page.get(),
            region: pulpit_core::notes::Region::FULL,
            width,
            height,
            with_annotations: true,
        };
        request.validate().map_err(to_document_error)?;
        PdfBackend::render_into(self.backend, &request, rgba, &crate::pdf::NeverCancel)
            .map_err(to_document_error)?;
        self.composite_form_fields(page, width, height, rgba)
    }
}

/// Turn a canonical selection into the runs of text it covers.
fn resolve_selection(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    geometry: &PageGeometry,
    selection: TextSelection,
) -> TextSelectionResult {
    let count = unsafe { bindings.FPDFText_CountChars(text_page) };
    if count <= 0 {
        return TextSelectionResult::default();
    }

    let index_at = |point: PagePoint| -> Option<i32> {
        let (x, y) = geometry.to_user_space(point);
        // A generous tolerance: a click between two lines should land on one
        // of them rather than on nothing.
        let index = unsafe {
            bindings.FPDFText_GetCharIndexAtPos(text_page, x as f64, y as f64, 12.0, 12.0)
        };
        (index >= 0).then_some(index)
    };

    let (start, end) = match selection {
        TextSelection::Range { anchor, head } => {
            let (Some(a), Some(b)) = (index_at(anchor), index_at(head)) else {
                return TextSelectionResult::default();
            };
            (a.min(b), a.max(b))
        }
        TextSelection::Word { at } => {
            let Some(index) = index_at(at) else {
                return TextSelectionResult::default();
            };
            expand(bindings, text_page, index, count, |c| {
                c.is_alphanumeric() || c == '\'' || c == '-'
            })
        }
        TextSelection::Line { at } => {
            let Some(index) = index_at(at) else {
                return TextSelectionResult::default();
            };
            expand(bindings, text_page, index, count, |c| {
                c != '\n' && c != '\r'
            })
        }
    };
    let length = (end - start + 1).max(0);
    if length == 0 {
        return TextSelectionResult::default();
    }

    let quads = crate::pdf::search::quads_of(
        bindings,
        text_page,
        geometry,
        start,
        length,
        limits::MAX_QUADS_PER_SELECTION,
    );
    let text =
        crate::pdf::search::text_of(bindings, text_page, start, length, limits::MAX_TEXT_BYTES);

    TextSelectionResult {
        quads,
        text,
        truncated: length as usize > limits::MAX_TEXT_BYTES,
    }
}

/// Grow a selection outwards from `index` while `keep` holds.
fn expand(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    index: i32,
    count: i32,
    keep: impl Fn(char) -> bool,
) -> (i32, i32) {
    let character = |at: i32| -> Option<char> {
        let unicode = unsafe { bindings.FPDFText_GetUnicode(text_page, at) };
        char::from_u32(unicode)
    };
    let mut start = index;
    while start > 0 {
        match character(start - 1) {
            Some(c) if keep(c) => start -= 1,
            _ => break,
        }
    }
    let mut end = index;
    while end + 1 < count {
        match character(end + 1) {
            Some(c) if keep(c) => end += 1,
            _ => break,
        }
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_from_pdfium_stops_at_the_terminator() {
        // "hi" followed by the terminator and trailing slack, which is what a
        // caller-sized buffer looks like coming back.
        let bytes = [b'h', 0, b'i', 0, 0, 0, 0xff, 0xff];
        assert_eq!(decode_utf16(&bytes), "hi");
        assert_eq!(decode_utf16(&[]), "");
        assert_eq!(decode_utf16(&[0, 0]), "");
    }

    #[test]
    fn a_page_out_of_range_keeps_its_shape_through_the_error_conversion() {
        let converted = to_document_error(PdfError::PageOutOfRange { page: 9, count: 3 });
        assert!(matches!(
            converted,
            DocumentError::NoSuchPage { page: 9, count: 3 }
        ));
        assert!(matches!(
            to_document_error(PdfError::Render("boom".into())),
            DocumentError::Backend(_)
        ));
    }
}
