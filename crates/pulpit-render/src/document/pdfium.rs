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
    MarkStyle, NoteDraft, StampDraft, StampMark, TextSource, NOTE_ICON_POINTS,
};
use pulpit_core::annotation::InkColor;
use pulpit_core::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect};

use crate::pdf::capabilities::{ActionKind, FormType};
use crate::pdf::pdfium::{PdfiumBackend, FPDF_BITMAP_BGRA, FPDF_REVERSE_BYTE_ORDER};
use crate::pdf::{BackendDocumentId, PdfBackend, PdfError};

use super::limits;
use super::model::{
    AnnotationBeforeImage, AnnotationContents, AnnotationSummary, AnnotationSupport,
    CompatibilityLevel, DocumentWarning, FieldFormat, FieldKind, FieldWidget, FormField,
    OpenDocumentInfo, SaveOptions, TextSelection, TextSelectionResult,
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

/// `/F` bit 3, Print: the annotation appears when the page is printed. PDF
/// 12.5.3 makes this opt-in, so a mark without it is a mark that vanishes from
/// the paper.
const FLAG_PRINT: std::os::raw::c_int = 4;

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
    /// Which page an annotation was last found on.
    ///
    /// A PDF has no index by `/NM`, so [`Self::locate`] has to walk `/Annots`.
    /// Walking *every* page to find one mark makes an eraser sweep, an undo
    /// and a replace each cost the whole document; a hint that says which page
    /// to look at first makes them cost one page. It is only ever a hint — the
    /// page is still searched, and a miss falls back to the full walk — so a
    /// stale entry costs a wasted page scan and never a wrong answer.
    located: RefCell<HashMap<AnnotationId, usize>>,
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
    /// The page a text selection is being swept across, held loaded together
    /// with its text layer.
    ///
    /// The second place a native handle outlives a call, and for the same
    /// reason as [`FormBinding::open_page`]: a highlighter drag asks where the
    /// text is on every pointer sample, and `FPDF_LoadPage` +
    /// `FPDFText_LoadPage` re-parse the content stream and rebuild the whole
    /// text layer each time — tens of milliseconds per sample on a text-heavy
    /// page, paid at pointer rate.
    ///
    /// It is a cache of a *read*, so it is dropped before any mutation and in
    /// [`PdfiumDocument::close`]: no handle survives an edit, and a selection
    /// never observes a half-changed page.
    text_page: RefCell<Option<TextPageCache>>,
}

/// A loaded page and its text layer, held across the samples of one drag.
struct TextPageCache {
    page: usize,
    handle: FPDF_PAGE,
    text: FPDF_TEXTPAGE,
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
    /// Whether the press that is still down was intercepted as an overlay
    /// choice rather than handed to the engine.
    ///
    /// A latch rather than a second hit test, because the release of a press
    /// is the release of *that* press wherever the pointer has since travelled
    /// to. Recomputing the interception on the button-up asks a different
    /// question — "is the pointer over an overlay choice now?" — and answers
    /// it about a widget the gesture never began in: drag out of an
    /// intercepted choice and the engine is handed a button-up with no
    /// button-down behind it. Cleared on the up, on a focus change and when
    /// the interaction moves page, all three of which end the gesture.
    overlay_capture: bool,
}

