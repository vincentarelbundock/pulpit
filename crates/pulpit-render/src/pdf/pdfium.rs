//! PDFium backend, driven through the raw `pdfium-render` bindings.
//!
//! Two things matter here and neither is available through a high-level
//! wrapper:
//!
//! 1. **Progressive rendering.** `FPDF_RenderPageBitmap_Start` plus a pause
//!    callback and `FPDF_RenderPage_Continue` is the only way an obsolete or
//!    lower-priority page yields promptly instead of holding a worker for the
//!    length of a complex render. The specification makes this a gating
//!    criterion for the binding; `pdfium-render` exposes all three calls, so
//!    the binding passes.
//! 2. **Rendering into our own buffer.** `FPDFBitmap_CreateEx` lets PDFium
//!    draw straight into the shared-memory region the supervisor already
//!    owns, avoiding a copy per frame.
//!
//! This module runs *only inside a renderer worker process*. A crash here
//! takes down a worker, never the presentation.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use pdfium_render::prelude::{
    Pdfium, PdfiumLibraryBindings, FPDF_ATTACHMENT, FPDF_BOOKMARK, FPDF_DOCUMENT, FPDF_FORMHANDLE,
    FPDF_LINK, FPDF_PAGE, FS_RECTF, IFSDK_PAUSE,
};

/// PDFium ABI constants that `pdfium-render` does not re-export.
pub(crate) const FPDF_BITMAP_BGRA: i32 = 4;
const FPDF_RENDER_TOBECONTINUED: u32 = 1;
const FPDF_RENDER_DONE: u32 = 2;
/// `FPDF_REVERSE_BYTE_ORDER` from `fpdfview.h`: PDFium fills the BGRA bitmap
/// in RGBA byte order instead. The UI and the protocol only ever consume
/// RGBA, so rendering it directly saves a full read-modify-write pass over
/// every frame.
pub(crate) const FPDF_REVERSE_BYTE_ORDER: i32 = 0x10;
/// `PDFACTION_*` action types from `fpdf_doc.h`.
const PDFACTION_GOTO: std::ffi::c_ulong = 1;
const PDFACTION_URI: std::ffi::c_ulong = 3;
/// `PDFDEST_VIEW_FITR` from `fpdf_doc.h`: a destination that names an exact
/// rectangle of the page (beamer's `\framezoom`). The only view kind that
/// carries a zoom; the others fall back to plain page navigation.
const PDFDEST_VIEW_FITR: std::ffi::c_ulong = 5;
/// A URI longer than this is discarded rather than shipped over IPC.
const MAX_URI_BYTES: u64 = 2048;
/// More link annotations than any real slide carries; bounds the message.
const MAX_LINKS_PER_PAGE: usize = 512;
/// The most hits one search request answers with, and the most runs one hit
/// carries. The same bounds the document worker uses, for the same reason:
/// the hits are held in memory and drawn over every visible page.
const MAX_HITS_PER_SEARCH: usize = crate::document::limits::MAX_HITS_PER_SEARCH;
const MAX_QUADS_PER_HIT: usize = crate::document::limits::MAX_QUADS_PER_HIT;
/// More embedded files than any real deck carries.
const MAX_ATTACHMENTS: i32 = 4096;
/// An attachment name longer than this is not one a producer wrote.
const MAX_ATTACHMENT_NAME_BYTES: u64 = 1024;
/// Attachment bytes are held in memory once before staging, so the ceiling
/// here is the protocol's: anything larger is refused, never truncated.
const MAX_ATTACHMENT_FILE_BYTES: u64 = crate::protocol::MAX_ATTACHMENT_BYTES;
/// A page label longer than this is not a logical-slide marker.
const MAX_PAGE_LABEL_BYTES: u64 = 256;
/// A bookmark title longer than this is truncated before it is even decoded;
/// the domain truncates again, in characters, once it is a string.
const MAX_BOOKMARK_TITLE_BYTES: u64 = 4096;
/// Document-level scripts are only counted and named, never run, so a handful
/// of names is all the report needs.
const MAX_JAVASCRIPT_ACTIONS: i32 = 64;
/// A script name longer than this tells the presenter nothing more.
const MAX_JAVASCRIPT_NAME_BYTES: u64 = 512;
/// Annotations inspected per page when collecting capability evidence. A page
/// with more than this is not a slide, and the findings would be identical.
const MAX_ANNOTATIONS_PER_PAGE: i32 = 256;
/// Pages inspected for capability evidence. Beyond this the report is already
/// summarised, and inspection costs a page load each.
const MAX_INSPECTED_PAGES: usize = 512;
/// `PDFACTION_*` values `link_target` does not need but the capability scan
/// reports on.
const PDFACTION_UNSUPPORTED: std::ffi::c_ulong = 0;
const PDFACTION_REMOTEGOTO: std::ffi::c_ulong = 2;
const PDFACTION_LAUNCH: std::ffi::c_ulong = 4;
const PDFACTION_EMBEDDEDGOTO: std::ffi::c_ulong = 5;
/// `FPDF_FORMTYPE_*` from `fpdf_formfill.h`.
const FPDF_FORMTYPE_NONE: i32 = 0;
const FPDF_FORMTYPE_XFA_FULL: i32 = 2;
const FPDF_FORMTYPE_XFA_FOREGROUND: i32 = 3;
/// `FPDF_ANNOT_*` subtypes the capability scan decides on.
const FPDF_ANNOT_SOUND: u32 = 18;
const FPDF_ANNOT_MOVIE: u32 = 19;
const FPDF_ANNOT_WIDGET: u32 = 20;
const FPDF_ANNOT_SCREEN: u32 = 21;
const FPDF_ANNOT_THREED: u32 = 25;
const FPDF_ANNOT_RICHMEDIA: u32 = 26;
const FPDF_ANNOT_LINK: u32 = 2;
use pulpit_core::navigation::{Outline, OutlineSource};
use pulpit_core::notes::Region;
use pulpit_core::{LinkTarget, PageLink, PageSize};

use crate::pdf::capabilities::{
    scan_transition_styles, ActionKind, AnnotationEvidence, AnnotationSubtype, DocumentEvidence,
    FormType, PageEvidence, RestrictionEvidence,
};
use crate::pdf::{
    BackendDocumentId, CancelSignal, DocumentMetadata, PdfBackend, PdfError, RenderRequest,
    RenderedPage, Result,
};

static BOUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Metadata tags searched for the notes-mapping contract, in order.
const METADATA_TAGS: [&str; 3] = ["Keywords", "Subject", "Title"];

pub struct PdfiumBackend {
    bindings: Box<dyn PdfiumLibraryBindings>,
    documents: HashMap<u64, FPDF_DOCUMENT>,
    /// Where each open document came from. Kept only so the capability scan
    /// can read the file's bytes for the one feature PDFium exposes no
    /// accessor for: page transitions.
    paths: HashMap<u64, PathBuf>,
    /// A form-fill environment per open document that has a form, and the
    /// handle that borrows it.
    ///
    /// Needed for *drawing*, not for editing. PDFium's page render draws a
    /// page's content; a form field's value is drawn by `FPDF_FFLDraw` out of
    /// the form-fill environment, and a renderer without one produces a page
    /// whose fields are blank — the boxes and the printed labels, and nothing
    /// in them. That is true of a form nobody has touched: the values were
    /// already in the file.
    ///
    /// Empty for every slide deck, which is most of what this backend opens.
    forms: HashMap<u64, FormBinding>,
    next_id: u64,
    library_path: Option<PathBuf>,
    /// Each page's text layer, extracted once and kept for the next query.
    ///
    /// The presenter searches through this backend, and a search is not one
    /// question but a stream of them: every keystroke rescans the deck, and
    /// the expensive half of scanning a page is building its text layer
    /// rather than looking through it. See [`crate::pdf::search`].
    ///
    /// Interior mutability because searching is a read, and a read must not
    /// need the exclusive borrow that renders take.
    page_text: std::cell::RefCell<crate::pdf::search::PageTextCache<(u64, usize)>>,
}

/// A form-fill environment and the handle PDFium gave back for it.
///
/// The environment is boxed and must outlive the handle: PDFium keeps the
/// address it was given and calls back through it.
struct FormBinding {
    #[allow(
        dead_code,
        reason = "PDFium holds this box's address for the handle's lifetime; \
                  dropping it early is what this field prevents"
    )]
    environment: Box<crate::document::form::FormEnvironment>,
    handle: FPDF_FORMHANDLE,
}

