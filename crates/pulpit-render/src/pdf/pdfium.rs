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
    Pdfium, PdfiumLibraryBindings, FPDF_ATTACHMENT, FPDF_BOOKMARK, FPDF_DOCUMENT, FPDF_LINK,
    FPDF_PAGE, FS_RECTF, IFSDK_PAUSE,
};

/// PDFium ABI constants that `pdfium-render` does not re-export.
const FPDF_BITMAP_BGRA: i32 = 4;
const FPDF_RENDER_TOBECONTINUED: u32 = 1;
const FPDF_RENDER_DONE: u32 = 2;
/// `FPDF_REVERSE_BYTE_ORDER` from `fpdfview.h`: PDFium fills the BGRA bitmap
/// in RGBA byte order instead. The UI and the protocol only ever consume
/// RGBA, so rendering it directly saves a full read-modify-write pass over
/// every frame.
const FPDF_REVERSE_BYTE_ORDER: i32 = 0x10;
/// Path drawing constants from `fpdf_edit.h`: `FPDF_FILLMODE_NONE`, and the
/// round cap and join the presenter panels draw their strokes with.
const FILL_MODE_NONE: std::ffi::c_int = 0;
const LINE_CAP_ROUND: std::ffi::c_int = 1;
const LINE_JOIN_ROUND: std::ffi::c_int = 1;
/// A stroke thinner than this disappears at print resolution. Nothing the
/// palette offers is anywhere near it; the floor exists so a deck with an
/// unusually large page cannot round a mark down to nothing.
const MIN_STROKE_POINTS: f32 = 0.1;
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
    BackendDocumentId, CancelSignal, DocumentMetadata, PageStamp, PdfBackend, PdfError,
    RenderRequest, RenderedPage, Result, StampImage,
};
use pulpit_core::annotation::StrokeKind;

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
    next_id: u64,
    library_path: Option<PathBuf>,
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
                        paths: HashMap::new(),
                        next_id: 0,
                        library_path: Some(candidate),
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
                    paths: HashMap::new(),
                    next_id: 0,
                    library_path: None,
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

    /// Draw every stamp into `document`, then write it out.
    ///
    /// A page that cannot be loaded or measured is left exactly as it was
    /// rather than aborting the export: a deck saves with the marks it could
    /// place, which is strictly better than no file at all, and the pages
    /// that failed are the ones the presenter can see are missing them.
    fn stamp_and_save(
        &self,
        document: FPDF_DOCUMENT,
        destination: &Path,
        pages: &[PageStamp],
    ) -> Result<()> {
        let count = unsafe { self.bindings.FPDF_GetPageCount(document) } as usize;
        for stamp in pages {
            if stamp.page >= count {
                continue;
            }
            if stamp.strokes.is_empty() && stamp.images.is_empty() {
                continue;
            }
            let page = unsafe { self.bindings.FPDF_LoadPage(document, stamp.page as i32) };
            if page.is_null() {
                tracing::warn!(
                    page = stamp.page,
                    "cannot load page to stamp; left unmarked"
                );
                continue;
            }
            match PageFrame::measure(&*self.bindings, page) {
                Some(frame) => {
                    self.stamp_page(document, page, &frame, stamp);
                    // Without this the objects exist in memory and are absent
                    // from the saved file: content generation is what commits
                    // them to the page's stream.
                    if unsafe { self.bindings.FPDFPage_GenerateContent(page) } == 0 {
                        tracing::warn!(page = stamp.page, "content generation refused the marks");
                    }
                }
                None => tracing::warn!(page = stamp.page, "cannot measure page; left unmarked"),
            }
            unsafe { self.bindings.FPDF_ClosePage(page) };
        }
        let bytes = self.save_to_memory(document)?;
        write_atomically(destination, &bytes)
    }

    /// Put one page's marks on it, as ordinary page content.
    fn stamp_page(
        &self,
        document: FPDF_DOCUMENT,
        page: FPDF_PAGE,
        frame: &PageFrame,
        stamp: &PageStamp,
    ) {
        let region = stamp.region;
        // Marks are normalised inside the region the panel showed, so a
        // split-page deck lands its ink on the half it was drawn over.
        let locate = |x: f32, y: f32| {
            frame.to_user(region.x + x * region.width, region.y + y * region.height)
        };
        for stroke in &stamp.strokes {
            let Some((first, rest)) = stroke.points.split_first() else {
                continue;
            };
            let (x, y) = locate(first.0, first.1);
            let path = unsafe { self.bindings.FPDFPageObj_CreateNewPath(x, y) };
            if path.is_null() {
                continue;
            }
            for point in rest {
                let (x, y) = locate(point.0, point.1);
                unsafe { self.bindings.FPDFPath_LineTo(path, x, y) };
            }
            let (red, green, blue) = stroke.color.rgb();
            let alpha = (stroke.kind.opacity() * 255.0).round().clamp(0.0, 255.0) as u32;
            unsafe {
                self.bindings.FPDFPageObj_SetStrokeColor(
                    path,
                    channel(red),
                    channel(green),
                    channel(blue),
                    alpha,
                );
                self.bindings.FPDFPageObj_SetStrokeWidth(
                    path,
                    (stroke.width * region.width * frame.display_width).max(MIN_STROKE_POINTS),
                );
                // The panels draw round caps and joins; a stroke that grew
                // mitred corners on the way into the file would not be the
                // mark the room watched being made.
                self.bindings.FPDFPageObj_SetLineCap(path, LINE_CAP_ROUND);
                self.bindings.FPDFPageObj_SetLineJoin(path, LINE_JOIN_ROUND);
                // The highlighter is translucent *and* multiplies, which is
                // how it darkens the text under it instead of fogging it.
                if stroke.kind == StrokeKind::Highlight {
                    self.bindings.FPDFPageObj_SetBlendMode(path, "Multiply");
                }
                self.bindings.FPDFPath_SetDrawMode(path, FILL_MODE_NONE, 1);
                let _ = self.bindings.FPDFPage_InsertObject(page, path);
            }
        }
        for image in &stamp.images {
            self.stamp_image(document, page, frame, region, image);
        }
    }

    fn stamp_image(
        &self,
        document: FPDF_DOCUMENT,
        page: FPDF_PAGE,
        frame: &PageFrame,
        region: Region,
        image: &StampImage,
    ) {
        // The panel fits the picture into whatever is left of the page to the
        // right of and below its corner, keeping its aspect. The same
        // arithmetic here is what makes the file agree with the screen.
        let left = region.x + image.x * region.width;
        let top = region.y + image.y * region.height;
        let available_width = (image.width * region.width * frame.display_width).max(0.0);
        let available_height = (((region.y + region.height) - top) * frame.display_height).max(0.0);
        let aspect = image.pixel_width as f32 / image.pixel_height as f32;
        let (width, height) = if available_width / aspect > available_height {
            (available_height * aspect, available_height)
        } else {
            (available_width, available_width / aspect)
        };
        if width <= 0.0 || height <= 0.0 {
            return;
        }

        let bitmap = unsafe {
            self.bindings.FPDFBitmap_CreateEx(
                image.pixel_width as i32,
                image.pixel_height as i32,
                FPDF_BITMAP_BGRA,
                std::ptr::null_mut(),
                image.pixel_width as i32 * 4,
            )
        };
        if bitmap.is_null() {
            return;
        }
        // PDFium owns this buffer; `FPDFImageObj_SetBitmap` copies out of it,
        // so it only has to be correct for the length of that call.
        let buffer = unsafe { self.bindings.FPDFBitmap_GetBuffer(bitmap) };
        if !buffer.is_null() {
            let stride = unsafe { self.bindings.FPDFBitmap_GetStride(bitmap) } as usize;
            let target = unsafe {
                std::slice::from_raw_parts_mut(
                    buffer as *mut u8,
                    stride * image.pixel_height as usize,
                )
            };
            for row in 0..image.pixel_height as usize {
                for column in 0..image.pixel_width as usize {
                    let from = (row * image.pixel_width as usize + column) * 4;
                    let to = row * stride + column * 4;
                    target[to] = image.rgba[from + 2];
                    target[to + 1] = image.rgba[from + 1];
                    target[to + 2] = image.rgba[from];
                    target[to + 3] = image.rgba[from + 3];
                }
            }
        }

        let object = unsafe { self.bindings.FPDFPageObj_NewImageObj(document) };
        if !object.is_null() {
            let ok = unsafe {
                self.bindings
                    .FPDFImageObj_SetBitmap(std::ptr::null_mut(), 0, object, bitmap)
            };
            if ok != 0 {
                // A PDF image is a unit square with its origin at the
                // bottom left, so the matrix is anchored there rather than
                // at the corner the presenter placed.
                let (x, y) = frame.to_user(left, top);
                let matrix = pdfium_render::prelude::FS_MATRIX {
                    a: frame.right.0 * width,
                    b: frame.right.1 * width,
                    c: -frame.down.0 * height,
                    d: -frame.down.1 * height,
                    e: x + frame.down.0 * height,
                    f: y + frame.down.1 * height,
                };
                unsafe {
                    self.bindings.FPDFPageObj_SetMatrix(object, &matrix);
                    let _ = self.bindings.FPDFPage_InsertObject(page, object);
                }
            }
        }
        unsafe { self.bindings.FPDFBitmap_Destroy(bitmap) };
    }

    /// Serialise the document into memory.
    ///
    /// Buffering the whole file before a byte of it reaches the destination
    /// is what makes [`write_atomically`] able to promise the presenter that
    /// a save either produced a complete PDF or produced nothing.
    fn save_to_memory(&self, document: FPDF_DOCUMENT) -> Result<Vec<u8>> {
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
                0,
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
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
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

/// The bookmark tree, as the domain's outline walk wants to see it.
///
/// Nothing here bounds anything: the depth, cycle and count guards all live in
/// [`pulpit_core::navigation::build_outline`], which is where they can be
/// tested without a PDF. This adapter's only job is the two-call
/// length-then-buffer dance PDFium strings require.
/// Where a page's content lives in user space, and which way round it is
/// drawn.
///
/// Everything above this module works in normalised coordinates over the
/// picture PDFium *renders*; page objects are placed in the page's own user
/// space, which a `/Rotate` entry turns relative to it. Resolving the two
/// once, here, is what keeps a rotated page from receiving its ink sideways.
struct PageFrame {
    /// Size of the displayed page in points.
    display_width: f32,
    display_height: f32,
    /// User-space direction of displayed rightwards and downwards. Unit
    /// vectors: `/Rotate` is a rotation, never a scale.
    right: (f32, f32),
    down: (f32, f32),
    /// User-space point the displayed top-left corner sits at.
    origin: (f32, f32),
}

impl PageFrame {
    fn measure(bindings: &dyn PdfiumLibraryBindings, page: FPDF_PAGE) -> Option<Self> {
        // The crop box is what PDFium renders; the media box is the sheet it
        // was imposed on, and the two differ in any deck that was trimmed.
        let box_of = |read: BoxReader| {
            let (mut left, mut bottom, mut right, mut top) = (0.0, 0.0, 0.0, 0.0);
            let ok = unsafe { read(bindings, page, &mut left, &mut bottom, &mut right, &mut top) };
            (ok != 0 && right > left && top > bottom).then_some((left, bottom, right, top))
        };
        let (left, bottom, right, top) =
            box_of(read_crop_box).or_else(|| box_of(read_media_box))?;
        let width = right - left;
        let height = top - bottom;
        // PDFium reports the rotation in quarter turns clockwise.
        let quarters = unsafe { bindings.FPDFPage_GetRotation(page) }.rem_euclid(4);
        Some(match quarters {
            1 => Self {
                display_width: height,
                display_height: width,
                right: (0.0, 1.0),
                down: (1.0, 0.0),
                origin: (left, bottom),
            },
            2 => Self {
                display_width: width,
                display_height: height,
                right: (-1.0, 0.0),
                down: (0.0, 1.0),
                origin: (right, bottom),
            },
            3 => Self {
                display_width: height,
                display_height: width,
                right: (0.0, -1.0),
                down: (-1.0, 0.0),
                origin: (right, top),
            },
            _ => Self {
                display_width: width,
                display_height: height,
                right: (1.0, 0.0),
                down: (0.0, -1.0),
                origin: (left, top),
            },
        })
    }

    /// Normalised coordinates over the displayed page to a point in the
    /// page's own user space.
    fn to_user(&self, u: f32, v: f32) -> (f32, f32) {
        let across = u * self.display_width;
        let down = v * self.display_height;
        (
            self.origin.0 + self.right.0 * across + self.down.0 * down,
            self.origin.1 + self.right.1 * across + self.down.1 * down,
        )
    }
}

type BoxReader = unsafe fn(
    &dyn PdfiumLibraryBindings,
    FPDF_PAGE,
    &mut f32,
    &mut f32,
    &mut f32,
    &mut f32,
) -> pdfium_render::prelude::FPDF_BOOL;

unsafe fn read_crop_box(
    bindings: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    left: &mut f32,
    bottom: &mut f32,
    right: &mut f32,
    top: &mut f32,
) -> pdfium_render::prelude::FPDF_BOOL {
    unsafe { bindings.FPDFPage_GetCropBox(page, left, bottom, right, top) }
}

unsafe fn read_media_box(
    bindings: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    left: &mut f32,
    bottom: &mut f32,
    right: &mut f32,
    top: &mut f32,
) -> pdfium_render::prelude::FPDF_BOOL {
    unsafe { bindings.FPDFPage_GetMediaBox(page, left, bottom, right, top) }
}

fn channel(value: f32) -> std::ffi::c_uint {
    (value * 255.0).round().clamp(0.0, 255.0) as std::ffi::c_uint
}

/// Write `bytes` to `destination` through a temporary file in the same
/// directory, so an interrupted save leaves the presenter's chosen path
/// either untouched or holding a complete PDF — never half of one, and never
/// a truncated overwrite of a file they already had.
fn write_atomically(destination: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let directory = destination.parent().unwrap_or_else(|| Path::new("."));
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = directory.join(format!(".pulpit-export-{}-{ticket}", std::process::id()));

    let write = |path: &Path| -> std::io::Result<()> {
        let mut file = std::fs::File::create(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    if let Err(e) = write(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(PdfError::Render(format!(
            "cannot write {}: {e}",
            temporary.display()
        )));
    }
    std::fs::rename(&temporary, destination).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        PdfError::Render(format!("cannot save {}: {e}", destination.display()))
    })
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
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
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
        Ok(BackendDocumentId(self.next_id))
    }

    fn export_annotated(
        &self,
        source: &Path,
        destination: &Path,
        pages: &[PageStamp],
    ) -> Result<()> {
        for stamp in pages {
            stamp.validate()?;
        }
        // A second, independent handle on the same file. The documents this
        // backend renders from are never stamped: mutating one would change
        // what the audience is looking at, and a page is not re-rendered just
        // because a save happened.
        let path = source.to_string_lossy().to_string();
        let document = unsafe { self.bindings.FPDF_LoadDocument(&path, None) };
        if document.is_null() {
            let code = unsafe { self.bindings.FPDF_GetLastError() };
            return Err(PdfError::Open {
                path,
                reason: format!("PDFium error {code}"),
            });
        }
        let result = self.stamp_and_save(document, destination, pages);
        unsafe { self.bindings.FPDF_CloseDocument(document) };
        result
    }

    fn close(&mut self, document: BackendDocumentId) {
        self.paths.remove(&document.0);
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
        self.with_page(request.document, request.page, |page| {
            render_page_progressively(
                self.bindings.as_ref(),
                page,
                &mut target[..bytes],
                request.width,
                request.height,
                request.region,
                cancel,
            )
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
    let full_width = (width as f32 / region.width).round() as i32;
    let full_height = (height as f32 / region.height).round() as i32;
    let start_x = -((region.x * full_width as f32).round() as i32);
    let start_y = -((region.y * full_height as f32).round() as i32);

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
            FPDF_REVERSE_BYTE_ORDER,
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

    /// Only runs where a libpdfium is actually installed; CI without one
    /// still exercises everything above through the fixture backend.
    #[test]
    fn binds_and_renders_when_pdfium_is_present() {
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