// No `unsafe impl Send` here, on purpose. PDFium is not thread safe, and the
// form-fill environment is worse than merely unsynchronised: its V8 isolate is
// entered on the thread that created it, so moving a document with an open form
// environment to another thread crashes inside V8 instead of failing cleanly.
// The raw pointers this struct holds keep it `!Send`, which is the invariant —
// one document is owned by one execution context (§6) — expressed in the type
// system rather than in a comment.

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
        // The backend starts a form-fill environment of its own so that the
        // render pool can draw field values. This engine is about to start one
        // it can also *type* into, and two on one document draw every field
        // twice. The editor keeps its own; the backend's goes.
        backend.release_form(document);
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
            located: RefCell::new(HashMap::new()),
            form: None,
            text_page: RefCell::new(None),
        };
        engine.info = engine.survey()?;
        engine.open_form_environment();
        engine.note_outward_scripts();
        Ok(engine)
    }

    /// Warn when a field's own scripts try to leave the machine.
    ///
    /// A form that mails or submits itself is a thing the reader should be
    /// told about *before* typing a name into it, not after. pulpit refuses
    /// every one of these at the callback layer, so nothing is sent — but a
    /// refusal nobody is told about is indistinguishable from the document
    /// having asked for nothing.
    ///
    /// Read from the field scripts themselves, which PDFium hands over
    /// decompressed. That is a deliberate limit and worth stating: a *button*
    /// carrying a plain `/A << /S /SubmitForm >>` action dictionary is not
    /// visible here. PDFium's `FPDFAction_GetType` has no value for
    /// `/SubmitForm` — it reports `PDFACTION_UNSUPPORTED` — and the public API
    /// offers no way to walk into an action dictionary, so the only alternative
    /// would be a byte scan that object streams defeat. What is caught is the
    /// common case: `this.submitForm(…)` and friends, called from a field.
    fn note_outward_scripts(&mut self) {
        /// What a script has to name to reach outside the document.
        const REACHES_OUT: [&str; 7] = [
            "submitForm",
            "mailDoc",
            "mailForm",
            "launchURL",
            "getURL",
            "importDataObject",
            "exportDataObject",
        ];
        let Some(form) = self.form_handle() else {
            return;
        };
        let bindings = self.backend.bindings();
        let mut found = false;
        for page in 0..self.info.page_count {
            if found {
                break;
            }
            let scanned = self
                .backend
                .on_page(self.document, page, |handle| {
                    let count = unsafe { bindings.FPDFPage_GetAnnotCount(handle) }.max(0);
                    for index in 0..count.min(limits::MAX_ANNOTATIONS_PER_PAGE as i32) {
                        let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index) };
                        if annotation.is_null() {
                            continue;
                        }
                        let reaches = unsafe { bindings.FPDFAnnot_GetSubtype(annotation) }
                            == FPDF_ANNOT_WIDGET
                            && field_script_reaches_out(bindings, form, annotation, &REACHES_OUT);
                        unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                        if reaches {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                })
                .unwrap_or(false);
            found |= scanned;
        }
        if found {
            self.info.warnings.push(DocumentWarning::ScriptReachesOut);
        }
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
                    overlay_capture: false,
                })
            }
            None => {
                // Said out loud, not only to the log.
                //
                // This used to be a `tracing::warn!` and nothing else, which
                // meant a form that could not be initialised looked exactly
                // like a document with no form: `fields()` answered with an
                // empty list, the level stayed `Native`, and the only trace
                // was a line in a worker process's log that no reader sees.
                // A version mismatch in the form-fill struct hid a
                // twenty-eight-field form that way. §3.4 already requires
                // encryption, XFA, JavaScript and signatures to warn rather
                // than degrade silently; a form that cannot be filled is the
                // same kind of fact and is now told the same way.
                tracing::warn!("this document's form fields cannot be filled");
                self.info.warnings.push(DocumentWarning::FormUnavailable);
                // "Native form semantics absent or unavailable; the page
                // renders as a stable surface and every annotation tool
                // works" — §3.4's definition of this level, which is exactly
                // the situation.
                self.info.level = CompatibilityLevel::AnnotateOnly;
            }
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
            // Lent to the environment so `FFI_GetCurrentPage` can answer while
            // a field script runs; taken back in `release_form_page`, before
            // the handle it lends is closed.
            binding.environment.set_current_page(page.get(), handle);
        }
        Ok(handle)
    }

    /// End the open form interaction, if there is one.
    fn release_form_page(&mut self) {
        let Some(form) = self.form_handle() else {
            return;
        };
        let Some((_, handle)) = self.form.as_mut().and_then(|form| {
            // The gesture cannot outlive the page it began on.
            form.overlay_capture = false;
            form.open_page.take()
        }) else {
            return;
        };
        // The loan comes back before the page it lent goes away, so no script
        // can be handed a pointer to a freed page.
        if let Some(binding) = self.form.as_mut() {
            binding.environment.clear_current_page();
        }
        let bindings = self.backend.bindings();
        unsafe {
            bindings.FORM_OnBeforeClosePage(handle, form);
            bindings.FPDF_ClosePage(handle);
        }
    }

    /// Let go of the page a selection was being swept across, if there is one.
    ///
    /// Takes `&self` because it is called from the read path as well as before
    /// every mutation, and closing a cache entry is not a change to the
    /// document.
    fn release_text_page(&self) {
        let Some(cache) = self.text_page.borrow_mut().take() else {
            return;
        };
        let bindings = self.backend.bindings();
        unsafe {
            bindings.FPDFText_ClosePage(cache.text);
            bindings.FPDF_ClosePage(cache.handle);
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
        region: pulpit_core::notes::Region,
        width: u32,
        height: u32,
        full_size: Option<(u32, u32)>,
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

        // The page is drawn at `full_size`, or at whatever size makes the crop
        // come out at `width` × `height` when the caller named none, and then
        // shifted so the crop's corner lands on the bitmap's. The same
        // placement the page render used, because the two passes have to agree
        // to the pixel.
        let (start_x, start_y, full_width, full_height) =
            crate::pdf::page_placement(region, width, height, full_size);

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
                        start_x,
                        start_y,
                        full_width,
                        full_height,
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
        was_focused: Option<FormField>,
    ) -> Option<crate::document::protocol::CommittedField> {
        use crate::document::protocol::CommittedField;

        // The before-image for whichever field this turns out to be. A commit
        // with no focus is the field that *was* focused; a commit with focus
        // is the same field, still held.
        let previous = |name: &str| -> (String, Vec<u32>) {
            was_focused
                .as_ref()
                .filter(|had| had.name == name)
                .map(|had| (had.value.clone(), had.selected.clone()))
                .unwrap_or_default()
        };

        // The focused widget first: during typing that is the field being
        // edited, and it is exact.
        if let Some(field) = self.focused_form_field(page) {
            let (previous, previous_selected) = previous(&field.name);
            return Some(CommittedField {
                name: field.name,
                value: field.value,
                previous,
                revision: DocumentRevision::INITIAL,
                selected: field.selected,
                previous_selected,
            });
        }

        // Nothing focused now — which is the *usual* way a text field commits,
        // because clicking away or tabbing out is what commits it, and by the
        // time this runs the focus that named the field is gone.
        //
        // So the name is taken from what was focused when the event arrived,
        // captured before it was dispatched, and the value is read back from
        // the document by that name. Answering with an anonymous change here
        // was the old behaviour: enough to bump a revision, useless to a caller
        // that wants to say *which* field it just filled, and invisible because
        // nothing asserted on it.
        let had = was_focused?;
        // Read back from the page the interaction is on, which is where the
        // field that just lost the caret almost always is. Walking the whole
        // document — every page loaded, every widget read — is the fallback
        // for a field mirrored somewhere else, not the cost of every commit.
        let field = self.field_on_page(&had.name, page).or_else(|| {
            self.fields()
                .ok()?
                .into_iter()
                .find(|field| field.name == had.name)
        })?;
        Some(CommittedField {
            name: had.name,
            value: field.value,
            previous: had.value,
            revision: DocumentRevision::INITIAL,
            selected: field.selected,
            previous_selected: had.selected,
        })
    }

    /// One named field, read from one page's widgets only.
    fn field_on_page(&self, name: &str, page: PageIndex) -> Option<FormField> {
        let form = self.form_handle()?;
        let bindings = self.backend.bindings();
        let geometry = self.measure(page).ok()?;
        self.backend
            .on_page(self.document, page.get(), |handle| {
                let count = unsafe { bindings.FPDFPage_GetAnnotCount(handle) }.max(0);
                for index in 0..count.min(limits::MAX_ANNOTATIONS_PER_PAGE as i32) {
                    let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index) };
                    if annotation.is_null() {
                        continue;
                    }
                    let field = (unsafe { bindings.FPDFAnnot_GetSubtype(annotation) }
                        == FPDF_ANNOT_WIDGET)
                        .then(|| read_form_field(bindings, form, annotation, page, &geometry))
                        .flatten()
                        .filter(|field| field.name == name);
                    unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                    if field.is_some() {
                        return Ok(field);
                    }
                }
                Ok(None)
            })
            .ok()
            .flatten()
    }

    /// The field holding the caret, read from the focused annotation.
    ///
    /// One annotation, not a walk of the document — and no page load either:
    /// `FORM_GetFocusedAnnot` answers from the form environment alone. This
    /// runs on every form event, twice — once before to name a commit and once
    /// after to report the focus — so anything heavier here is paid at
    /// keystroke rate.
    fn focused_form_field(&self, page: PageIndex) -> Option<FormField> {
        let form = self.form_handle()?;
        let bindings = self.backend.bindings();
        let geometry = self.measure(page).ok()?;
        let mut index = 0;
        let mut annotation = std::ptr::null_mut();
        let found =
            unsafe { bindings.FORM_GetFocusedAnnot(form, &mut index, &mut annotation) } != 0;
        if !found || annotation.is_null() {
            return None;
        }
        let field = read_form_field(bindings, form, annotation, page, &geometry);
        unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
        field
    }

    /// The choice widget under `point` whose open list the *application*
    /// draws, if one is there (§8.6).
    ///
    /// Answered from the form environment's own open page, for the reason
    /// [`Self::focus_field_widget`] gives: an annotation read off a second
    /// `FPDF_PAGE` for the same page is not one `FORM_SetFocusedAnnot`
    /// recognises. The later annotation wins, because a widget drawn over
    /// another is the one a press lands on.
    ///
    /// Walking the page's widgets costs a read per annotation, and it is paid
    /// on a press rather than on a keystroke or a pointer move.
    fn overlay_choice_widget(
        &self,
        page: PageIndex,
        handle: FPDF_PAGE,
        point: PagePoint,
    ) -> Option<i32> {
        let form = self.form_handle()?;
        let bindings = self.backend.bindings();
        let geometry = self.measure(page).ok()?;
        let count = unsafe { bindings.FPDFPage_GetAnnotCount(handle) }.max(0);
        let mut found = None;
        for index in 0..count.min(limits::MAX_ANNOTATIONS_PER_PAGE as i32) {
            let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index) };
            if annotation.is_null() {
                continue;
            }
            let takes_it = (unsafe { bindings.FPDFAnnot_GetSubtype(annotation) }
                == FPDF_ANNOT_WIDGET)
                .then(|| read_form_field(bindings, form, annotation, page, &geometry))
                .flatten()
                .is_some_and(|field| {
                    application_draws_the_list(&field)
                        && field
                            .widgets
                            .iter()
                            .any(|widget| widget.page == page && widget.bounds.contains(point))
                });
            unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
            if takes_it {
                found = Some(index);
            }
        }
        found
    }

    /// Focus the first widget of `name` on `page`, on the page the form
    /// environment has open, and return that page's handle.
    ///
    /// Focusing by annotation rather than by synthesising a click is what
    /// makes this work for a widget that sits underneath another one.
    ///
    /// The annotations are enumerated from the form environment's own open
    /// page — not from a separately loaded copy of the same page. PDFium
    /// matches a widget to its page *view*, so an annotation read off a second
    /// `FPDF_PAGE` for the same page is not one `FORM_SetFocusedAnnot`
    /// recognises: it returns false, and the field silently refuses to take a
    /// value.
    fn focus_field_widget(&mut self, name: &str, page: PageIndex) -> Result<FPDF_PAGE> {
        let form = self
            .form_handle()
            .ok_or_else(|| DocumentError::Backend("this document has no fillable form".into()))?;
        let handle = self.open_form_page(page)?;
        let bindings = self.backend.bindings();
        let geometry = self.measure(page)?;
        let mut focused = false;
        let count = unsafe { bindings.FPDFPage_GetAnnotCount(handle) }.max(0);
        for index in 0..count.min(limits::MAX_ANNOTATIONS_PER_PAGE as i32) {
            let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index) };
            if annotation.is_null() {
                continue;
            }
            let matches = unsafe { bindings.FPDFAnnot_GetSubtype(annotation) } == FPDF_ANNOT_WIDGET
                && read_form_field(bindings, form, annotation, page, &geometry)
                    .is_some_and(|found| found.name == name);
            if matches {
                focused = unsafe { bindings.FORM_SetFocusedAnnot(form, annotation) } != 0;
                unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                break;
            }
            unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
        }
        if !focused {
            return Err(DocumentError::Backend(format!(
                "the field {name} could not be focused"
            )));
        }
        Ok(handle)
    }

    /// Put `value` into a text-editable field, exactly as a person selecting
    /// all and typing would.
    fn set_text_field(&mut self, field: &FormField, value: &str) -> Result<()> {
        let page = field.widgets[0].page;
        let handle = self.focus_field_widget(&field.name, page)?;
        let form = self
            .form_handle()
            .expect("the widget above was focused through this form");
        let bindings = self.backend.bindings();
        // Select what is there, then replace it. An empty replacement is a
        // cleared field, which is what undoing the first fill of an empty
        // field has to produce.
        unsafe { bindings.FORM_SelectAllText(form, handle) };
        let mut text: Vec<u16> = value.encode_utf16().collect();
        text.push(0);
        unsafe { bindings.FORM_ReplaceSelection(form, handle, text.as_ptr()) };
        // Losing the focus is what commits it, exactly as it is when a person
        // clicks away.
        unsafe { bindings.FORM_ForceToKillFocus(form) };
        self.release_form_page();
        Ok(())
    }

    /// Press a checkbox or one option of a radio group, exactly as a person
    /// clicking it would.
    ///
    /// Text replacement does not edit a button — PDFium accepts the calls and
    /// changes nothing — so the inverse of a toggle has to be a press. The
    /// press is only made when the state differs from what is asked for,
    /// because pressing a checkbox that is already right would toggle it
    /// wrong.
    fn set_button_field(&mut self, field: &FormField, value: &str) -> Result<()> {
        let unchecked = |held: &str| held.is_empty() || held == "Off";
        if field.kind == FieldKind::Checkbox {
            if unchecked(value) == unchecked(&field.value) {
                // Already what was asked for. Pressing anyway would toggle it
                // wrong.
                return Ok(());
            }
            // A checkbox is a toggle, so any of its widgets presses it.
            return self.press_widget(&field.widgets[0]);
        }

        // A radio group. Pressing an option chooses it; nothing a person can
        // press chooses *nothing*, and this path does what a person can do
        // (§8.6). A group that has never been chosen cannot be returned to
        // that state, and saying so beats pretending.
        if unchecked(value) {
            return if unchecked(&field.value) {
                Ok(())
            } else {
                Err(DocumentError::Unsupported(
                    "clear a chosen radio group".into(),
                ))
            };
        }
        if field.value == value {
            return Ok(());
        }
        // Which widget stands for `value`? A group with `/Opt` says so per
        // widget; the common group without one keeps its on-state in each
        // kid's appearance dictionary, whose keys PDFium cannot enumerate. So
        // the stated option is believed where there is one, and otherwise the
        // options are pressed in turn and the group's own value — read back
        // from the page after each press — says when the right one was hit.
        let mut candidates: Vec<&FieldWidget> = field
            .widgets
            .iter()
            .filter(|widget| widget.option.as_deref() == Some(value))
            .collect();
        candidates.extend(
            field
                .widgets
                .iter()
                .filter(|widget| widget.option.is_none()),
        );
        let mut pressed_something = false;
        for widget in candidates {
            self.press_widget(widget)?;
            pressed_something = true;
            let now = self
                .field_on_page(&field.name, widget.page)
                .map(|field| field.value);
            if now.as_deref() == Some(value) {
                return Ok(());
            }
        }
        // Nothing produced the asked-for value. Best effort to leave the
        // group as it was found rather than on whichever option the search
        // ended on; the refusal is the answer either way.
        if pressed_something {
            for widget in &field.widgets {
                self.press_widget(widget)?;
                let now = self
                    .field_on_page(&field.name, widget.page)
                    .map(|found| found.value);
                if now.as_deref() == Some(field.value.as_str()) {
                    break;
                }
            }
        }
        Err(DocumentError::Backend(format!(
            "{} does not offer {value}",
            field.name
        )))
    }

    /// One click in the middle of a widget, exactly as a pointer would make it.
    fn press_widget(&mut self, widget: &FieldWidget) -> Result<()> {
        let handle = self.open_form_page(widget.page)?;
        let form = self
            .form_handle()
            .expect("the page above was opened through this form");
        let bindings = self.backend.bindings();
        let geometry = self.measure(widget.page)?;
        let centre = PagePoint::new(
            (widget.bounds.left + widget.bounds.right) / 2.0,
            (widget.bounds.top + widget.bounds.bottom) / 2.0,
        );
        let (x, y) = geometry.to_user_space(centre);
        unsafe {
            bindings.FORM_OnLButtonDown(form, handle, 0, f64::from(x), f64::from(y));
            bindings.FORM_OnLButtonUp(form, handle, 0, f64::from(x), f64::from(y));
            bindings.FORM_ForceToKillFocus(form);
        }
        self.release_form_page();
        Ok(())
    }

    /// Select the asked-for options of a combo or list box, through
    /// `FORM_SetIndexSelected` — PDFium's own way in, the same one the
    /// interactive `SelectOption` event uses.
    ///
    /// `selected` names the whole target selection when it is non-empty; a
    /// bare `value` names one option, or — for an editable combo box — a
    /// custom value that goes in by typing instead.
    fn set_choice_field(&mut self, field: &FormField, value: &str, selected: &[u32]) -> Result<()> {
        let target: Vec<u32> = if !selected.is_empty() {
            selected.to_vec()
        } else if let Some(index) = field.options.iter().position(|option| option == value) {
            vec![index as u32]
        } else if value.is_empty() {
            Vec::new()
        } else if field.allows_custom_value {
            // Not in the list, but the field takes what is typed.
            return self.set_text_field(field, value);
        } else {
            return Err(DocumentError::Backend(format!(
                "{} does not offer {value}",
                field.name
            )));
        };
        if target
            .iter()
            .any(|index| *index as usize >= field.options.len())
        {
            return Err(DocumentError::Backend(format!(
                "{} has no such option to select",
                field.name
            )));
        }

        let page = field.widgets[0].page;
        let handle = self.focus_field_widget(&field.name, page)?;
        let form = self
            .form_handle()
            .expect("the widget above was focused through this form");
        let bindings = self.backend.bindings();
        for index in 0..field.options.len().min(limits::MAX_FIELD_OPTIONS) {
            let wanted = target.contains(&(index as u32));
            unsafe {
                bindings.FORM_SetIndexSelected(form, handle, index as i32, i32::from(wanted))
            };
        }
        unsafe {
            self.backend.bindings().FORM_ForceToKillFocus(form);
        }
        self.release_form_page();
        Ok(())
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
        self.release_text_page();
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

        // A widget carrying an `/A` action: a submit button, a reset button, or
        // a button that runs a script. Which of the three cannot be told from
        // here — `FPDFAnnot_GetLink` answers null for a widget, and
        // `FPDFAction_GetType` has no value for `/SubmitForm` — but pulpit
        // performs none of them, so one warning covers all three honestly.
        //
        // This is the other half of the reporting `ScriptReachesOut` does: that
        // one reads the field scripts, which catches `this.submitForm(…)`; this
        // one catches the plain action dictionary, which no script mentions.
        let has_button_action = evidence.pages.iter().any(|page| {
            page.annotations.iter().any(|annotation| {
                annotation.subtype == crate::pdf::capabilities::AnnotationSubtype::Widget
                    && annotation.has_action
            })
        });
        if has_button_action {
            warnings.push(DocumentWarning::ButtonAction);
            if level == CompatibilityLevel::Native {
                level = CompatibilityLevel::NativeWithLimitations;
            }
        }

        // A9: an existing signature is detected and warned about *before* the
        // first mutation, not discovered after a save.
        match signature_status(self.backend.bindings(), handle, self.source.as_deref()) {
            SignatureStatus::Signed => warnings.push(DocumentWarning::Signed),
            SignatureStatus::Unsigned => {}
            SignatureStatus::Unknown => warnings.push(DocumentWarning::SignatureUnknown),
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
        // The page this id was last found on, tried first. An annotation does
        // not move between pages under any edit pulpit offers, so this hits
        // for every repeat lookup — which is what an eraser sweep, a replace
        // and an undo are made of.
        let hint = self.located.borrow().get(id).copied();
        if let Some(page) = hint {
            if let Some(index) = self.locate_on(id, PageIndex(page))? {
                return Ok((PageIndex(page), index));
            }
        }
        for page in 0..self.info.page_count {
            if hint == Some(page) {
                continue;
            }
            let page = PageIndex(page);
            if let Some(index) = self.locate_on(id, page)? {
                self.located.borrow_mut().insert(id.clone(), page.get());
                return Ok((page, index));
            }
        }
        // Gone, so the hint is worse than nothing: it would send the next
        // lookup down a page that cannot answer.
        self.located.borrow_mut().remove(id);
        Err(DocumentError::NoSuchAnnotation(id.clone()))
    }

    /// Where `id` sits in one page's `/Annots`, if it is on that page.
    fn locate_on(&self, id: &AnnotationId, page: PageIndex) -> Result<Option<usize>> {
        let found = self.on_annotations(page, |index, annotation, _| {
            match self.string_value(annotation, "NM") {
                Some(name) => (AnnotationId::imported(&name).as_ref() == Some(id)).then_some(index),
                // No `/NM`: this is somebody else's annotation, and its
                // identity is where it sits. Matched only against a session
                // identity for this very page and index, so a named mark and
                // an unnamed one can never answer to each other's id.
                None => (session_id(page, index) == *id).then_some(index),
            }
        })?;
        Ok(found.first().copied())
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
        index: usize,
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
            // (A3). The identity is its place in `/Annots` — the page and the
            // index — and never the PDFium handle: a handle does not survive
            // the event-loop turn it was resolved in (rule 2), so an identity
            // derived from one can never be looked up again, and every edit
            // of a mark another reader wrote failed to find it.
            .unwrap_or_else(|| session_id(page, index));

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

        // The border width is what a viewer regenerating an appearance uses,
        // so it is written even though pulpit supplies its own `/AP`.
        unsafe { bindings.FPDFAnnot_SetBorder(annotation, 0.0, 0.0, style.width) };
        // `/F`. A mark with no flags is a mark that does not print: PDF 12.5.3
        // makes Print opt-in, and a reader who annotates a page and then
        // prints it expects the annotations on the paper. Hidden and NoView
        // are deliberately not set — a mark pulpit cannot see is a mark the
        // reader cannot erase.
        unsafe { bindings.FPDFAnnot_SetFlags(annotation, FLAG_PRINT) };
        // `/M`, the modification date, in the format PDF 7.9.4 asks for.
        // Comment panels sort by it, and an annotation without one sorts
        // arbitrarily among the ones that have it.
        //
        // All three are written *before* the kind-specific part, because that
        // part ends by handing PDFium an appearance and every dictionary
        // write after it is one more chance to disturb it.
        set_string(bindings, annotation, "M", &pdf_date_now())?;

        match draft {
            AnnotationDraft::Ink(ink) => self.write_ink(annotation, ink, geometry)?,
            AnnotationDraft::Highlight(highlight) => {
                self.write_highlight(annotation, highlight, geometry)?
            }
            AnnotationDraft::FreeText(free) => self.write_free_text(annotation, free, geometry)?,
            AnnotationDraft::Note(note) => self.write_note(annotation, note, geometry)?,
            AnnotationDraft::Stamp(stamp) => {
                self.write_stamp(page_handle, annotation, stamp, geometry)?
            }
        }
        Ok(())
    }

    /// Write a sticky note: what it says, who said it, and what it looks like.
    ///
    /// `/Contents` alone is what pulpit used to write, and it is why other
    /// readers made so little of pulpit's notes. A `/Text` annotation is a
    /// small pile of conventions rather than one key: `/Name` chooses the
    /// icon, `/T` is the author every comment panel groups by, `/Subj` is the
    /// heading it shows above the text, and `/AP` is the only thing PDF 2.0
    /// lets a viewer draw. Left out, each one degrades differently in each
    /// viewer, which is exactly the behaviour that was reported.
    fn write_note(
        &self,
        annotation: FPDF_ANNOTATION,
        note: &NoteDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        set_string(bindings, annotation, "Contents", &note.text)?;
        // `/Name` names one of the standard icons of PDF 12.5.6.4. Without it
        // the icon is the viewer's default, which is `/Note` in some and
        // nothing at all in others. `/Comment` is the speech bubble, and it is
        // what the appearance below draws, so the mark looks the same whether
        // a viewer honours the appearance or falls back to the name.
        //
        // Written as a string where the spec asks for a name object, because
        // PDFium's annotation API offers no way to write one. It is a hint
        // rather than the mark: the appearance below is what every viewer
        // actually draws, and a viewer that reads this leniently gets the same
        // speech bubble the appearance draws anyway.
        set_string(bindings, annotation, "Name", "Comment")?;
        set_string(bindings, annotation, "T", &author())?;
        // `/CreationDate` is a note's alone: `/M` says when it was last
        // touched, and a comment panel wants to know when it was written.
        set_string(bindings, annotation, "CreationDate", &pdf_date_now())?;
        // The heading a comment panel puts above the text. "Note" rather than
        // the text's first line: a heading that repeated the note would show
        // the same words twice in every panel that draws both.
        set_string(bindings, annotation, "Subj", "Note")?;
        self.write_note_appearance(annotation, note, geometry)
    }

    /// Draw the note's icon, so every viewer shows the same mark.
    ///
    /// PDFium does generate an appearance for a `/Text` annotation, but it is
    /// generated from `/C` and the viewer's own idea of the icon — which is
    /// how a note ended up as a black tab. A speech bubble drawn here is the
    /// same speech bubble everywhere, and it is drawn for the same reason a
    /// Typst mark carries a picture: what the reader sees when they place a
    /// mark is what every other viewer sees (§7.4).
    ///
    /// Written as a content stream rather than as page objects, which is how
    /// the other appearances in this file are built: `FPDFAnnot_AppendObject`
    /// refuses every subtype but `/Ink` and `/Stamp`, so a note's appearance
    /// has to be handed over as the operators themselves. The coordinates are
    /// the annotation's own, in page user space, because PDFium gives the form
    /// it generates a `/BBox` equal to `/Rect` and no `/Matrix`.
    fn write_note_appearance(
        &self,
        annotation: FPDF_ANNOTATION,
        note: &NoteDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        let rect = geometry.rect_to_user_space(
            AnnotationDraft::Note(note.clone())
                .bounds()
                .expect("a note always has bounds"),
        );
        let (left, bottom) = (rect[0], rect[1]);
        // The icon is designed on a 20-point square — the size a `/Text`
        // annotation is drawn at everywhere — and scaled to whatever the
        // rectangle turned out to be, so a rotated or cropped page does not
        // stretch it.
        let scale_x = (rect[2] - rect[0]) / NOTE_ICON_POINTS;
        let scale_y = (rect[3] - rect[1]) / NOTE_ICON_POINTS;
        let at = |x: f32, y: f32| (left + x * scale_x, bottom + y * scale_y);

        let (r, g, b) = note.style.color.rgb();
        let mut stream = String::new();
        stream.push_str("q\n");
        stream.push_str(&format!("{r:.3} {g:.3} {b:.3} rg\n"));
        // Outlined in the ink the rest of pulpit's marks are drawn in, so a
        // pale note is still a shape on a white page.
        stream.push_str("0.157 0.157 0.157 RG\n");
        stream.push_str(&format!("{:.3} w\n", 0.75 * scale_x));

        // The bubble: a rounded box with a tail at the bottom left, drawn as
        // one closed path so the tail is part of the outline rather than a
        // second shape sitting on top of it. The corners are cut rather than
        // curved — four short chamfers read as a rounded box at icon size and
        // cost no control points.
        let outline = [
            (5.0, 2.0),
            (4.0, 6.0),
            (2.0, 6.0),
            (2.0, 16.0),
            (4.0, 18.0),
            (16.0, 18.0),
            (18.0, 16.0),
            (18.0, 8.0),
            (16.0, 6.0),
            (7.0, 6.0),
        ];
        for (index, (x, y)) in outline.into_iter().enumerate() {
            let (x, y) = at(x, y);
            let operator = if index == 0 { "m" } else { "l" };
            stream.push_str(&format!("{x:.3} {y:.3} {operator}\n"));
        }
        // `b` closes the path, then fills and strokes it.
        stream.push_str("b\n");

        // Three rules inside the bubble: the convention that says the icon is
        // something written rather than something drawn. The last is short,
        // the way a last line of writing is.
        stream.push_str(&format!("{:.3} w\n", 0.9 * scale_y));
        for row in 0..3 {
            let y = 14.5 - row as f32 * 3.0;
            let (from_x, from_y) = at(5.5, y);
            let (to_x, to_y) = at(if row == 2 { 11.0 } else { 14.5 }, y);
            stream.push_str(&format!("{from_x:.3} {from_y:.3} m\n"));
            stream.push_str(&format!("{to_x:.3} {to_y:.3} l\nS\n"));
        }
        stream.push_str("Q\n");

        let rc = unsafe { bindings.FPDFAnnot_SetAP_str(annotation, APPEARANCE_NORMAL, &stream) };
        let back = unsafe {
            bindings.FPDFAnnot_GetAP(annotation, APPEARANCE_NORMAL, std::ptr::null_mut(), 0)
        };
        eprintln!(
            "DEBUG setap rc={rc} readback_len={back} stream_len={}",
            stream.len()
        );
        if rc == 0 {
            return Err(DocumentError::Backend(
                "PDFium refused the note's appearance".into(),
            ));
        }
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
    /// Written as a content stream rather than as page objects, because
    /// `FPDFAnnot_AppendObject` accepts only `/Ink` and `/Stamp`: hung off a
    /// `/FreeText` it refuses every object it is handed, which took the whole
    /// mark down with it and left the text tool unable to place anything at
    /// all. The font is named the way `/DA` names it, so a viewer that draws
    /// this appearance and one that regenerates from `/DA` agree.
    ///
    /// One line of text per line typed, from the top of the box down. No
    /// background is drawn, and none is wanted: a comment written over a page
    /// must not hide the page.
    fn write_free_text_appearance(
        &self,
        annotation: FPDF_ANNOTATION,
        free: &FreeTextDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        let size = free.style.font_size.max(1.0);
        // The leading the editor's own box was sized for, so the line spacing
        // on the page matches the line spacing that was typed into.
        let leading = size * 1.2;
        let (r, g, b) = free.style.color.rgb();
        let rect = geometry.rect_to_user_space(free.rect);
        let (left, top) = (rect[0], rect[3]);

        let mut stream = String::new();
        stream.push_str("q\n");
        stream.push_str(&format!("{r:.3} {g:.3} {b:.3} rg\n"));
        stream.push_str("BT\n");
        // Helvetica because it is one of the fourteen standard faces every
        // conforming viewer has without embedding, and the same face `/DA`
        // names.
        stream.push_str(&format!("/Helv {size:.3} Tf\n"));
        for (index, line) in free.text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            // The baseline of the first line is one line down from the top of
            // the box, in PDF user space, where y grows upwards.
            let baseline = top - leading * (index as f32 + 1.0);
            stream.push_str(&format!("1 0 0 1 {left:.3} {baseline:.3} Tm\n"));
            stream.push_str(&format!("({}) Tj\n", escape_pdf_string(line)));
        }
        stream.push_str("ET\nQ\n");

        if unsafe { bindings.FPDFAnnot_SetAP_str(annotation, APPEARANCE_NORMAL, &stream) } == 0 {
            return Err(DocumentError::Backend(
                "PDFium refused the mark's appearance".into(),
            ));
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
        for chunk in rgba.as_chunks::<4>().0 {
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

/// What a text field's format script makes of its value.
///
/// PDF has no field type for "date". Acrobat's format categories are entries
/// in a standard JavaScript library, and choosing "Date / dd mmmm yyyy" in its
/// field properties writes `AFDate_FormatEx("dd mmmm yyyy")` into the field's
/// `/AA /F` action. So the only way to know a date field is a date field is to
/// read that script, which is what this does — PDFium hands it over
/// decompressed, so no byte scanning is involved.
///
/// Only the format event is read. The keystroke event names the same category
/// and would be a second chance at the same answer, but a field whose format
/// script pulpit cannot parse is a field it should describe as plain text
/// rather than guess about.
fn field_format(
    bindings: &dyn PdfiumLibraryBindings,
    form: FPDF_FORMHANDLE,
    annotation: pdfium_render::prelude::FPDF_ANNOTATION,
) -> FieldFormat {
    /// `FPDF_ANNOT_AACTION_KEY_STROKE` and `FPDF_ANNOT_AACTION_FORMAT`, from
    /// `fpdf_annot.h`.
    const KEYSTROKE_EVENT: i32 = 12;
    const FORMAT_EVENT: i32 = 13;

    if let Some(script) = additional_action_script(bindings, form, annotation, FORMAT_EVENT) {
        // `AFDate_FormatEx("dd mmmm yyyy")` carries its pattern;
        // `AFDate_Format(2)` names one of Acrobat's numbered presets, whose
        // table has been fixed since Acrobat 4 and is translated below so the
        // date picker has a pattern to format into.
        if let Some(pattern) = quoted_argument(&script, "AFDate_FormatEx") {
            return FieldFormat::Date { pattern };
        }
        if script.contains("AFDate_Format") {
            return FieldFormat::Date {
                pattern: numbered_argument(&script, "AFDate_Format")
                    .and_then(date_preset_pattern)
                    .unwrap_or_default(),
            };
        }
        // `AFTime_Format(n)` names one of four presets, the same fixed table
        // Acrobat has carried since it had a time category at all — so the
        // number becomes the pattern the time helper writes into.
        if script.contains("AFTime_Format") {
            return FieldFormat::Time {
                pattern: numbered_argument(&script, "AFTime_Format")
                    .and_then(time_preset_pattern)
                    .unwrap_or_default(),
            };
        }
        // `AFPercent_Format(nDec, sepStyle)`: the decimals are the first
        // argument, and the separator style is the engine's business.
        if script.contains("AFPercent_Format") {
            return FieldFormat::Percent {
                decimals: numbered_argument(&script, "AFPercent_Format")
                    .map(|decimals| decimals.min(u8::MAX as u32) as u8)
                    .unwrap_or(0),
            };
        }
        // `AFNumber_Format(nDec, sepStyle, negStyle, currStyle, strCurrency,
        // bCurrencyPrepend)`. Two of those six are worth carrying: the
        // decimals, and the currency symbol — the first quoted argument,
        // because everything before it is a number.
        if script.contains("AFNumber_Format") {
            return FieldFormat::Number {
                decimals: numbered_argument(&script, "AFNumber_Format")
                    .map(|decimals| decimals.min(u8::MAX as u32) as u8)
                    .unwrap_or(0),
                currency: quoted_argument(&script, "AFNumber_Format").unwrap_or_default(),
            };
        }
        if script.contains("AFSpecial_Format") {
            use crate::document::model::SpecialFormat;
            let kind = match numbered_argument(&script, "AFSpecial_Format") {
                Some(0) => SpecialFormat::Zip,
                Some(1) => SpecialFormat::ZipPlusFour,
                Some(2) => SpecialFormat::Phone,
                Some(3) => SpecialFormat::Ssn,
                _ => SpecialFormat::Unknown,
            };
            return FieldFormat::Special { kind };
        }
    }
    // No recognised format script. An explicit keystroke mask still says what
    // the field takes — `AFSpecial_KeystrokeEx("999-9999")` is how Acrobat
    // writes an arbitrary mask, and it lives on the keystroke event alone.
    if let Some(script) = additional_action_script(bindings, form, annotation, KEYSTROKE_EVENT) {
        if let Some(mask) = quoted_argument(&script, "AFSpecial_KeystrokeEx") {
            return FieldFormat::Special {
                kind: crate::document::model::SpecialFormat::Mask { mask },
            };
        }
    }
    FieldFormat::Plain
}

/// Acrobat's numbered `AFDate_Format` presets, as the patterns its own field
/// properties dialog shows for them.
///
/// The table is `AFDate_Format`'s `cFormat` list, unchanged across Acrobat
/// versions — which is what makes translating a bare number safe. A number
/// beyond it is a document from a future Acrobat, and honesty about that is a
/// date with no pattern rather than a guessed one.
fn date_preset_pattern(preset: u32) -> Option<String> {
    const PRESETS: [&str; 14] = [
        "m/d",
        "m/d/yy",
        "mm/dd/yy",
        "mm/yy",
        "d-mmm",
        "d-mmm-yy",
        "dd-mmm-yy",
        "yy-mm-dd",
        "mmm-yy",
        "mmmm-yy",
        "mmm d, yyyy",
        "mmmm d, yyyy",
        "m/d/yy h:MM tt",
        "m/d/yy HH:MM",
    ];
    PRESETS
        .get(preset as usize)
        .map(|pattern| pattern.to_string())
}

/// Acrobat's four numbered `AFTime_Format` presets, as patterns in the same
/// vocabulary the date patterns use — `HH` a 24-hour hour, `h` a 12-hour one,
/// `MM` minutes, `ss` seconds, `tt` the am/pm marker.
///
/// A number beyond the table is a document from a future Acrobat, and the
/// honest answer to that is a time with no pattern rather than a guessed one.
fn time_preset_pattern(preset: u32) -> Option<String> {
    const PRESETS: [&str; 4] = ["HH:MM", "h:MM tt", "HH:MM:ss", "h:MM:ss tt"];
    PRESETS
        .get(preset as usize)
        .map(|pattern| pattern.to_string())
}

/// The first bare-number argument of a call to `name`, if there is one.
///
/// The counterpart of [`quoted_argument`] for `AFDate_Format(2)` and
/// `AFSpecial_Format(0)`, and deliberately no more of a parser than that one
/// is: what is read is a one-line call written by Acrobat's own form editor.
fn numbered_argument(script: &str, name: &str) -> Option<u32> {
    let start = script.find(name)? + name.len();
    let rest = script.get(start..)?;
    let open = rest.find('(')?;
    let digits: String = rest
        .get(open + 1..)?
        .chars()
        .skip_while(|character| character.is_whitespace())
        .take_while(|character| character.is_ascii_digit())
        .take(4)
        .collect();
    digits.parse().ok()
}

/// The first double-quoted argument of a call to `name`, if there is one.
///
/// Deliberately not a JavaScript parser. What is being read is a one-line call
/// written by Acrobat's own form editor, and the alternative to matching it
/// literally is running the script to find out — which is a great deal more
/// machinery for a field label.
fn quoted_argument(script: &str, name: &str) -> Option<String> {
    let start = script.find(name)? + name.len();
    let rest = script.get(start..)?;
    let open = rest.find('"')?;
    let after = rest.get(open + 1..)?;
    let close = after.find('"')?;
    // A pattern is a handful of characters; anything longer is not one, and is
    // not worth carrying into a label (A8).
    let pattern = after.get(..close)?;
    (!pattern.is_empty() && pattern.len() <= 64).then(|| pattern.to_owned())
}

/// One of a field's four `/AA` scripts, as text.
fn additional_action_script(
    bindings: &dyn PdfiumLibraryBindings,
    form: FPDF_FORMHANDLE,
    annotation: pdfium_render::prelude::FPDF_ANNOTATION,
    event: i32,
) -> Option<String> {
    let length = unsafe {
        bindings.FPDFAnnot_GetFormAdditionalActionJavaScript(
            form,
            annotation,
            event,
            std::ptr::null_mut(),
            0,
        )
    };
    // 2 is the null terminator alone: no script for this event.
    if length <= 2 || length as usize > limits::MAX_METADATA_BYTES {
        return None;
    }
    let mut buffer = vec![0u16; length as usize / 2];
    unsafe {
        bindings.FPDFAnnot_GetFormAdditionalActionJavaScript(
            form,
            annotation,
            event,
            buffer.as_mut_ptr().cast(),
            length,
        )
    };
    Some(
        String::from_utf16_lossy(&buffer)
            .trim_end_matches('\0')
            .to_owned(),
    )
}

/// Whether any of one field's four scripts names something outward-facing.
///
/// The four `/AA` events PDFium exposes — keystroke, format, validate,
/// calculate — are the whole of what a text field can run, so a form that
/// submits itself from a field is caught here whichever of them it hides in.
fn field_script_reaches_out(
    bindings: &dyn pdfium_render::prelude::PdfiumLibraryBindings,
    form: FPDF_FORMHANDLE,
    annotation: pdfium_render::prelude::FPDF_ANNOTATION,
    reaches_out: &[&str],
) -> bool {
    /// `FPDF_ANNOT_AACTION_*`, from `fpdf_annot.h`.
    const EVENTS: [i32; 4] = [12, 13, 14, 15];
    for event in EVENTS {
        let length = unsafe {
            bindings.FPDFAnnot_GetFormAdditionalActionJavaScript(
                form,
                annotation,
                event,
                std::ptr::null_mut(),
                0,
            )
        };
        // 2 is the null terminator alone: no script for this event.
        if length <= 2 || length as usize > limits::MAX_METADATA_BYTES {
            continue;
        }
        let mut buffer = vec![0u16; length as usize / 2];
        unsafe {
            bindings.FPDFAnnot_GetFormAdditionalActionJavaScript(
                form,
                annotation,
                event,
                buffer.as_mut_ptr().cast(),
                length,
            )
        };
        let script: String = String::from_utf16_lossy(&buffer)
            .trim_end_matches('\0')
            .to_owned();
        if reaches_out.iter().any(|name| script.contains(name)) {
            return true;
        }
    }
    false
}

/// Whether a document carries a cryptographic signature — including the
/// answer "pulpit could not tell".
///
/// Three states rather than a bool, for the same reason every operation that
/// leaves the process returns an `Outcome`: "no signature" and "no answer" are
/// different facts, and collapsing them is how a signed file gets reported as
/// unsigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignatureStatus {
    Signed,
    Unsigned,
    Unknown,
}

/// Ask the engine first, and fall back to the file's bytes.
///
/// `FPDF_GetSignatureCount` is the structured answer: it reads the parsed
/// document, so it sees a signature hidden in a compressed object stream,
/// which no byte scan can. It returns -1 when the build cannot answer, and
/// only then is the bounded byte scan consulted — and if that cannot answer
/// either, the status is [`SignatureStatus::Unknown`] rather than a guess.
fn signature_status(
    bindings: &dyn PdfiumLibraryBindings,
    handle: FPDF_DOCUMENT,
    source: Option<&Path>,
) -> SignatureStatus {
    match unsafe { bindings.FPDF_GetSignatureCount(handle) } {
        count if count > 0 => SignatureStatus::Signed,
        0 => SignatureStatus::Unsigned,
        // -1: this PDFium cannot answer. Ask the bytes.
        _ => match source {
            Some(source) => scan_for_signature(source),
            None => SignatureStatus::Unknown,
        },
    }
}

/// Does this file carry a signature, as far as a bounded byte scan can say?
///
/// The fallback only. It cannot see into a compressed object stream and it
/// will not read an unbounded file, so both of those answer
/// [`SignatureStatus::Unknown`] — an oversize file used to be reported as
/// unsigned, which is exactly the belief A9 exists to prevent.
fn scan_for_signature(source: &Path) -> SignatureStatus {
    /// Signature dictionaries live in the trailer's neighbourhood, but an
    /// incrementally updated file can carry them anywhere; 32 MiB covers every
    /// document a person opens by hand and bounds the read.
    const MAX_SCAN_BYTES: u64 = 32 << 20;

    let Ok(metadata) = std::fs::metadata(source) else {
        return SignatureStatus::Unknown;
    };
    if metadata.len() > MAX_SCAN_BYTES {
        return SignatureStatus::Unknown;
    }
    let Ok(bytes) = std::fs::read(source) else {
        return SignatureStatus::Unknown;
    };
    // `/Type /Sig` with any amount of whitespace between the two names, and
    // `/FT /Sig` for the field that carries it.
    if contains_pdf_name_pair(&bytes, b"/Type", b"/Sig")
        || contains_pdf_name_pair(&bytes, b"/FT", b"/Sig")
    {
        SignatureStatus::Signed
    } else if has_compressed_object_streams(&bytes) {
        // The scan reads what it can see, and an object stream is what it
        // cannot: a signature inside one leaves no `/Sig` in the raw bytes.
        SignatureStatus::Unknown
    } else {
        SignatureStatus::Unsigned
    }
}

/// Does this file put objects inside compressed streams?
///
/// If it does, the byte scan's "no signature here" covers only the part of the
/// file it can read, and the honest answer is that it does not know.
fn has_compressed_object_streams(bytes: &[u8]) -> bool {
    contains_pdf_name_pair(bytes, b"/Type", b"/ObjStm")
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

/// One line of text, escaped for a PDF literal string.
///
/// The three characters PDF 7.3.4.2 gives meaning to inside parentheses, and
/// nothing else: an appearance stream carrying an unescaped bracket is a
/// stream that ends in the middle of the reader's own words.
fn escape_pdf_string(line: &str) -> String {
    let mut escaped = String::with_capacity(line.len());
    for character in line.chars() {
        if matches!(character, '(' | ')' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// The identity an annotation with no `/NM` answers to: where it sits.
///
/// Never derived from a PDFium handle. Handles do not survive the event-loop
/// turn they were resolved in (rule 2), so an identity built from one is an
/// identity nothing can ever look up again.
fn session_id(page: PageIndex, index: usize) -> AnnotationId {
    AnnotationId::imported(&format!("session-{}-{}", page.get(), index))
        .expect("a derived name is well formed")
}

/// Who a mark says wrote it, for `/T`.
///
/// The account name, because that is the only name the process has that a
/// reader would recognise in another viewer's comment panel, and pulpit asks
/// for no identity of its own. "pulpit" when there is none — an author key
/// that is present and dull beats one that is absent, which is what makes a
/// comment panel drop the note into an unnamed group.
fn author() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "pulpit".into())
}

/// Now, in the format PDF 7.9.4 asks a date string to be written in.
///
/// UTC, so no local time zone database is consulted and the string is the same
/// wherever the deck is annotated. Hand-rolled rather than pulled from a date
/// library: this is the one clock read in the crate, and it wants a formatter
/// rather than a calendar.
fn pdf_date_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hour, minute, second) = (rest / 3_600, (rest % 3_600) / 60, rest % 60);

    // Howard Hinnant's civil-from-days, shifted to an era beginning in March
    // so the leap day is the last day of the year and needs no special case.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z00'00'")
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

/// `FPDF_FORMFLAG_READONLY` and `FPDF_FORMFLAG_REQUIRED` from `fpdf_annot.h`.
const FORMFLAG_READONLY: std::os::raw::c_int = 1;
const FORMFLAG_REQUIRED: std::os::raw::c_int = 2;
/// The `/Ff` text-field flags PDF 12.7.4.3 defines: a password field, one
/// whose value is a file path, and one whose value is also styled `/RV` rich
/// text. `fpdf_annot.h` names only the first; the other two are the PDF
/// specification's own bit positions, which is where PDFium's come from too.
const FORMFLAG_TEXT_PASSWORD: u32 = 1 << 13;
const FORMFLAG_TEXT_FILE_SELECT: u32 = 1 << 20;
const FORMFLAG_TEXT_RICH_TEXT: u32 = 1 << 25;
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

/// Whether this field's *open list* is the application's to draw (§8.6).
///
/// True for a plain combo box and a list box, whose list is transient viewer
/// chrome: it appears in no saved file, so drawing it outside PDFium costs the
/// one-implementation rule nothing — the chosen index still goes back through
/// `FORM_SetIndexSelected`, and the value and its appearance are still
/// PDFium's.
///
/// False for an *editable* combo box, which is a text box with a list attached
/// and has a caret PDFium is already drawing, and false for a read-only field,
/// which has no list to open at all.
fn application_draws_the_list(field: &FormField) -> bool {
    matches!(field.kind, FieldKind::ComboBox | FieldKind::ListBox)
        && !field.allows_custom_value
        && !field.read_only
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
    let required = flags >= 0 && flags & FORMFLAG_REQUIRED != 0;
    // The text-field variants `/FT Tx` hides behind `/Ff` bits. Only read for
    // a text field, because the same bit positions mean other things for the
    // other kinds.
    let text_flag = |bit: u32| kind == FieldKind::Text && flags >= 0 && flags as u32 & bit != 0;
    let password = text_flag(FORMFLAG_TEXT_PASSWORD);
    let file_select = text_flag(FORMFLAG_TEXT_FILE_SELECT);
    let rich_text = text_flag(FORMFLAG_TEXT_RICH_TEXT);

    // What the field is *for*, when its own format script says so. Only text
    // fields carry one, and reading the script for a checkbox would be work
    // done on every widget of every form to find nothing.
    let format = if kind == FieldKind::Text {
        field_format(bindings, form, annotation)
    } else {
        FieldFormat::Plain
    };

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
        format,
        read_only,
        options,
        allows_custom_value,
        multiple_selection,
        selected,
        required,
        password,
        file_select,
        rich_text,
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
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
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
        self.on_annotations(page, |index, annotation, geometry| {
            self.summarise(index, annotation, page, geometry)
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
        self.release_text_page();
        let page = draft.page();
        // Where it is about to be, so the first lookup after it is made — the
        // undo of this very create, most often — does not walk the document.
        self.located.borrow_mut().insert(id.clone(), page.get());
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
                if std::env::var("PULPIT_DEBUG_NOGEN").is_err() {
                    unsafe { bindings.FPDFPage_GenerateContent(handle) };
                }
                Ok(())
            })
            .map_err(to_document_error)?;

        self.annotation(id)
    }

    fn replace(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary> {
        self.release_text_page();
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
                // An annotation another reader wrote has no `/NM`, and has
                // been answering to the session identity of its place in
                // `/Annots`. Editing it is the moment that identity has to
                // become durable: written here, the mark keeps the same name
                // across the save, so an undo of this very edit still finds
                // it after the file has been reopened (A3).
                let outcome = (|| {
                    if self.string_value(annotation, "NM").is_none() {
                        set_string(bindings, annotation, "NM", id.as_str())?;
                    }
                    self.write_draft(handle, annotation, draft, &geometry)
                })();
                unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                unsafe { bindings.FPDFPage_GenerateContent(handle) };
                outcome.map_err(|error| PdfError::Render(error.to_string()))
            })
            .map_err(to_document_error)?;
        self.annotation(id)
    }

    fn delete(&mut self, id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        self.release_text_page();
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
        self.located.borrow_mut().remove(id);
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

    /// Put a value into a named field — for undo, and through the same editor.
    ///
    /// §8.6 is explicit that there is exactly one editing surface: values are
    /// typed on the page, by PDFium, under the field's own `/DA`. This does not
    /// break that rule, and the way it is written is the reason. It does not
    /// touch `/V`, does not generate an appearance and does not decide what the
    /// value looks like. It focuses the widget, selects what is in it and
    /// replaces the selection — three calls into the form-fill environment,
    /// after which PDFium has done exactly what it does when a person selects
    /// all and types. The comb spacing, the auto-sizing, the quadding and the
    /// field's own format script all still happen, once, in PDFium.
    ///
    /// What this exists for is the inverse of a fill: undoing a typed field
    /// value needs to put the old one back, and there is no other way to say
    /// that (§9.1). Its one forward caller is the date and time pickers, which
    /// commit a value someone *chose* rather than typed — the text is the
    /// application's, the appearance and the format script are still PDFium's,
    /// and the edit is ordinary enough to undo like any other. Nothing else in
    /// the application sets a field from outside the page.
    ///
    /// The mechanism follows the kind, because PDFium's editor does: a text
    /// value is typed, a button is pressed, a choice is selected. Replacing
    /// the selection of a checkbox edits nothing at all — silently — which is
    /// why the dispatch is on [`FieldKind`] rather than one path for all.
    fn set_field(&mut self, name: &str, value: &str, selected: &[u32]) -> Result<String> {
        self.form_handle()
            .ok_or_else(|| DocumentError::Backend("this document has no fillable form".into()))?;

        // Where the field is. A field with no widget on any page cannot be
        // focused and so cannot be edited — which is the honest answer for a
        // field that is not on a page rather than a silent success.
        let field = self
            .fields()?
            .into_iter()
            .find(|field| field.name == name)
            .ok_or_else(|| DocumentError::NoSuchField(name.to_string()))?;
        if field.read_only {
            return Err(DocumentError::Backend(format!(
                "the field {name} is read-only"
            )));
        }
        if field.widgets.is_empty() {
            return Err(DocumentError::NoSuchField(name.to_string()));
        }

        match field.kind {
            FieldKind::Checkbox | FieldKind::RadioGroup => {
                self.set_button_field(&field, value)?;
            }
            FieldKind::ComboBox | FieldKind::ListBox => {
                self.set_choice_field(&field, value, selected)?;
            }
            _ => self.set_text_field(&field, value)?,
        }
        self.field_value(name)
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
        // Who has the caret *now*, before this event moves it. A focus loss
        // commits the field it is leaving, and by the time the event has been
        // handled there is nothing focused left to name it.
        let was_focused = self.focused_form_field(page);
        let dropping_focus = matches!(&event, FormInputEvent::Focus { gained: false });
        // What a copy read out, filled in by the one event that asks for it.
        let mut selected_text: Option<String> = None;
        // A press on a non-editable choice widget is answered with *focus*
        // rather than with the click itself. `FORM_OnLButtonDown` on one of
        // those opens PDFium's own list, drawn into the page bitmap and
        // reported back a sliver at a time, with a round trip per hovered row;
        // the application draws the list instead and commits what is chosen
        // through `SelectOption` (§8.6). Which widgets those are is read from
        // the document — an editable combo keeps the engine's list — rather
        // than decided here.
        let overlay_press = match &event {
            FormInputEvent::PointerDown { at: point } => {
                self.overlay_choice_widget(page, handle, *point)
            }
            _ => None,
        };
        let opened_choice = overlay_press.is_some();
        // The release is decided by the latch the press set, not by a fresh
        // hit test — see `FormBinding::overlay_capture`. Set here and cleared
        // on the up, on a focus event and when the page moves.
        match (&event, self.form.as_mut()) {
            (FormInputEvent::PointerDown { .. }, Some(binding)) => {
                binding.overlay_capture = overlay_press.is_some();
            }
            (FormInputEvent::Focus { .. }, Some(binding)) => binding.overlay_capture = false,
            _ => {}
        }
        let held_overlay_press = match &event {
            FormInputEvent::PointerUp { .. } => {
                let held = self.form.as_ref().is_some_and(|form| form.overlay_capture);
                if let Some(binding) = self.form.as_mut() {
                    binding.overlay_capture = false;
                }
                held
            }
            _ => false,
        };
        unsafe {
            match event {
                FormInputEvent::PointerDown { at: point } => match overlay_press {
                    // Focus alone: the widget takes the caret, and its list
                    // stays shut because the application is about to draw one.
                    Some(index) => {
                        let annotation = bindings.FPDFPage_GetAnnot(handle, index);
                        if !annotation.is_null() {
                            bindings.FORM_SetFocusedAnnot(form, annotation);
                            bindings.FPDFPage_CloseAnnot(annotation);
                        }
                    }
                    None => {
                        let (x, y) = at(point);
                        bindings.FORM_OnLButtonDown(form, handle, 0, f64::from(x), f64::from(y));
                    }
                },
                // The release of a press the engine never saw is not the
                // engine's either: forwarding it alone would land a button-up
                // in a widget with no button-down behind it.
                FormInputEvent::PointerUp { at: point } if !held_overlay_press => {
                    let (x, y) = at(point);
                    bindings.FORM_OnLButtonUp(form, handle, 0, f64::from(x), f64::from(y));
                }
                FormInputEvent::PointerUp { .. } => {}
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
                FormInputEvent::KeyDown { key, modifiers } => match control_character(key) {
                    Some(character) => {
                        bindings.FORM_OnChar(form, handle, character, modifiers.flags());
                    }
                    None => {
                        // The modifier flags are what make shift-arrow
                        // *extend* the field's selection rather than move the
                        // caret: PDFium reads them out of this argument, and a
                        // zero here is the engine being told nothing was held.
                        bindings.FORM_OnKeyDown(form, handle, key_code(key), modifiers.flags());
                    }
                },
                FormInputEvent::KeyUp { key, modifiers } => {
                    bindings.FORM_OnKeyUp(form, handle, key_code(key), modifiers.flags());
                }
                FormInputEvent::Focus { gained: false } => {
                    // Losing focus is what *commits* an in-progress edit,
                    // which is why it is an event rather than a hint.
                    bindings.FORM_ForceToKillFocus(form);
                }
                FormInputEvent::Focus { gained: true } => {}
                FormInputEvent::SelectOption { index, selected } => {
                    // Per-index, which is what makes one event enough for both
                    // kinds of field: PDFium clears the other options itself
                    // on a single-select field and leaves them alone on a
                    // multi-select one, so this is "choose only this" or
                    // "toggle this" without the caller having to know which.
                    bindings.FORM_SetIndexSelected(form, handle, index as i32, i32::from(selected));

                    // A multi-select list box's selection is held in the
                    // form-fill widget until the field loses focus, and only
                    // then written to `/V` and `/I`. That is fine for a list
                    // PDFium is drawing — it draws the pending state — and no
                    // use at all for one the application draws, which can only
                    // read the committed field: every tick would come back
                    // unticked and the rows would show the selection as it was
                    // one press ago.
                    //
                    // So the focus is dropped and put straight back on the
                    // same widget, which is what makes PDFium commit. The
                    // price is one commit — and therefore one undo entry — per
                    // tick rather than one per visit to the field, which is
                    // the honest record anyway: each tick is a change a person
                    // made and may want back on its own (§8.6).
                    //
                    // Only for the multi-select case. A single-select field
                    // commits on the selection itself, and killing its focus
                    // would cost a round of appearance generation for nothing.
                    let mut focused = std::ptr::null_mut();
                    let mut focused_page = 0;
                    let refocus =
                        (bindings.FORM_GetFocusedAnnot(form, &mut focused_page, &mut focused) != 0
                            && !focused.is_null())
                        .then_some(focused)
                        .filter(|annotation| {
                            let flags = bindings.FPDFAnnot_GetFormFieldFlags(form, *annotation);
                            flags >= 0
                                && flags
                                    & (FPDF_FORMFLAG_CHOICE_MULTI_SELECT as std::os::raw::c_int)
                                    != 0
                        });
                    if let Some(annotation) = refocus {
                        bindings.FORM_ForceToKillFocus(form);
                        bindings.FORM_SetFocusedAnnot(form, annotation);
                    }
                    if !focused.is_null() {
                        bindings.FPDFPage_CloseAnnot(focused);
                    }
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
                // The clipboard trio. All three are PDFium's own entry points
                // into the selection it made, which is the only place the
                // selection exists: this layer forwarded the clicks and the
                // shift-arrows that built it and never modelled the result.
                FormInputEvent::CopySelection => {
                    selected_text = form_string(bindings, |buffer, length| {
                        bindings.FORM_GetSelectedText(
                            form,
                            handle,
                            buffer as *mut std::os::raw::c_void,
                            length,
                        )
                    });
                }
                FormInputEvent::ReplaceSelection { ref text } => {
                    // NUL-terminated UTF-16LE, the shape every `FPDF_WIDESTRING`
                    // takes; the same conversion `set_field` does for an undo.
                    let mut units: Vec<u16> = text.encode_utf16().collect();
                    units.push(0);
                    bindings.FORM_ReplaceSelection(form, handle, units.as_ptr());
                }
                FormInputEvent::SelectAll => {
                    bindings.FORM_SelectAllText(form, handle);
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
        let invalidated: Vec<PageRect> = form
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
        // What the document's own JavaScript asked for while its field scripts
        // ran. Drained here, with the rest of what the event left behind, so a
        // request is reported exactly once (§8.6).
        let requests = form.environment.take_requests();
        let text_focus = form.environment.has_text_focus();
        let focused = self.focused_form_field(page);
        let focused_hint = focused.as_ref().and_then(|field| {
            // A file-select field looks exactly like a text field and can
            // never be filled — its value is a path a picker pulpit refuses
            // to open — so what it is has to be said the moment the caret
            // arrives, before typing into it teaches the wrong lesson.
            if field.file_select {
                Some("file path this viewer cannot choose".into())
            } else {
                field.format.hint()
            }
        });
        let focused_date = focused.as_ref().and_then(|field| match &field.format {
            FieldFormat::Date { pattern } => {
                let widget = field.widgets.first()?;
                Some(crate::document::protocol::FocusedDate {
                    field: field.name.clone(),
                    pattern: pattern.clone(),
                    page: widget.page,
                    bounds: widget.bounds,
                })
            }
            _ => None,
        });
        let focused_time = focused.as_ref().and_then(|field| {
            let pattern = field.format.time_pattern()?;
            let widget = field.widgets.first()?;
            Some(crate::document::protocol::FocusedTime {
                field: field.name.clone(),
                pattern: pattern.to_owned(),
                value: field.value.clone(),
                page: widget.page,
                bounds: widget.bounds,
            })
        });
        // Where to draw the focus ring. The widget on *this* page and no
        // other: a field's widgets can sit on several pages, and the event
        // names one.
        let focused_widget = focused.as_ref().and_then(|field| {
            let widget = field.widgets.iter().find(|widget| widget.page == page)?;
            Some(crate::document::protocol::FocusedWidget {
                field: field.name.clone(),
                page: widget.page,
                bounds: widget.bounds,
            })
        });
        // Reported for a list box as well as for a combo box: a list box needs
        // nothing from the arrow keys — it moves its own selection — but its
        // rows are drawn by the application on the same terms as a combo's,
        // and drawing them takes the labels and the widget's rectangle.
        let focused_choice = focused.as_ref().and_then(|field| {
            if !matches!(field.kind, FieldKind::ComboBox | FieldKind::ListBox) {
                return None;
            }
            let widget = field
                .widgets
                .iter()
                .find(|widget| widget.page == page)
                .or_else(|| field.widgets.first())?;
            Some(crate::document::protocol::FocusedChoice {
                field: field.name.clone(),
                selected: field.selected.first().copied(),
                // Every chosen row, not just the first. A multi-select list
                // box reports three of these and the drawn list ticks three
                // rows; anything else reports one or none and this is the same
                // answer `selected` gives.
                selections: field.selected.clone(),
                options: field.options.len().min(u32::MAX as usize) as u32,
                labels: field.options.clone(),
                editable: field.allows_custom_value,
                multiple_selection: field.multiple_selection,
                list_box: field.kind == FieldKind::ListBox,
                page: widget.page,
                bounds: widget.bounds,
            })
        });
        // Only the *editable* combo boxes reach this now: a press on any other
        // choice widget is answered with focus alone and its list is drawn by
        // the application, so PDFium has no popup on the page to composite.
        // The widening stays for the fields that still open one.
        //
        // A focused combo box may have its list open, and PDFium draws that
        // list into the page while reporting only the slivers that changed —
        // opening it invalidates a few pixels of border, not the rows about to
        // appear. The caller composites exactly what is invalidated, so the
        // popup would arrive on screen in fragments. Widening to the area the
        // list can occupy costs redrawing pixels that come out identical;
        // under-reporting costs a dropdown visibly missing its own rows.
        let mut invalidated = invalidated;
        if let (Some(field), Some(_)) = (&focused, &focused_choice) {
            if let Some(widget) = field.widgets.iter().find(|widget| widget.page == page) {
                if !invalidated.is_empty() {
                    let row = (widget.bounds.bottom - widget.bounds.top).max(4.0);
                    // One row per option plus the field itself, capped: the
                    // popup never grows past what PDFium could draw on the
                    // page below the field.
                    let rows = (field.options.len() as f32 + 1.0).min(24.0);
                    invalidated.push(PageRect::new(
                        widget.bounds.left,
                        widget.bounds.top,
                        widget.bounds.right,
                        (widget.bounds.bottom + rows * row).min(geometry.height),
                    ));
                }
            }
        }
        // Coalesced rather than truncated when the widening above tips the
        // list past what the wire carries. `take_dirty` bounds what PDFium
        // reported, and this adds one more to it, so a form event that
        // dirtied exactly the maximum came out one over — which was an error
        // rather than a redraw, and the event was lost. A multi-select tick
        // reaches that: the refocus that commits it invalidates the list's
        // rows twice over. One rectangle covering them all repaints the same
        // pixels in a single patch, which is what the caller would have
        // composited anyway.
        if invalidated.len() > limits::MAX_DIRTY_RECTS {
            let whole = invalidated[1..].iter().fold(invalidated[0], |all, one| {
                PageRect::new(
                    all.left.min(one.left),
                    all.top.min(one.top),
                    all.right.max(one.right),
                    all.bottom.max(one.bottom),
                )
            });
            invalidated = vec![whole];
        }

        // Which field changed is read back from the document rather than
        // guessed from the event: PDFium knows what it committed and the
        // caller of this does not. Read *before* the page is released, while
        // the focus that names the field is still there.
        let committed = if changed {
            self.committed_field(page, was_focused)
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
            requests,
            text_focus,
            focused_choice,
            opened_choice,
            focused_hint,
            focused_date,
            focused_time,
            focused_widget,
            selected_text,
        })
    }

    fn select_text(
        &self,
        page: PageIndex,
        selection: TextSelection,
    ) -> Result<TextSelectionResult> {
        let geometry = self.geometry_of(page)?;
        let bindings = self.backend.bindings();

        // The samples of one drag all land on the same page, so the loaded
        // page and its text layer are kept between them rather than rebuilt
        // per sample. A different page ends the previous one first.
        if self.text_page.borrow().as_ref().map(|cache| cache.page) != Some(page.get()) {
            self.release_text_page();
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
                    "cannot load page {} to select text on",
                    page.get()
                )));
            }
            let text = unsafe { bindings.FPDFText_LoadPage(handle) };
            if text.is_null() {
                // No text layer is an empty result, not a failure (§6.3).
                unsafe { bindings.FPDF_ClosePage(handle) };
                return Ok(TextSelectionResult::default());
            }
            *self.text_page.borrow_mut() = Some(TextPageCache {
                page: page.get(),
                handle,
                text,
            });
        }

        let cache = self.text_page.borrow();
        let text = cache
            .as_ref()
            .expect("the text page was just loaded for this page")
            .text;
        Ok(resolve_selection(bindings, text, &geometry, selection))
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
        // What is serialised must not be a document with a page held open
        // behind the writer's back.
        self.release_text_page();
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
    fn render_page(
        &self,
        page: PageIndex,
        region: pulpit_core::notes::Region,
        width: u32,
        height: u32,
        full_size: Option<(u32, u32)>,
        rgba: &mut [u8],
    ) -> Result<()> {
        let request = crate::pdf::RenderRequest {
            document: self.document,
            page: page.get(),
            region,
            width,
            height,
            full_size,
            with_annotations: true,
        };
        request.validate().map_err(to_document_error)?;
        PdfBackend::render_into(self.backend, &request, rgba, &crate::pdf::NeverCancel)
            .map_err(to_document_error)?;
        // Live field contents are drawn over the crop, not over the page, so
        // the pass has to know which part of the page it is looking at.
        self.composite_form_fields(page, region, width, height, full_size, rgba)
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

    /// Write a file of `bytes` bytes, sparsely where the filesystem allows it,
    /// so the oversize path can be exercised without a 32 MiB fixture in the
    /// repository or 32 MiB of writing.
    fn sparse_file(directory: &Path, name: &str, bytes: u64) -> std::path::PathBuf {
        let path = directory.join(name);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(bytes).unwrap();
        path
    }

    #[test]
    fn a_file_too_large_to_scan_is_unknown_rather_than_unsigned() {
        // The bug this replaced: a signed 40 MiB file reported as carrying no
        // signature, which is the one answer A9 must never give.
        let directory = tempfile::tempdir().unwrap();
        let big = sparse_file(directory.path(), "big.pdf", (32 << 20) + 1);
        assert_eq!(scan_for_signature(&big), SignatureStatus::Unknown);
    }

    #[test]
    fn a_scannable_file_is_read_and_answered_either_way() {
        let directory = tempfile::tempdir().unwrap();
        let signed = directory.path().join("signed.pdf");
        std::fs::write(&signed, b"%PDF-1.7\n1 0 obj << /Type /Sig >> endobj\n").unwrap();
        assert_eq!(scan_for_signature(&signed), SignatureStatus::Signed);

        let field = directory.path().join("field.pdf");
        std::fs::write(&field, b"%PDF-1.7\n1 0 obj << /FT/Sig >> endobj\n").unwrap();
        assert_eq!(scan_for_signature(&field), SignatureStatus::Signed);

        let plain = directory.path().join("plain.pdf");
        std::fs::write(&plain, b"%PDF-1.7\n1 0 obj << /Type /Page >> endobj\n").unwrap();
        assert_eq!(scan_for_signature(&plain), SignatureStatus::Unsigned);
    }

    #[test]
    fn a_compressed_object_stream_is_unknown_because_the_scan_cannot_see_in() {
        // A signature inside an object stream leaves no `/Sig` in the raw
        // bytes, so "not found" is not "not there".
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("objstm.pdf");
        std::fs::write(
            &path,
            b"%PDF-1.7\n1 0 obj << /Type /ObjStm /N 4 >> stream\n....\nendstream endobj\n",
        )
        .unwrap();
        assert_eq!(scan_for_signature(&path), SignatureStatus::Unknown);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_unknown() {
        assert_eq!(
            scan_for_signature(Path::new("/nowhere/at/all.pdf")),
            SignatureStatus::Unknown
        );
    }

    #[test]
    fn acrobats_numbered_date_presets_translate_to_their_patterns() {
        // The fixed table Acrobat's own field-properties dialog writes
        // `AFDate_Format(n)` from. The old, still very common form of the
        // script — a bare number instead of a pattern — used to reach the
        // date picker as a date with nothing to format into.
        assert_eq!(
            numbered_argument("AFDate_Format(2);", "AFDate_Format"),
            Some(2)
        );
        assert_eq!(
            numbered_argument("AFDate_Format( 13 );", "AFDate_Format"),
            Some(13)
        );
        assert_eq!(numbered_argument("AFDate_Format();", "AFDate_Format"), None);
        assert_eq!(date_preset_pattern(0).as_deref(), Some("m/d"));
        assert_eq!(date_preset_pattern(2).as_deref(), Some("mm/dd/yy"));
        assert_eq!(date_preset_pattern(11).as_deref(), Some("mmmm d, yyyy"));
        assert_eq!(
            date_preset_pattern(99),
            None,
            "a preset from a future Acrobat is honestly patternless"
        );
    }

    #[test]
    fn the_time_presets_become_patterns_a_helper_can_write_into() {
        // The same bargain the date presets strike: `AFTime_Format(1)` says
        // nothing a person can read, and the table behind it has not moved
        // since Acrobat had a time category at all.
        assert_eq!(time_preset_pattern(0).as_deref(), Some("HH:MM"));
        assert_eq!(time_preset_pattern(1).as_deref(), Some("h:MM tt"));
        assert_eq!(time_preset_pattern(3).as_deref(), Some("h:MM:ss tt"));
        assert_eq!(
            time_preset_pattern(9),
            None,
            "a preset outside the table is honestly patternless"
        );
    }

    #[test]
    fn a_number_script_gives_up_its_decimals_and_its_currency() {
        // `AFNumber_Format(nDec, sepStyle, negStyle, currStyle, strCurrency,
        // bCurrencyPrepend)`: the first number and the first quoted argument
        // are the two a person typing into the field can act on.
        let script = "AFNumber_Format(2, 0, 0, 0, \"$\", true);";
        assert_eq!(numbered_argument(script, "AFNumber_Format"), Some(2));
        assert_eq!(
            quoted_argument(script, "AFNumber_Format").as_deref(),
            Some("$")
        );
        // A script with no currency at all leaves the symbol unclaimed rather
        // than borrowing one from further along the line.
        assert_eq!(
            quoted_argument(
                "AFNumber_Format(0, 1, 0, 0, \"\", false);",
                "AFNumber_Format"
            ),
            None
        );
        // And a percentage carries its decimals in the same first position.
        assert_eq!(
            numbered_argument("AFPercent_Format(1, 0);", "AFPercent_Format"),
            Some(1)
        );
    }

    #[test]
    fn a_number_hint_says_the_shape_before_it_is_typed_wrong() {
        use crate::document::model::FieldFormat;

        // What the tooltip over the field reads. The decimals matter because
        // a field that rewrites 3 as 3.00 has already surprised someone.
        let hint = |format: FieldFormat| format.hint().unwrap_or_default();
        assert_eq!(
            hint(FieldFormat::Number {
                decimals: 0,
                currency: String::new()
            }),
            "a number"
        );
        assert_eq!(
            hint(FieldFormat::Number {
                decimals: 2,
                currency: String::new()
            }),
            "number, 2 decimals"
        );
        assert_eq!(
            hint(FieldFormat::Number {
                decimals: 2,
                currency: "€".into()
            }),
            "number in €, 2 decimals"
        );
        assert_eq!(
            hint(FieldFormat::Number {
                decimals: 0,
                currency: "$".into()
            }),
            "a number in $"
        );
        assert_eq!(hint(FieldFormat::Percent { decimals: 0 }), "a percentage");
        assert_eq!(
            hint(FieldFormat::Percent { decimals: 1 }),
            "percentage, 1 decimal"
        );
        assert_eq!(
            hint(FieldFormat::Time {
                pattern: "h:MM tt".into()
            }),
            "time, as h:MM tt"
        );
        assert_eq!(
            hint(FieldFormat::Time {
                pattern: String::new()
            }),
            "a time"
        );
    }

    #[test]
    fn the_special_formats_are_told_apart_by_their_number() {
        use crate::document::model::{FieldFormat, SpecialFormat};
        // The hint is the point: "a phone number" beats "a formatted value",
        // and beats learning the shape from a rejection alert after typing.
        for (kind, expectation) in [
            (SpecialFormat::Zip, "a ZIP code"),
            (SpecialFormat::Phone, "a phone number"),
            (SpecialFormat::Ssn, "a Social Security number"),
            (
                SpecialFormat::Mask {
                    mask: "999-9999".into(),
                },
                "a value shaped 999-9999",
            ),
        ] {
            assert_eq!(
                FieldFormat::Special { kind }.hint().as_deref(),
                Some(expectation)
            );
        }
    }

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