// PDFium is not thread safe and `pdfium-render` serialises every call behind
// a global reentrant mutex. That is ergonomic safety, not parallelism: it is
// precisely why one backend lives in one worker *process*.
unsafe impl Send for PdfiumBackend {}

impl std::fmt::Debug for PdfiumBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfiumBackend")
            .field("library_path", &self.library_path)
            .field("open_documents", &self.documents.len())
            .finish()
    }
}

impl PdfiumBackend {
    /// Give up the form-fill environment for one document.
    ///
    /// Called by the document engine, which opens its document through this
    /// backend and then starts an environment of its own — one it can forward
    /// keystrokes to. Two environments on one `FPDF_DOCUMENT` would draw every
    /// field twice, once from each, and the second pass over the first is
    /// visible: the text comes out heavier than the page around it.
    ///
    /// Editing and drawing belong together, so whoever is editing keeps the
    /// environment and this one steps aside.
    pub fn release_form(&mut self, document: BackendDocumentId) {
        if let Some(form) = self.forms.remove(&document.0) {
            unsafe { self.bindings.FPDFDOC_ExitFormFillEnvironment(form.handle) };
        }
    }

    /// Start a form-fill environment for a document that has a form.
    ///
    /// Failure is not failure to open: the pages still render, and what is
    /// lost is the field values drawn over them. A slide deck gets nothing,
    /// because `FPDF_GetFormType` says it has no form and the environment is
    /// not free.
    fn open_form_environment(&mut self, id: u64, document: FPDF_DOCUMENT) {
        if self.form_type(document) == FormType::None {
            return;
        }
        let mut environment = crate::document::form::FormEnvironment::new();
        // Safety: the environment is boxed and stored beside the handle that
        // borrows it; `close` exits the environment before the box is dropped,
        // and nothing else removes either.
        let attached = unsafe { environment.attach(self.bindings.as_ref(), document) };
        match attached {
            Some(handle) => {
                self.forms.insert(
                    id,
                    FormBinding {
                        environment,
                        handle,
                    },
                );
            }
            None => tracing::warn!(
                "this document's widget annotations cannot be drawn; its pages \
                 will render with empty fields and without the visible \
                 appearance of any signature"
            ),
        }
    }

    /// Bind to a `libpdfium` shared library.
    ///
    /// Search order: `PULPIT_PDFIUM_PATH`, the directory next to the
    /// executable, the installed `../lib/pulpit` beside it, `./lib`, then the
    /// system loader path.
    pub fn bind() -> Result<Self> {
        // PDFium is a process-global, non-reentrant library: one instance per
        // process, initialised once. This is the invariant that makes the
        // worker *process* boundary mandatory rather than stylistic.
        if BOUND.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(PdfError::Unavailable(
                "PDFium is already bound in this process; use one worker process per backend"
                    .into(),
            ));
        }
        let mut attempts: Vec<PathBuf> = Vec::new();
        if let Ok(path) = std::env::var("PULPIT_PDFIUM_PATH") {
            attempts.push(PathBuf::from(path));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                attempts.push(dir.to_path_buf());
                attempts.push(dir.join("lib"));
                // A native package installs the binary as `<prefix>/bin/pulpit`
                // and the pinned library as `<prefix>/lib/pulpit/libpdfium.so`
                // (SPEC-package.md §1). Deriving that from the executable is
                // what lets a `.deb` or `.rpm` ship no wrapper and set no
                // environment variable.
                if let Some(prefix) = dir.parent() {
                    attempts.push(prefix.join("lib/pulpit"));
                    attempts.push(prefix.join("lib64/pulpit"));
                }
            }
        }
        attempts.push(PathBuf::from("./lib"));
        attempts.push(PathBuf::from("."));

        let mut errors = Vec::new();
        for directory in &attempts {
            let candidate = if directory.is_file() {
                directory.clone()
            } else {
                Pdfium::pdfium_platform_library_name_at_path(directory)
            };
            match Pdfium::bind_to_library(&candidate) {
                Ok(bindings) => {
                    tracing::info!(path = %candidate.display(), "bound to PDFium");
                    // The library must be initialised exactly once per
                    // process before any other call; skipping this is an
                    // immediate segfault rather than an error return.
                    unsafe { bindings.FPDF_InitLibrary() };
                    return Ok(Self {
                        bindings,
                        documents: HashMap::new(),
                        forms: HashMap::new(),
                        paths: HashMap::new(),
                        next_id: 0,
                        library_path: Some(candidate),
                        page_text: Default::default(),
                    });
                }
                Err(e) => errors.push(format!("{}: {e}", candidate.display())),
            }
        }
        match Pdfium::bind_to_system_library() {
            Ok(bindings) => {
                unsafe { bindings.FPDF_InitLibrary() };
                Ok(Self {
                    bindings,
                    documents: HashMap::new(),
                    forms: HashMap::new(),
                    paths: HashMap::new(),
                    next_id: 0,
                    library_path: None,
                    page_text: Default::default(),
                })
            }
            Err(e) => {
                errors.push(format!("system library: {e}"));
                BOUND.store(false, std::sync::atomic::Ordering::SeqCst);
                Err(PdfError::Unavailable(errors.join("; ")))
            }
        }
    }

    pub fn library_path(&self) -> Option<&Path> {
        self.library_path.as_deref()
    }

    /// The raw bindings, for the document engine.
    ///
    /// `crate::document::pdfium` is the other half of this backend rather than
    /// a second one: PDFium is bound once per process (see [`BOUND`]), so the
    /// document engine borrows this binding instead of opening its own, which
    /// is also what §5.1 means by document mutation being a capability of the
    /// existing worker rather than a new helper.
    pub(crate) fn bindings(&self) -> &dyn PdfiumLibraryBindings {
        self.bindings.as_ref()
    }

    /// The open document behind an id, for the document engine.
    pub(crate) fn document_handle(&self, document: BackendDocumentId) -> Result<FPDF_DOCUMENT> {
        self.handle(document)
    }

    /// Run `f` with a loaded page, closing it afterwards however `f` ended.
    pub(crate) fn on_page<T>(
        &self,
        document: BackendDocumentId,
        page: usize,
        f: impl FnOnce(FPDF_PAGE) -> Result<T>,
    ) -> Result<T> {
        self.with_page(document, page, f)
    }

    /// Every hit for one prepared query on one page of one document.
    ///
    /// A page whose text is known and does not match costs no PDFium call at
    /// all — which, on the second query over a deck, is nearly every page. One
    /// that does match, or has never been read, is opened once and gives up
    /// its geometry, its text and its rectangles in that one visit.
    fn find_on_one_page(
        &self,
        document: BackendDocumentId,
        page: usize,
        query: &pulpit_core::search::PreparedQuery<'_>,
    ) -> Result<Vec<pulpit_core::search::Hit>> {
        let key = (document.0, page);
        if let Some(text) = self.page_text.borrow().get(&key) {
            let found = text.matches(query, MAX_HITS_PER_SEARCH);
            if found.is_empty() {
                return Ok(Vec::new());
            }
            return self.with_page(document, page, |handle| {
                let text_page = unsafe { self.bindings.FPDFText_LoadPage(handle) };
                if text_page.is_null() {
                    return Ok(Vec::new());
                }
                let geometry = crate::pdf::search::geometry_of(self.bindings.as_ref(), handle);
                let hits = crate::pdf::search::hits_from_pdfium_matches(
                    pulpit_core::page::PageIndex(page),
                    &text,
                    &found,
                    |start, length| {
                        crate::pdf::search::quads_of(
                            self.bindings.as_ref(),
                            text_page,
                            &geometry,
                            start,
                            length,
                            MAX_QUADS_PER_HIT,
                        )
                    },
                );
                unsafe { self.bindings.FPDFText_ClosePage(text_page) };
                Ok(hits)
            });
        }

        let (hits, text) = self.with_page(document, page, |handle| {
            let text_page = unsafe { self.bindings.FPDFText_LoadPage(handle) };
            if text_page.is_null() {
                // A slide with no text layer — a picture, a poster — is not a
                // failure. It simply has nothing to find, and remembering that
                // is worth as much as remembering what a page does say.
                return Ok((Vec::new(), crate::pdf::search::PageText::default()));
            }
            let geometry = crate::pdf::search::geometry_of(self.bindings.as_ref(), handle);
            let text = crate::pdf::search::PageText::extract(self.bindings.as_ref(), text_page);
            let found = text.matches(query, MAX_HITS_PER_SEARCH);
            let hits = crate::pdf::search::hits_from_pdfium_matches(
                pulpit_core::page::PageIndex(page),
                &text,
                &found,
                |start, length| {
                    crate::pdf::search::quads_of(
                        self.bindings.as_ref(),
                        text_page,
                        &geometry,
                        start,
                        length,
                        MAX_QUADS_PER_HIT,
                    )
                },
            );
            unsafe { self.bindings.FPDFText_ClosePage(text_page) };
            Ok((hits, text))
        })?;
        self.page_text.borrow_mut().insert(key, text);
        Ok(hits)
    }

    fn handle(&self, document: BackendDocumentId) -> Result<FPDF_DOCUMENT> {
        self.documents
            .get(&document.0)
            .copied()
            .ok_or_else(|| PdfError::Render(format!("unknown document {}", document.0)))
    }

    fn with_page<T>(
        &self,
        document: BackendDocumentId,
        page: usize,
        f: impl FnOnce(FPDF_PAGE) -> Result<T>,
    ) -> Result<T> {
        let handle = self.handle(document)?;
        let count = unsafe { self.bindings.FPDF_GetPageCount(handle) } as usize;
        if page >= count {
            return Err(PdfError::PageOutOfRange { page, count });
        }
        let page_handle = unsafe { self.bindings.FPDF_LoadPage(handle, page as i32) };
        if page_handle.is_null() {
            return Err(PdfError::Render(format!("cannot load page {page}")));
        }
        let result = f(page_handle);
        unsafe { self.bindings.FPDF_ClosePage(page_handle) };
        result
    }

    /// Serialise the document into memory.
    ///
    /// Buffering the whole file before a byte of it reaches the destination
    /// is what makes [`write_atomically`] able to promise the presenter that
    /// a save either produced a complete PDF or produced nothing.
    pub(crate) fn save_to_memory(
        &self,
        document: FPDF_DOCUMENT,
        incremental: bool,
    ) -> Result<Vec<u8>> {
        /// `FPDF_INCREMENTAL` from `fpdf_save.h`: append the changed objects
        /// to the original byte stream rather than re-serialising the whole
        /// document. Cheaper to produce, and it leaves an existing
        /// signature's byte ranges intact.
        const FPDF_INCREMENTAL: std::os::raw::c_ulong = 1;
        // The C struct PDFium writes through; `writer` is the Rust tail this
        // module casts back to in the callback. `#[repr(C)]` and the header
        // coming first are what make that cast sound.
        #[repr(C)]
        struct Sink {
            header: pdfium_render::prelude::FPDF_FILEWRITE,
            bytes: Vec<u8>,
        }

        unsafe extern "C" fn write_block(
            this: *mut pdfium_render::prelude::FPDF_FILEWRITE,
            data: *const c_void,
            size: std::os::raw::c_ulong,
        ) -> std::os::raw::c_int {
            if this.is_null() || data.is_null() {
                return 0;
            }
            let sink = unsafe { &mut *(this as *mut Sink) };
            let block = unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) };
            sink.bytes.extend_from_slice(block);
            1
        }

        let mut sink = Sink {
            header: pdfium_render::prelude::FPDF_FILEWRITE {
                version: 1,
                WriteBlock: Some(write_block),
            },
            bytes: Vec::new(),
        };
        let ok = unsafe {
            self.bindings.FPDF_SaveAsCopy(
                document,
                &mut sink.header as *mut pdfium_render::prelude::FPDF_FILEWRITE,
                if incremental { FPDF_INCREMENTAL } else { 0 },
            )
        };
        if ok == 0 || sink.bytes.is_empty() {
            return Err(PdfError::Render("PDFium refused to write the copy".into()));
        }
        Ok(sink.bytes)
    }

    fn metadata_text(&self, handle: FPDF_DOCUMENT) -> String {
        let mut collected = Vec::new();
        for tag in METADATA_TAGS {
            let length = unsafe {
                self.bindings
                    .FPDF_GetMetaText(handle, tag, std::ptr::null_mut(), 0)
            };
            if length <= 2 {
                continue;
            }
            let mut buffer = vec![0u8; length as usize];
            unsafe {
                self.bindings.FPDF_GetMetaText(
                    handle,
                    tag,
                    buffer.as_mut_ptr() as *mut c_void,
                    length,
                );
            }
            // PDFium returns UTF-16LE including a trailing NUL.
            let utf16: Vec<u16> = buffer
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .take_while(|unit| *unit != 0)
                .collect();
            if let Ok(text) = String::from_utf16(&utf16) {
                if !text.trim().is_empty() {
                    collected.push(text);
                }
            }
        }
        collected.join(" ")
    }

    /// The name-tree key of one attachment, read with the same
    /// length-then-buffer dance as every other PDFium string.
    fn attachment_name(&self, attachment: FPDF_ATTACHMENT) -> Option<String> {
        let length = unsafe {
            self.bindings
                .FPDFAttachment_GetName(attachment, std::ptr::null_mut(), 0)
        };
        if length as u64 <= 2 || length as u64 > MAX_ATTACHMENT_NAME_BYTES {
            return None;
        }
        // The buffer is counted in bytes but written as UTF-16 code units.
        let mut units = vec![0u16; (length as usize).div_ceil(2)];
        unsafe {
            self.bindings
                .FPDFAttachment_GetName(attachment, units.as_mut_ptr(), length);
        }
        let text: Vec<u16> = units.into_iter().take_while(|unit| *unit != 0).collect();
        String::from_utf16(&text).ok().filter(|s| !s.is_empty())
    }

    fn form_type(&self, handle: FPDF_DOCUMENT) -> FormType {
        match unsafe { self.bindings.FPDF_GetFormType(handle) } {
            FPDF_FORMTYPE_NONE => FormType::None,
            FPDF_FORMTYPE_XFA_FULL | FPDF_FORMTYPE_XFA_FOREGROUND => FormType::Xfa,
            _ => FormType::AcroForm,
        }
    }

    /// Names of the document-level scripts, which are reported and never run.
    fn javascript_names(&self, handle: FPDF_DOCUMENT) -> Vec<String> {
        let count = unsafe { self.bindings.FPDFDoc_GetJavaScriptActionCount(handle) };
        let mut names = Vec::new();
        for index in 0..count.min(MAX_JAVASCRIPT_ACTIONS) {
            let action = unsafe { self.bindings.FPDFDoc_GetJavaScriptAction(handle, index) };
            if action.is_null() {
                continue;
            }
            let length = unsafe {
                self.bindings
                    .FPDFJavaScriptAction_GetName(action, std::ptr::null_mut(), 0)
            };
            if length as u64 > 2 && length as u64 <= MAX_JAVASCRIPT_NAME_BYTES {
                let mut units = vec![0u16; (length as usize).div_ceil(2)];
                unsafe {
                    self.bindings
                        .FPDFJavaScriptAction_GetName(action, units.as_mut_ptr(), length);
                }
                let text: Vec<u16> = units.into_iter().take_while(|unit| *unit != 0).collect();
                if let Ok(name) = String::from_utf16(&text) {
                    if !name.trim().is_empty() {
                        names.push(name.trim().to_string());
                    }
                }
            }
            unsafe { self.bindings.FPDFDoc_CloseJavaScriptAction(action) };
        }
        // A script the name of which could not be read still exists, and the
        // count is what the finding is really about.
        while names.len() < count.clamp(0, MAX_JAVASCRIPT_ACTIONS) as usize {
            names.push("unnamed script".to_string());
        }
        names
    }

    /// What every page's annotations declare. A page that will not load is
    /// skipped: the scan is advisory, and failing it would fail the open.
    fn page_evidence(
        &self,
        document: BackendDocumentId,
        handle: FPDF_DOCUMENT,
    ) -> Vec<PageEvidence> {
        let count = unsafe { self.bindings.FPDF_GetPageCount(handle) }.max(0) as usize;
        let mut pages = Vec::new();
        for page in 0..count.min(MAX_INSPECTED_PAGES) {
            let annotations = self
                .with_page(document, page, |page_handle| {
                    Ok(self.annotation_evidence(handle, page_handle))
                })
                .unwrap_or_default();
            if !annotations.is_empty() {
                pages.push(PageEvidence {
                    page,
                    annotations,
                    // PDFium exposes no `/Trans` accessor; transitions are
                    // reported document-wide instead, from the file bytes.
                    transition: None,
                });
            }
        }
        pages
    }

    fn annotation_evidence(
        &self,
        document: FPDF_DOCUMENT,
        page: FPDF_PAGE,
    ) -> Vec<AnnotationEvidence> {
        let count = unsafe { self.bindings.FPDFPage_GetAnnotCount(page) };
        let mut collected = Vec::new();
        for index in 0..count.min(MAX_ANNOTATIONS_PER_PAGE) {
            let annotation = unsafe { self.bindings.FPDFPage_GetAnnot(page, index) };
            if annotation.is_null() {
                continue;
            }
            let subtype = match unsafe { self.bindings.FPDFAnnot_GetSubtype(annotation) } as u32 {
                FPDF_ANNOT_LINK => AnnotationSubtype::Link,
                FPDF_ANNOT_WIDGET => AnnotationSubtype::Widget,
                FPDF_ANNOT_SCREEN => AnnotationSubtype::Screen,
                FPDF_ANNOT_MOVIE => AnnotationSubtype::Movie,
                FPDF_ANNOT_SOUND => AnnotationSubtype::Sound,
                FPDF_ANNOT_THREED => AnnotationSubtype::ThreeD,
                FPDF_ANNOT_RICHMEDIA => AnnotationSubtype::RichMedia,
                _ => AnnotationSubtype::Other,
            };
            // `/AA` is where an annotation's scripted actions live.
            let has_additional_actions =
                unsafe { self.bindings.FPDFAnnot_HasKey(annotation, "AA") } != 0;
            let has_action = unsafe { self.bindings.FPDFAnnot_HasKey(annotation, "A") } != 0;
            let link = unsafe { self.bindings.FPDFAnnot_GetLink(annotation) };
            let (action, uri) = if link.is_null() {
                (ActionKind::None, None)
            } else {
                self.link_action(document, link)
            };
            unsafe { self.bindings.FPDFPage_CloseAnnot(annotation) };
            collected.push(AnnotationEvidence {
                subtype,
                action,
                uri,
                has_action,
                has_additional_actions,
            });
        }
        collected
    }

    /// The action a link carries, classified for the capability report. This
    /// deliberately duplicates none of [`link_target`]'s policy: that function
    /// decides what to *do*, this one only says what was found.
    fn link_action(
        &self,
        document: FPDF_DOCUMENT,
        link: FPDF_LINK,
    ) -> (ActionKind, Option<String>) {
        let bindings = self.bindings.as_ref();
        let dest = unsafe { bindings.FPDFLink_GetDest(document, link) };
        if !dest.is_null() {
            return (ActionKind::GoTo, None);
        }
        let action = unsafe { bindings.FPDFLink_GetAction(link) };
        if action.is_null() {
            return (ActionKind::None, None);
        }
        match unsafe { bindings.FPDFAction_GetType(action) } {
            PDFACTION_GOTO => (ActionKind::GoTo, None),
            PDFACTION_URI => {
                let uri = match link_target(bindings, document, link) {
                    Some(LinkTarget::Uri(uri)) => Some(uri),
                    _ => None,
                };
                (ActionKind::Uri, uri)
            }
            PDFACTION_LAUNCH => (ActionKind::Launch, None),
            PDFACTION_REMOTEGOTO => (ActionKind::RemoteGoTo, None),
            PDFACTION_EMBEDDEDGOTO => (ActionKind::EmbeddedGoTo, None),
            PDFACTION_UNSUPPORTED => (ActionKind::Unrecognised, None),
            _ => (ActionKind::Unrecognised, None),
        }
    }

    /// Transition styles found in the document's own bytes, since PDFium has
    /// no accessor for `/Trans`. Only the head of the file is read: a deck
    /// declares its transitions on its pages, and a gigabyte of embedded
    /// video is not worth scanning for a diagnostic.
    fn transition_styles(&self, document: BackendDocumentId) -> Vec<String> {
        /// Enough for the page tree and page dictionaries of any real deck.
        const MAX_SCANNED_BYTES: usize = 8 << 20;
        let Some(path) = self.paths.get(&document.0) else {
            return Vec::new();
        };
        // Only the head of the file is read — actually read only the head:
        // `fs::read` loaded the whole PDF (a deck with embedded video can be
        // hundreds of megabytes) to scan its first few.
        use std::io::Read;
        let Ok(file) = std::fs::File::open(path) else {
            return Vec::new();
        };
        let mut bytes = Vec::with_capacity(MAX_SCANNED_BYTES.min(1 << 20));
        if file
            .take(MAX_SCANNED_BYTES as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return Vec::new();
        }
        scan_transition_styles(&bytes)
    }
}

/// Write `bytes` to `destination` through a temporary file in the same
/// directory, so an interrupted save leaves the presenter's chosen path
/// either untouched or holding a complete PDF — never half of one, and never
/// a truncated overwrite of a file they already had.
///
/// The visibility is [`Inherited`]: the presenter picked this path, and an
/// export they cannot hand to anybody is not what they asked for. Their umask
/// decides, as it would for any other file they created there.
///
/// [`Inherited`]: crate::atomic::Visibility::Inherited
pub(crate) fn write_atomically(destination: &Path, bytes: &[u8]) -> Result<()> {
    crate::atomic::replace(
        destination,
        "export",
        crate::atomic::Visibility::Inherited,
        bytes,
    )
    .map_err(|e| PdfError::Render(format!("cannot save {e}")))
}

struct Bookmarks<'a> {
    bindings: &'a dyn PdfiumLibraryBindings,
    document: FPDF_DOCUMENT,
}

impl OutlineSource for Bookmarks<'_> {
    type Node = usize;

    fn first_child(&self, node: Option<usize>) -> Option<usize> {
        let parent = node.map_or(std::ptr::null_mut(), |node| node as FPDF_BOOKMARK);
        let child = unsafe {
            self.bindings
                .FPDFBookmark_GetFirstChild(self.document, parent)
        };
        (!child.is_null()).then_some(child as usize)
    }

    fn next_sibling(&self, node: usize) -> Option<usize> {
        let sibling = unsafe {
            self.bindings
                .FPDFBookmark_GetNextSibling(self.document, node as FPDF_BOOKMARK)
        };
        (!sibling.is_null()).then_some(sibling as usize)
    }

    fn title(&self, node: usize) -> Option<String> {
        let bookmark = node as FPDF_BOOKMARK;
        let length = unsafe {
            self.bindings
                .FPDFBookmark_GetTitle(bookmark, std::ptr::null_mut(), 0)
        };
        // Two bytes is the terminating NUL alone: an untitled bookmark.
        if length as u64 <= 2 || length as u64 > MAX_BOOKMARK_TITLE_BYTES {
            return None;
        }
        let mut buffer = vec![0u8; length as usize];
        unsafe {
            self.bindings.FPDFBookmark_GetTitle(
                bookmark,
                buffer.as_mut_ptr() as *mut c_void,
                length,
            );
        }
        utf16le_text(&buffer)
    }

    fn target(&self, node: usize) -> Option<LinkTarget> {
        let bookmark = node as FPDF_BOOKMARK;
        let mut dest = unsafe { self.bindings.FPDFBookmark_GetDest(self.document, bookmark) };
        if dest.is_null() {
            let action = unsafe { self.bindings.FPDFBookmark_GetAction(bookmark) };
            if action.is_null() {
                return None;
            }
            // Only a destination is followed. A bookmark that wants to launch
            // a file or run a script is not a section of this talk.
            if unsafe { self.bindings.FPDFAction_GetType(action) } != PDFACTION_GOTO {
                return None;
            }
            dest = unsafe { self.bindings.FPDFAction_GetDest(self.document, action) };
        }
        if dest.is_null() {
            return None;
        }
        let index = unsafe { self.bindings.FPDFDest_GetDestPageIndex(self.document, dest) };
        (index >= 0).then_some(LinkTarget::Page {
            page: index as usize,
            zoom: None,
        })
    }
}

/// Decode a PDFium UTF-16LE byte buffer, stopping at the terminating NUL.
fn utf16le_text(buffer: &[u8]) -> Option<String> {
    let units: Vec<u16> = buffer
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16(&units)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

impl Drop for PdfiumBackend {
    fn drop(&mut self) {
        for handle in self.documents.values() {
            unsafe { self.bindings.FPDF_CloseDocument(*handle) };
        }
    }
}

impl PdfBackend for PdfiumBackend {
    fn name(&self) -> &'static str {
        "pdfium"
    }

    /// The library actually loaded. Two machines rendering the same deck
    /// differently is nearly always two different libpdfium builds, so the
    /// path is the useful half of the answer.
    fn version(&self) -> String {
        match &self.library_path {
            Some(path) => path.display().to_string(),
            None => "system library".to_string(),
        }
    }
    fn open(&mut self, source: &Path) -> Result<BackendDocumentId> {
        let path = source.to_string_lossy().to_string();
        let handle = unsafe { self.bindings.FPDF_LoadDocument(&path, None) };
        if handle.is_null() {
            let code = unsafe { self.bindings.FPDF_GetLastError() };
            return Err(PdfError::Open {
                path,
                reason: format!("PDFium error {code}"),
            });
        }
        // A document that reports no pages is not a usable presentation and
        // must be rejected before it can be promoted over a working one.
        if unsafe { self.bindings.FPDF_GetPageCount(handle) } <= 0 {
            unsafe { self.bindings.FPDF_CloseDocument(handle) };
            return Err(PdfError::Open {
                path,
                reason: "document has no pages".into(),
            });
        }
        self.next_id += 1;
        self.documents.insert(self.next_id, handle);
        self.paths.insert(self.next_id, source.to_path_buf());
        self.open_form_environment(self.next_id, handle);
        Ok(BackendDocumentId(self.next_id))
    }

    fn close(&mut self, document: BackendDocumentId) {
        self.paths.remove(&document.0);
        // The text of a document nobody has open is not worth the memory, and
        // an id is reused only after this backend has forgotten the last one.
        self.page_text
            .borrow_mut()
            .retain(|(open, _)| *open != document.0);
        // Before the document, and in this order: PDFium's environment holds
        // the document, and closing the document first leaves it holding a
        // dangling one.
        if let Some(form) = self.forms.remove(&document.0) {
            unsafe { self.bindings.FPDFDOC_ExitFormFillEnvironment(form.handle) };
        }
        if let Some(handle) = self.documents.remove(&document.0) {
            unsafe { self.bindings.FPDF_CloseDocument(handle) };
        }
    }

    fn metadata(&self, document: BackendDocumentId) -> Result<DocumentMetadata> {
        let handle = self.handle(document)?;
        let page_count = unsafe { self.bindings.FPDF_GetPageCount(handle) } as usize;
        let first_page_size = self.page_size(document, 0)?;
        let (page_sizes, page_sizes_sampled) =
            crate::pdf::collect_page_sizes(self, document, page_count);
        Ok(DocumentMetadata {
            page_count,
            first_page_size,
            page_sizes,
            page_sizes_sampled,
            metadata_text: self.metadata_text(handle),
            source_digest: None,
        })
    }

    fn page_size(&self, document: BackendDocumentId, page: usize) -> Result<PageSize> {
        // Read from the page tree without loading the page: `FPDF_LoadPage`
        // parses the page's dictionary and content-stream references, and
        // measuring a whole deck that way put hundreds of page parses on the
        // critical path of opening a document.
        let handle = self.handle(document)?;
        let mut size = pdfium_render::prelude::FS_SIZEF {
            width: 0.0,
            height: 0.0,
        };
        let ok = unsafe {
            self.bindings
                .FPDF_GetPageSizeByIndexF(handle, page as i32, &mut size)
        };
        if ok == 0 {
            return Err(PdfError::PageOutOfRange { page, count: 0 });
        }
        Ok(PageSize {
            width: size.width,
            height: size.height,
        })
    }

    fn render(&self, request: &RenderRequest, cancel: &dyn CancelSignal) -> Result<RenderedPage> {
        let width = request.width;
        let height = request.height;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        self.render_into(request, &mut pixels, cancel)?;
        Ok(RenderedPage {
            width,
            height,
            pixels,
        })
    }

    /// PDFium draws straight into `target` — when that is the worker's
    /// shared-memory mapping, the frame never exists anywhere else, and the
    /// per-frame allocation and copy the default path pays are gone. No
    /// byte-order pass either: `FPDF_REVERSE_BYTE_ORDER` makes PDFium emit
    /// RGBA directly.
    fn render_into(
        &self,
        request: &RenderRequest,
        target: &mut [u8],
        cancel: &dyn CancelSignal,
    ) -> Result<()> {
        request.validate()?;
        let bytes = request.width as usize * request.height as usize * 4;
        if target.len() < bytes {
            return Err(PdfError::Invalid(format!(
                "a {} byte target cannot hold a {}x{} frame",
                target.len(),
                request.width,
                request.height
            )));
        }
        // The form handle for this document, if it has one. Looked up before
        // the page is borrowed so the closure below needs nothing from `self`.
        let form = request
            .with_annotations
            .then(|| self.forms.get(&request.document.0).map(|form| form.handle))
            .flatten();
        let bindings = self.bindings.as_ref();
        self.with_page(request.document, request.page, |page| {
            render_page_progressively(
                bindings,
                page,
                &mut target[..bytes],
                request.width,
                request.height,
                request.region,
                request.full_size,
                request.with_annotations,
                cancel,
            )?;
            // …and then the field values over the top of it.
            //
            // Only when the page's own annotations were asked for: a
            // presentation deliberately renders without them (§ the
            // `with_annotations` note above), and a projector showing a form's
            // filled-in values would be the same mistake as showing somebody's
            // review notes.
            if let Some(form) = form {
                composite_form_fields(
                    bindings,
                    form,
                    page,
                    &mut target[..bytes],
                    request.width,
                    request.height,
                    request.region,
                    request.full_size,
                );
            }
            Ok(())
        })
    }

    fn attachment_names(&self, document: BackendDocumentId) -> Result<Vec<String>> {
        let handle = self.handle(document)?;
        let count = unsafe { self.bindings.FPDFDoc_GetAttachmentCount(handle) };
        let mut names = Vec::new();
        for index in 0..count.min(MAX_ATTACHMENTS) {
            let attachment = unsafe { self.bindings.FPDFDoc_GetAttachment(handle, index) };
            if attachment.is_null() {
                continue;
            }
            if let Some(name) = self.attachment_name(attachment) {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn attachment(&self, document: BackendDocumentId, name: &str) -> Result<Vec<u8>> {
        let handle = self.handle(document)?;
        let count = unsafe { self.bindings.FPDFDoc_GetAttachmentCount(handle) };
        for index in 0..count.min(MAX_ATTACHMENTS) {
            let attachment = unsafe { self.bindings.FPDFDoc_GetAttachment(handle, index) };
            if attachment.is_null() {
                continue;
            }
            // Name-tree keys are compared exactly: a case-insensitive or
            // trimmed match would let one attachment stand in for another.
            if self.attachment_name(attachment).as_deref() != Some(name) {
                continue;
            }
            // Length first, so an implausible attachment is refused before a
            // single byte is allocated for it.
            let mut needed: std::ffi::c_ulong = 0;
            let ok = unsafe {
                self.bindings.FPDFAttachment_GetFile(
                    attachment,
                    std::ptr::null_mut(),
                    0,
                    &mut needed,
                )
            };
            if ok == 0 || needed == 0 {
                return Err(PdfError::Invalid(format!(
                    "attachment `{name}` has no file stream"
                )));
            }
            if needed as u64 > MAX_ATTACHMENT_FILE_BYTES {
                return Err(PdfError::Invalid(format!(
                    "attachment `{name}` of {needed} bytes exceeds the \
                     {MAX_ATTACHMENT_FILE_BYTES} byte limit"
                )));
            }
            let mut bytes = vec![0u8; needed as usize];
            let mut written: std::ffi::c_ulong = 0;
            let ok = unsafe {
                self.bindings.FPDFAttachment_GetFile(
                    attachment,
                    bytes.as_mut_ptr() as *mut c_void,
                    needed,
                    &mut written,
                )
            };
            if ok == 0 {
                return Err(PdfError::Invalid(format!(
                    "attachment `{name}` could not be read"
                )));
            }
            bytes.truncate(written.min(needed) as usize);
            return Ok(bytes);
        }
        Err(PdfError::Invalid(format!(
            "the document has no attachment named `{name}`"
        )))
    }

    fn page_labels(&self, document: BackendDocumentId) -> Result<pulpit_core::overlay::PageLabels> {
        let handle = self.handle(document)?;
        let count = unsafe { self.bindings.FPDF_GetPageCount(handle) };
        let mut labels = pulpit_core::overlay::PageLabels::default();
        for page in 0..count.max(0) {
            let length = unsafe {
                self.bindings
                    .FPDF_GetPageLabel(handle, page, std::ptr::null_mut(), 0)
            };
            // Two bytes is the terminating NUL alone: no label.
            if length as u64 <= 2 || length as u64 > MAX_PAGE_LABEL_BYTES {
                continue;
            }
            let mut buffer = vec![0u8; length as usize];
            unsafe {
                self.bindings.FPDF_GetPageLabel(
                    handle,
                    page,
                    buffer.as_mut_ptr() as *mut c_void,
                    length,
                );
            }
            if let Some(text) = utf16le_text(&buffer) {
                labels.labels.insert(page as usize, text);
            }
        }
        Ok(labels)
    }

    fn outline(&self, document: BackendDocumentId) -> Result<Outline> {
        let handle = self.handle(document)?;
        Ok(pulpit_core::navigation::build_outline(&Bookmarks {
            bindings: self.bindings.as_ref(),
            document: handle,
        }))
    }

    fn evidence(&self, document: BackendDocumentId) -> Result<DocumentEvidence> {
        let handle = self.handle(document)?;
        Ok(DocumentEvidence {
            form_type: self.form_type(handle),
            document_javascript: self.javascript_names(handle),
            restriction: Some(RestrictionEvidence {
                security_revision: unsafe { self.bindings.FPDF_GetSecurityHandlerRevision(handle) },
                // `unsigned long` in the PDFium header: 32 bits on Windows,
                // 64 on the Unixes, so the cast is redundant on exactly one
                // target and required on the others. The permissions field is
                // 32 bits wide in the PDF format itself, so nothing is lost.
                #[allow(clippy::unnecessary_cast)]
                permissions: unsafe { self.bindings.FPDF_GetDocPermissions(handle) } as u32,
            }),
            pages: self.page_evidence(document, handle),
            transition_styles: self.transition_styles(document),
        })
    }

    fn find_text(
        &self,
        document: BackendDocumentId,
        query: &pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) -> Result<Vec<pulpit_core::search::Hit>> {
        let mut hits = Vec::new();
        if query.is_empty() {
            return Ok(hits);
        }
        let prepared = query.prepare();
        for page in pages {
            let found = self.find_on_one_page(document, page, &prepared)?;
            hits.extend(found);
            if hits.len() >= MAX_HITS_PER_SEARCH {
                hits.truncate(MAX_HITS_PER_SEARCH);
                break;
            }
        }
        Ok(hits)
    }

    fn links(&self, document: BackendDocumentId, page: usize) -> Result<Vec<PageLink>> {
        let handle = self.handle(document)?;
        self.with_page(document, page, |page_handle| {
            let page_width = unsafe { self.bindings.FPDF_GetPageWidthF(page_handle) };
            let page_height = unsafe { self.bindings.FPDF_GetPageHeightF(page_handle) };
            if page_width <= 0.0 || page_height <= 0.0 {
                return Ok(Vec::new());
            }

            let mut links = Vec::new();
            let mut position: i32 = 0;
            let mut link: FPDF_LINK = std::ptr::null_mut();
            while links.len() < MAX_LINKS_PER_PAGE
                && unsafe {
                    self.bindings
                        .FPDFLink_Enumerate(page_handle, &mut position, &mut link)
                } != 0
            {
                if link.is_null() {
                    continue;
                }
                let Some(rect) =
                    annotation_rect(self.bindings.as_ref(), link, page_width, page_height)
                else {
                    continue;
                };
                if let Some(target) = link_target(self.bindings.as_ref(), handle, link) {
                    links.push(PageLink { rect, target });
                }
            }
            Ok(links)
        })
    }
}

/// The annotation rectangle, converted from PDF points (origin bottom-left,
/// y up) to the normalised top-left-origin convention of [`Region`].
fn annotation_rect(
    bindings: &dyn PdfiumLibraryBindings,
    link: FPDF_LINK,
    page_width: f32,
    page_height: f32,
) -> Option<Region> {
    let mut rect = FS_RECTF {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };
    if unsafe { bindings.FPDFLink_GetAnnotRect(link, &mut rect) } == 0 {
        return None;
    }
    let left = rect.left.min(rect.right);
    let right = rect.left.max(rect.right);
    let bottom = rect.bottom.min(rect.top);
    let top = rect.bottom.max(rect.top);

    let x = (left / page_width).clamp(0.0, 1.0);
    let y = ((page_height - top) / page_height).clamp(0.0, 1.0);
    let width = ((right - left) / page_width).clamp(0.0, 1.0 - x);
    let height = ((top - bottom) / page_height).clamp(0.0, 1.0 - y);
    (width > 0.0 && height > 0.0).then(|| Region::new(x, y, width, height))
}

/// Resolve where a link annotation points: an in-document destination or a
/// The file a launch action names, as UTF-8.
///
/// Reading it is not running it: the caller turns this into a `run:` URI that
/// only the overlay parser consumes, and only for a media extension inside
/// the document directory.
fn launch_file_path(
    bindings: &dyn PdfiumLibraryBindings,
    action: pdfium_render::prelude::FPDF_ACTION,
) -> Option<String> {
    let length = unsafe { bindings.FPDFAction_GetFilePath(action, std::ptr::null_mut(), 0) };
    if length as u64 == 0 || length as u64 > MAX_URI_BYTES {
        return None;
    }
    let mut buffer = vec![0u8; length as usize];
    unsafe {
        bindings.FPDFAction_GetFilePath(action, buffer.as_mut_ptr() as *mut c_void, length);
    }
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    let path = String::from_utf8(buffer).ok()?;
    let path = pdf_separators(path.trim(), HOST_CONVENTION);
    (!path.is_empty()).then_some(path)
}

/// How PDFium spells a path on a given platform.
///
/// `FPDFAction_GetFilePath` does not hand back the path as the PDF stores it.
/// PDFium's `CPDF_FileSpec` rewrites the `/`-separated form the format
/// mandates into whatever the *host* calls a path — colons on Apple,
/// backslashes on Windows — so the same deck yields `media:clip.mp4` on
/// macOS, `media\clip.mp4` on Windows and `media/clip.mp4` on Linux.
///
/// This is not cosmetic: the overlay parser resolves the result against the
/// document directory, so on two of three platforms every `run:` overlay in
/// every deck names a file that does not exist, and media degrades to posters
/// with no error anyone would see.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PathConvention {
    /// `/`-separated, which is also the PDF's own form.
    Posix,
    /// Classic Mac: `:`-separated.
    Apple,
    /// `\`-separated.
    Windows,
}

const HOST_CONVENTION: PathConvention = if cfg!(target_os = "macos") {
    PathConvention::Apple
} else if cfg!(target_os = "windows") {
    PathConvention::Windows
} else {
    PathConvention::Posix
};

/// Undo that rewrite, putting the path back into the PDF's own spelling.
///
/// Decks name assets *relative* to the document — that is the only shape the
/// overlay parser accepts — and for a relative path the rewrite is pure
/// separator substitution, so reversing it is exact. An absolute path comes
/// back imperfectly (`/C//x` became `C:\x`), which costs nothing: the parser
/// refuses anything outside the document directory anyway.
fn pdf_separators(path: &str, convention: PathConvention) -> String {
    match convention {
        PathConvention::Posix => path.to_string(),
        PathConvention::Apple => path.replace(':', "/"),
        PathConvention::Windows => path.replace('\\', "/"),
    }
}

/// URI action, or a launch action *read as a media reference*. Remote-goto
/// and JavaScript actions are ignored, and no launch action is ever executed
/// — a presentation tool has no business running programs.
fn link_target(
    bindings: &dyn PdfiumLibraryBindings,
    document: FPDF_DOCUMENT,
    link: FPDF_LINK,
) -> Option<pulpit_core::LinkTarget> {
    use pulpit_core::LinkTarget;

    let mut dest = unsafe { bindings.FPDFLink_GetDest(document, link) };
    if dest.is_null() {
        let action = unsafe { bindings.FPDFLink_GetAction(link) };
        if action.is_null() {
            return None;
        }
        match unsafe { bindings.FPDFAction_GetType(action) } {
            PDFACTION_GOTO => {
                dest = unsafe { bindings.FPDFAction_GetDest(document, action) };
            }
            PDFACTION_URI => {
                let length = unsafe {
                    bindings.FPDFAction_GetURIPath(document, action, std::ptr::null_mut(), 0)
                };
                if length as u64 == 0 || length as u64 > MAX_URI_BYTES {
                    return None;
                }
                let mut buffer = vec![0u8; length as usize];
                unsafe {
                    bindings.FPDFAction_GetURIPath(
                        document,
                        action,
                        buffer.as_mut_ptr() as *mut c_void,
                        length,
                    );
                }
                // Trailing NUL included; the encoding is UTF-8 in practice.
                while buffer.last() == Some(&0) {
                    buffer.pop();
                }
                let uri = String::from_utf8(buffer).ok()?;
                let uri = uri.trim().to_string();
                return (!uri.is_empty()).then_some(LinkTarget::Uri(uri));
            }
            PDFACTION_LAUNCH => {
                // `\href{run:clip.mp4?autostart}` — the idiom beamer decks
                // already use for pdfpc — becomes a /Launch action, so this
                // is the only way to read the standard convention at all.
                //
                // The file is *never* executed. It is surfaced as a `run:`
                // URI, which the overlay parser accepts only for a media
                // extension inside the document directory, and which
                // `follow_link`'s scheme allowlist refuses to open. A launch
                // action naming a program is simply not an overlay.
                let path = launch_file_path(bindings, action)?;
                return (!path.is_empty()).then_some(LinkTarget::Uri(format!("run:{path}")));
            }
            _ => return None,
        }
    }
    if dest.is_null() {
        return None;
    }
    let index = unsafe { bindings.FPDFDest_GetDestPageIndex(document, dest) };
    if index < 0 {
        return None;
    }
    Some(LinkTarget::Page {
        page: index as usize,
        zoom: fit_rect_zoom(bindings, document, dest, index),
    })
}

/// The normalised `/FitR` rectangle of a destination, when it has one.
///
/// The rectangle is expressed in the *destination* page's point coordinates
/// (origin bottom-left), so that page's size — not the source page's — is
/// what normalises it into the top-left-origin [`Region`] convention.
fn fit_rect_zoom(
    bindings: &dyn PdfiumLibraryBindings,
    document: FPDF_DOCUMENT,
    dest: pdfium_render::prelude::FPDF_DEST,
    page_index: i32,
) -> Option<Region> {
    let mut count: std::ffi::c_ulong = 0;
    let mut params = [0f32; 4];
    let view = unsafe { bindings.FPDFDest_GetView(dest, &mut count, params.as_mut_ptr()) };
    if view != PDFDEST_VIEW_FITR || count < 4 {
        return None;
    }
    let [p0, p1, p2, p3] = params;
    // `/FitR left bottom right top`, in either order per axis.
    let (left, right) = (p0.min(p2), p0.max(p2));
    let (bottom, top) = (p1.min(p3), p1.max(p3));

    let page = unsafe { bindings.FPDF_LoadPage(document, page_index) };
    if page.is_null() {
        return None;
    }
    let page_width = unsafe { bindings.FPDF_GetPageWidthF(page) };
    let page_height = unsafe { bindings.FPDF_GetPageHeightF(page) };
    unsafe { bindings.FPDF_ClosePage(page) };
    if page_width <= 0.0 || page_height <= 0.0 {
        return None;
    }

    let x = (left / page_width).clamp(0.0, 1.0);
    let y = ((page_height - top) / page_height).clamp(0.0, 1.0);
    let width = ((right - left) / page_width).clamp(0.0, 1.0 - x);
    let height = ((top - bottom) / page_height).clamp(0.0, 1.0 - y);
    let region = Region::new(x, y, width, height);
    // A degenerate or full-page rectangle is not worth a re-render.
    (region.is_valid() && !region.is_full()).then_some(region)
}

/// Pause callback state handed to PDFium. `pause` must stay pinned for the
/// duration of the render, which is why it lives on this stack frame only.
#[repr(C)]
struct PauseState<'a> {
    pause: IFSDK_PAUSE,
    cancel: &'a dyn CancelSignal,
}

unsafe extern "C" fn need_to_pause_now(this: *mut IFSDK_PAUSE) -> i32 {
    if this.is_null() {
        return 0;
    }
    // `pause` is the first field, so the IFSDK_PAUSE pointer is the state
    // pointer.
    let state = this as *const PauseState<'_>;
    i32::from((*state).cancel.is_cancelled())
}

/// Draw the live form field values of one loaded page over the bitmap that
/// page was just rendered into.
///
/// PDFium splits a form's pixels in two, and this is the half the page render
/// does not do. `FPDF_RenderPageBitmap` draws page *content* and, with
/// `FPDF_ANNOT`, the page's annotations — except its `/Widget` ones, which it
/// draws not at all: every widget is left to the form-fill environment and
/// `FPDF_FFLDraw`, appearance stream and all.
///
/// Measured, because the division of labour is not what the flag names suggest:
/// with this pass suppressed and `FPDF_ANNOT` set, a signed signature's `/AP`
/// `/N` renders as nothing, and with `FPDF_ANNOT` cleared and this pass left in
/// it renders in full. So a renderer without an environment produces a form
/// whose boxes and printed labels are all there and whose answers are missing —
/// including the answers that were in the file when it was opened, which is what
/// made this look like a bug about typing rather than about drawing — and a
/// signed document whose visible signature is missing from the page while every
/// other viewer draws it.
///
/// One function, called from two places — the render pool here and the document
/// engine in `crate::document::pdfium` — because the two have to agree to the
/// pixel and used to be two copies that could only be checked by reading them
/// side by side. Everything that must match the page render is in here: the
/// placement (the page drawn at `full_size`, or at whatever size brings the
/// crop out at `width` × `height`, then shifted so the crop's corner lands on
/// the bitmap's origin) and the byte-order flag that decides what a pixel is.
///
/// What is *not* in here is the page's relationship with the form environment,
/// because that is the one part the two callers genuinely differ on. PDFium
/// will not draw fields into a page view it has not been told about, and which
/// view it is matters: a value someone is still typing belongs to the view the
/// interaction is open on, and a second handle for the same page has an empty
/// one. So `page` arrives already announced with `FORM_OnAfterLoadPage`, and
/// saying when it goes is the caller's business too.
///
/// Failure is silent by design: a page with no field values is a great deal
/// better than no page.
#[allow(
    clippy::too_many_arguments,
    reason = "the destination, its size and the crop it holds are seven \
              separate facts; grouping them into a struct would hide the \
              arithmetic that has to match the page render's"
)]
pub(crate) fn draw_form_fields(
    bindings: &dyn PdfiumLibraryBindings,
    form: FPDF_FORMHANDLE,
    page: FPDF_PAGE,
    rgba: &mut [u8],
    width: u32,
    height: u32,
    region: Region,
    full_size: Option<(u32, u32)>,
) {
    if width == 0 || height == 0 || rgba.len() < (width as usize) * (height as usize) * 4 {
        return;
    }
    let (start_x, start_y, full_width, full_height) =
        crate::pdf::page_placement(region, width, height, full_size);

    // The buffer the page was just drawn into, wrapped rather than copied:
    // `FPDF_FFLDraw` composites over what is already there, which is exactly
    // the page under the fields.
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
        return;
    }
    unsafe {
        bindings.FPDF_FFLDraw(
            form,
            bitmap,
            page,
            start_x,
            start_y,
            full_width,
            full_height,
            0,
            // The byte-order flag the page render used. Without it the field
            // text arrives with red and blue swapped.
            FPDF_REVERSE_BYTE_ORDER,
        );
        bindings.FPDFBitmap_Destroy(bitmap);
    }
}

/// [`draw_form_fields`] for a page the caller loaded itself, announcing it to
/// the form environment and taking it back out again.
///
/// The render pool's case: it loads a page, draws it, and has no interactive
/// state of its own to preserve.
#[allow(clippy::too_many_arguments, reason = "it forwards draw_form_fields'")]
fn composite_form_fields(
    bindings: &dyn PdfiumLibraryBindings,
    form: FPDF_FORMHANDLE,
    page: FPDF_PAGE,
    rgba: &mut [u8],
    width: u32,
    height: u32,
    region: Region,
    full_size: Option<(u32, u32)>,
) {
    // PDFium will not draw fields into a page it has not been told about.
    unsafe { bindings.FORM_OnAfterLoadPage(page, form) };
    draw_form_fields(bindings, form, page, rgba, width, height, region, full_size);
    unsafe { bindings.FORM_OnBeforeClosePage(page, form) };
}

/// Render one page into `pixels`, yielding to the cancel signal.
///
/// The `region` crop is expressed to PDFium as a page scaled to the full
/// logical size with a negative origin, so no intermediate full-page bitmap
/// is ever allocated.
#[allow(clippy::too_many_arguments)]
fn render_page_progressively(
    bindings: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    pixels: &mut [u8],
    width: u32,
    height: u32,
    region: Region,
    full_size: Option<(u32, u32)>,
    with_annotations: bool,
    cancel: &dyn CancelSignal,
) -> Result<()> {
    let stride = width as i32 * 4;
    let bitmap = unsafe {
        bindings.FPDFBitmap_CreateEx(
            width as i32,
            height as i32,
            FPDF_BITMAP_BGRA,
            pixels.as_mut_ptr() as *mut c_void,
            stride,
        )
    };
    if bitmap.is_null() {
        return Err(PdfError::Render("cannot create bitmap".into()));
    }
    // White page background; letterboxing is the audience view's job.
    unsafe {
        bindings.FPDFBitmap_FillRect(bitmap, 0, 0, width as i32, height as i32, 0xFFFF_FFFF);
    }

    // Full-page size in bitmap pixels, and the offset that brings the wanted
    // region to the bitmap origin.
    let (start_x, start_y, full_width, full_height) =
        crate::pdf::page_placement(region, width, height, full_size);

    let mut state = PauseState {
        pause: IFSDK_PAUSE {
            version: 1,
            NeedToPauseNow: Some(need_to_pause_now),
            user: std::ptr::null_mut(),
        },
        cancel,
    };
    let pause_ptr = &mut state.pause as *mut IFSDK_PAUSE;

    let mut status = unsafe {
        bindings.FPDF_RenderPageBitmap_Start(
            bitmap,
            page,
            start_x,
            start_y,
            full_width,
            full_height,
            0,
            // `FPDF_ANNOT` is bit 0. Document mode asks for it because the
            // marks are the point; a presentation does not, because the
            // document's own annotations are not the presenter's (see
            // `RenderRequest::with_annotations`).
            if with_annotations {
                FPDF_REVERSE_BYTE_ORDER | 0x01
            } else {
                FPDF_REVERSE_BYTE_ORDER
            },
            pause_ptr,
        )
    } as u32;

    while status == FPDF_RENDER_TOBECONTINUED {
        if cancel.is_cancelled() {
            unsafe {
                bindings.FPDF_RenderPage_Close(page);
                bindings.FPDFBitmap_Destroy(bitmap);
            }
            return Err(PdfError::Cancelled);
        }
        status = unsafe { bindings.FPDF_RenderPage_Continue(page, pause_ptr) } as u32;
    }

    unsafe {
        bindings.FPDF_RenderPage_Close(page);
        bindings.FPDFBitmap_Destroy(bitmap);
    }

    if status == FPDF_RENDER_DONE {
        Ok(())
    } else if cancel.is_cancelled() {
        Err(PdfError::Cancelled)
    } else {
        Err(PdfError::Render(format!("PDFium render status {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug these pin was invisible on Linux and broke every `run:`
    /// overlay on the other two platforms: PDFium rewrites a PDF path into
    /// the host's spelling, so the deck's `media-assets/bouncing.gif` arrived
    /// as `media-assets:bouncing.gif` on macOS and `media-assets\bouncing.gif`
    /// on Windows, and resolving it against the document directory then found
    /// nothing.
    #[test]
    fn a_relative_asset_path_survives_every_platform_rewrite() {
        let wanted = "media-assets/bouncing.gif";
        assert_eq!(pdf_separators(wanted, PathConvention::Posix), wanted);
        assert_eq!(
            pdf_separators("media-assets:bouncing.gif", PathConvention::Apple),
            wanted
        );
        assert_eq!(
            pdf_separators("media-assets\\bouncing.gif", PathConvention::Windows),
            wanted
        );
    }

    #[test]
    fn a_nested_path_converts_at_every_separator() {
        assert_eq!(
            pdf_separators("a:b:c:clip.mp4", PathConvention::Apple),
            "a/b/c/clip.mp4"
        );
        assert_eq!(
            pdf_separators("a\\b\\c\\clip.mp4", PathConvention::Windows),
            "a/b/c/clip.mp4"
        );
    }

    #[test]
    fn a_path_needing_no_rewrite_is_left_alone() {
        // A deck with a single-component asset name, and the platform whose
        // convention already is the PDF's.
        assert_eq!(
            pdf_separators("clip.mp4", PathConvention::Apple),
            "clip.mp4"
        );
        assert_eq!(
            pdf_separators("media/clip.mp4", PathConvention::Posix),
            "media/clip.mp4"
        );
    }

    #[test]
    fn the_host_convention_matches_the_platform_being_built_for() {
        // The constant is what production uses; a wrong one is a silent
        // breakage of every media overlay on that platform.
        #[cfg(target_os = "macos")]
        assert_eq!(HOST_CONVENTION, PathConvention::Apple);
        #[cfg(target_os = "windows")]
        assert_eq!(HOST_CONVENTION, PathConvention::Windows);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(HOST_CONVENTION, PathConvention::Posix);
    }

    #[test]
    fn the_render_asks_pdfium_for_rgba_directly() {
        // The old path byte-swapped every frame from BGRA; the flag that
        // replaced the swap must not silently change value.
        assert_eq!(FPDF_REVERSE_BYTE_ORDER, 0x10);
    }

    // The rendering path announces every page with `FORM_OnAfterLoadPage`
    // (see `draw_form_fields` above), which is what makes PDFium create its
    // V8 isolate — so this test is form-fill work whether or not the fixture
    // carries a form, and it owes the same one-thread discipline every
    // integration test observes. libtest hands each `#[test]` its own thread,
    // so without this the isolate is built on a thread that then exits.
    #[allow(dead_code)]
    mod pdfium_thread {
        include!("../../tests/testkit/pdfium_thread.rs");
    }
    use self::pdfium_thread::on_the_pdfium_thread;

    /// Only runs where a libpdfium is actually installed; CI without one
    /// still exercises everything above through the fixture backend.
    #[test]
    fn binds_and_renders_when_pdfium_is_present() {
        on_the_pdfium_thread(binds_and_renders);
    }

    fn binds_and_renders() {
        let Ok(mut backend) = PdfiumBackend::bind() else {
            eprintln!("skipping: no libpdfium available");
            return;
        };
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/three-pages.pdf");
        if !fixture.exists() {
            eprintln!("skipping: fixture not generated");
            return;
        }
        let document = backend.open(&fixture).unwrap();
        assert_eq!(backend.page_count(document).unwrap(), 3);
        let page = backend
            .render(
                &RenderRequest {
                    document,
                    page: 1,
                    region: Region::FULL,
                    width: 400,
                    height: 300,
                    full_size: None,
                    with_annotations: false,
                },
                &crate::pdf::NeverCancel,
            )
            .unwrap();
        assert!(page.is_consistent());
        assert!(
            page.pixels.iter().any(|byte| *byte != 0),
            "rendered something"
        );
    }
}
