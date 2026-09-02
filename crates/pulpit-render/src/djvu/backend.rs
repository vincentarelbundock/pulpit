//! DjVu behind the renderer's [`PdfBackend`] interface.
//!
//! `SPEC-reader-formats.md` §55 and §56. DjVu is a Class B format: pages
//! exist, have fixed sizes and render independently, so it fits the interface
//! as it stands and needs no new page model (§55.1). What it needs is a
//! library on the machine, and everything unusual here follows from that:
//! binding is lazy and per-document (§56.1), a missing library is `Unavailable`
//! naming djvulibre rather than a corrupt file (§61.2), and the backend runs
//! in the same supervised worker PDFium does, under the same crash recovery
//! (§56.2).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_ulong};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pulpit_core::PageSize;

use crate::djvu::sys::{
    self, Api, Context, Document, Format, Job, Page, FORMAT_RGB24, MESSAGE_ERROR, RENDER_COLOR,
    STATUS_FAILED, STATUS_OK,
};
use crate::djvu::text::{DjvuPageText, Placement};
use crate::pdf::{
    page_placement, BackendDocumentId, CancelSignal, DocumentMetadata, PdfBackend, PdfError,
    RenderRequest, RenderedPage, Result,
};

/// How much decoded DjVu djvulibre keeps around.
///
/// The same reasoning as the image tier's decoded cache (`SPEC-images.md`
/// §47.1): one page is wanted at three sizes at once — the audience frame,
/// the presenter frame and a thumbnail — and decoding it three times is the
/// difference between a responsive overview grid and a stuttering one.
/// djvulibre keeps this cache itself, so pulpit sizes it rather than building
/// a second one in front.
const DECODED_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// How many messages djvulibre's asynchronous API is pumped for before a
/// caller gives up on it (§77.8).
///
/// `ddjvu_document_get_pagetext` answers "not yet" until the page has been
/// fetched, and every asynchronous call in this API shares that shape —
/// `document_job` while a file decodes, `page_info` while a page's geometry
/// is fetched — each attempt costing one blocking wait on a message. A
/// message that has not arrived after this many is not going to: without a
/// bound here, `open` and `page_info` waited on `&NeverCancel` or on a
/// deadline enforced only by a *different* process, so a djvulibre context
/// that never posts another message parked this thread for good.
const MAX_WAIT_ATTEMPTS: usize = 4096;

/// The bounds a search answers within, shared with the PDF path so that a
/// query does not mean two different things depending on the format.
const MAX_HITS_PER_SEARCH: usize = crate::document::limits::MAX_HITS_PER_SEARCH;
const MAX_QUADS_PER_HIT: usize = crate::document::limits::MAX_QUADS_PER_HIT;

/// One open DjVu file.
struct OpenDocument {
    handle: *mut Document,
    path: PathBuf,
    page_count: usize,
    /// Sizes already measured, which §56.3 requires be readable without
    /// rendering. Memoised because `page_size` is asked the same question
    /// once per frame and the answer cannot change while the file is open.
    sizes: HashMap<usize, PageSize>,
}

/// Everything djvulibre owns, and the lock that serialises access to it.
struct Inner {
    api: Api,
    context: *mut Context,
    /// One pixel format for every render; it depends on nothing per-page.
    format: *mut Format,
    documents: HashMap<u64, OpenDocument>,
    next_id: u64,
    /// Text layers already read, in the same bounded cache the PDF path
    /// uses: after the first query a page with no match for the second costs
    /// nothing, and a book of dense pages fills the budget and stops.
    texts: crate::pdf::search::PageTextCache<(u64, usize), DjvuPageText>,
}

// SAFETY: every pointer in `Inner` is reached only through the `Mutex` in
// `DjvuBackend`, so no two threads are ever inside djvulibre through these
// handles at once. djvulibre runs decoding threads of its own, but those are
// internal to the library and reach the caller only through the message queue
// that `pump` drains under the same lock.
unsafe impl Send for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        // Documents first: they are jobs on the context, and releasing the
        // context out from under a live one is a use-after-free.
        for document in self.documents.values() {
            // SAFETY: each handle came from `document_create_by_filename_utf8`
            // on this context and is released exactly once, here.
            unsafe { self.api.document_release(document.handle) };
        }
        self.documents.clear();
        if !self.format.is_null() {
            // SAFETY: created by `format_create` below, released once.
            unsafe { (self.api.format_release)(self.format) };
        }
        if !self.context.is_null() {
            // SAFETY: created by `context_create` below, released once, and
            // after every job on it.
            unsafe { (self.api.context_release)(self.context) };
        }
        // Last, and only here: the claim is given back once the context is
        // fully torn down. Releasing it any earlier would let another thread
        // start a second context alongside one still being destroyed, which
        // is the race `BOUND` exists to prevent.
        BOUND.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Releases a page even when the render between here and there returns early.
struct PageGuard<'a> {
    api: &'a Api,
    page: *mut Page,
}

impl Drop for PageGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: `page` came from `page_create_by_pageno` and this is its
        // only release.
        unsafe { self.api.page_release(self.page) };
    }
}

/// Drain djvulibre's message queue, returning the first error text in it.
///
/// # Safety
///
/// `context` must be live and owned by `api`.
unsafe fn pump(api: &Api, context: *mut Context, wait: bool) -> Option<String> {
    let mut first_error = None;
    if wait {
        // Blocks until at least one message exists. It does not pop it, so
        // the peek loop below still sees it.
        (api.message_wait)(context);
    }
    loop {
        let message = (api.message_peek)(context);
        if message.is_null() {
            return first_error;
        }
        if (*message).tag == MESSAGE_ERROR {
            let error = message.cast::<sys::MessageError>();
            // The text belongs to the library and dies with the pop below,
            // so it is copied before that happens.
            if first_error.is_none() {
                first_error = sys::borrowed_text((*error).message);
            }
        }
        (api.message_pop)(context);
    }
}

/// Run djvulibre's message loop until `job` finishes.
///
/// Cancellation granularity is one message: the flag is checked between
/// messages, and djvulibre emits progress messages while it decodes, so a
/// cancelled render yields long before the page is finished. It cannot
/// interrupt djvulibre mid-message the way PDFium's progressive API can, which
/// is the same limitation the image tier carries (`SPEC-images.md` §47.3).
///
/// # Safety
///
/// `context` and `job` must be live and owned by `api`.
unsafe fn wait_for(
    api: &Api,
    context: *mut Context,
    job: *mut Job,
    cancel: &dyn CancelSignal,
) -> Result<()> {
    let mut last_error: Option<String> = None;
    let mut attempts = 0;
    loop {
        let status = (api.job_status)(job);
        if status >= STATUS_FAILED {
            return Err(PdfError::Render(last_error.unwrap_or_else(|| {
                "djvulibre could not decode this document".into()
            })));
        }
        if status >= STATUS_OK {
            return Ok(());
        }
        if cancel.is_cancelled() {
            return Err(PdfError::Cancelled);
        }
        // §77.8: a caller with no real cancel signal (`open`, on
        // `&NeverCancel`) must not be able to park this thread forever on a
        // context that stops posting messages.
        attempts += 1;
        if attempts > MAX_WAIT_ATTEMPTS {
            return Err(PdfError::Render(
                last_error.unwrap_or_else(|| "djvulibre never finished this job".into()),
            ));
        }
        if let Some(error) = pump(api, context, true) {
            last_error = Some(error);
        }
    }
}

/// Whether this process already holds a djvulibre context.
///
/// **Measured, not assumed.** Two `ddjvu_context_t` alive in one process are
/// safe as long as only one thread is inside the library; drive two of them
/// from two threads and
/// `ddjvu_document_create_by_filename_utf8` starts returning null for perfectly
/// good files, roughly one run in seven. A backend that reported "djvulibre
/// would not open this file" for a book that opens fine on the next attempt is
/// worse than one that refuses to exist twice, so the second bind is refused.
///
/// This is the same invariant PDFium carries, and it is what makes the worker
/// *process* boundary mandatory rather than stylistic: one library, one
/// context, one process (§56.2). Unlike PDFium's, the flag is cleared on drop,
/// because a djvulibre context is created and destroyed rather than
/// initialised once for the life of the process.
static BOUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A DjVu renderer, holding one djvulibre context and its open documents.
pub struct DjvuBackend {
    inner: Mutex<Inner>,
    version: String,
}

impl DjvuBackend {
    /// Find an installed djvulibre and start a context on it.
    ///
    /// The error is [`PdfError::Unavailable`] carrying
    /// [`crate::djvu::missing_djvu_message`], never [`PdfError::Open`]: "pulpit cannot read
    /// this kind of file" and "this file is damaged" are different facts, and
    /// reporting the second when the first is true sends a presenter looking
    /// for a problem that does not exist (§61.2).
    pub fn bind() -> Result<DjvuBackend> {
        if BOUND.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(PdfError::Unavailable(
                "djvulibre is already bound in this process; use one worker process per backend"
                    .into(),
            ));
        }
        // Every failure from here on must give the claim back, or a worker
        // that failed one bind could never try again.
        let release_claim = || BOUND.store(false, std::sync::atomic::Ordering::SeqCst);

        let api = Api::load().map_err(|reason| {
            release_claim();
            PdfError::Unavailable(crate::djvu::missing_djvu_message(&reason))
        })?;
        let program = std::ffi::CString::new("pulpit").expect("no NUL in a literal");
        // SAFETY: `program` outlives the call; djvulibre copies the name.
        let context = unsafe { (api.context_create)(program.as_ptr()) };
        if context.is_null() {
            release_claim();
            return Err(PdfError::Unavailable(
                "djvulibre was found but would not start a context".into(),
            ));
        }
        // SAFETY: the context was just created by this `api`.
        unsafe { (api.cache_set_size)(context, DECODED_CACHE_BYTES as c_ulong) };

        // SAFETY: RGB24 takes no format arguments, so a null argument array
        // with a zero count is what the header asks for.
        let format = unsafe { (api.format_create)(FORMAT_RGB24, 0, std::ptr::null()) };
        if format.is_null() {
            // SAFETY: nothing else holds the context; release it rather than
            // leaking it on the way out.
            unsafe { (api.context_release)(context) };
            release_claim();
            return Err(PdfError::Unavailable(
                "djvulibre would not create an RGB24 pixel format".into(),
            ));
        }
        // Both flags flip djvulibre's PostScript-like conventions to pulpit's:
        // rows arrive top first, and rectangle `y` is measured from the top of
        // the page. They are two separate settings — one describes the buffer,
        // the other the coordinates — and setting only one produces a page
        // that is upside down *or* cropped from the wrong end.
        // SAFETY: `format` was just created by this `api`.
        unsafe {
            (api.format_set_row_order)(format, 1);
            (api.format_set_y_direction)(format, 1);
        }

        let version = format!("djvulibre at {}", api.path().display());
        Ok(DjvuBackend {
            inner: Mutex::new(Inner {
                api,
                context,
                format,
                documents: HashMap::new(),
                next_id: 0,
                texts: crate::pdf::search::PageTextCache::default(),
            }),
            version,
        })
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Inner {
    fn look_up(&mut self, document: BackendDocumentId) -> Result<&mut OpenDocument> {
        self.documents
            .get_mut(&document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))
    }

    /// One page's `ddjvu_pageinfo_t`, waited out.
    ///
    /// This is the call §56.3 requires: page count and page sizes are read
    /// without rendering, and a backend that could only learn a page's size by
    /// rasterising it is not ready to be used. It is shared by `page_size` and
    /// by the text layer, which needs the rotation and the resolution to put a
    /// word where the renderer will draw it.
    fn page_info(&mut self, handle: *mut Document, page: usize) -> Result<sys::PageInfo> {
        let mut last_error: Option<String> = None;
        let mut attempts = 0;
        loop {
            // SAFETY: `handle` is a live document on this context.
            let (status, info) = unsafe { self.api.page_info(handle, page as c_int) };
            if status >= STATUS_FAILED {
                return Err(PdfError::Render(last_error.unwrap_or_else(|| {
                    format!("djvulibre could not describe page {}", page + 1)
                })));
            }
            if status >= STATUS_OK {
                return Ok(info);
            }
            // §77.8: `page_text` already bounds this same asynchronous shape;
            // `page_info` — reached from `page_size` and `page_text` alike —
            // must not be the one caller left free to wait forever.
            attempts += 1;
            if attempts > MAX_WAIT_ATTEMPTS {
                return Err(PdfError::Render(last_error.unwrap_or_else(|| {
                    format!("djvulibre never described page {}", page + 1)
                })));
            }
            // SAFETY: the context is live and owned by `self.api`.
            if let Some(error) = unsafe { pump(&self.api, self.context, true) } {
                last_error = Some(error);
            }
        }
    }

    /// One page's size in points, measured without rendering it (§56.3).
    fn page_size(&mut self, document: BackendDocumentId, page: usize) -> Result<PageSize> {
        let (handle, page_count, cached) = {
            let open = self.look_up(document)?;
            (open.handle, open.page_count, open.sizes.get(&page).copied())
        };
        if let Some(size) = cached {
            return Ok(size);
        }
        if page >= page_count {
            return Err(PdfError::PageOutOfRange {
                page,
                count: page_count,
            });
        }

        let info = self.page_info(handle, page)?;

        if info.width <= 0 || info.height <= 0 {
            return Err(PdfError::Render(format!("page {} has no size", page + 1)));
        }
        // `info.rotation` is *not* applied here, and that is a measured
        // decision rather than an oversight. `ddjvuapi.h` says the page's
        // stored rotation "is automatically taken into account by
        // ddjvu_page_render, ddjvu_page_get_width and ddjvu_page_get_height",
        // which reads as though `get_pageinfo` — a different call, and the
        // only one that answers without decoding — were the exception. It is
        // not: a quarter-turned page reports its *turned* dimensions here and
        // carries the angle alongside them, so turning them again would
        // report every rotated scan at the wrong aspect and letterbox it
        // against a frame shaped for the other orientation.
        // `tests/djvu_backend.rs` pins this against a rotated fixture,
        // because the header's wording invites the opposite conclusion.
        let (pixel_width, pixel_height) = (info.width, info.height);
        // Points, not pixels, so that a document mixing a 300dpi scan with a
        // 600dpi plate reports the two at their true relative sizes. A file
        // with no usable resolution keeps pixels, which preserves the aspect
        // ratio even when it loses the scale.
        let scale = if info.dpi > 0 {
            72.0 / info.dpi as f32
        } else {
            1.0
        };
        let size = PageSize {
            width: pixel_width as f32 * scale,
            height: pixel_height as f32 * scale,
        };
        self.look_up(document)?.sizes.insert(page, size);
        Ok(size)
    }

    /// One page's text layer, read once and kept (§59.2).
    ///
    /// Returns it beside the placement that maps its coordinates onto the
    /// page, because the two come from the same `pageinfo` call and a hit
    /// drawn with one and measured by the other would land in the wrong place
    /// on any rotated scan.
    fn page_text(
        &mut self,
        document: BackendDocumentId,
        page: usize,
    ) -> Result<(std::sync::Arc<DjvuPageText>, Placement)> {
        let (handle, page_count) = {
            let open = self.look_up(document)?;
            (open.handle, open.page_count)
        };
        if page >= page_count {
            return Err(PdfError::PageOutOfRange {
                page,
                count: page_count,
            });
        }
        let info = self.page_info(handle, page)?;
        // The same points-per-pixel `page_size` uses, so a word's box is in
        // the same units as the page it sits on.
        let scale = if info.dpi > 0 {
            72.0 / info.dpi as f32
        } else {
            1.0
        };
        let at = Placement::new(info.width, info.height, info.rotation, scale);
        if let Some(text) = self.texts.get(&(document.0, page)) {
            return Ok((text, at));
        }

        // `get_pagetext` answers `miniexp_dummy` while it fetches the page and
        // has to be asked again, which is the documented shape of every
        // asynchronous call in this API. The bound is not for a well-behaved
        // library: it is so that a file whose text never arrives fails the
        // search rather than parking a worker thread in this loop for good.
        let mut attempts = 0;
        let expression = loop {
            // SAFETY: `handle` is a live document on this context, the page
            // was bounds-checked above, and the detail string is a literal.
            let expression = unsafe {
                (self.api.document_get_pagetext)(
                    handle,
                    page as c_int,
                    crate::djvu::text::MAX_DETAIL.as_ptr().cast::<c_char>(),
                )
            };
            if expression != sys::MINIEXP_DUMMY {
                break expression;
            }
            attempts += 1;
            if attempts > MAX_WAIT_ATTEMPTS {
                return Err(PdfError::Render(format!(
                    "djvulibre never produced the text of page {}",
                    page + 1
                )));
            }
            // SAFETY: the context is live and owned by `self.api`.
            unsafe { pump(&self.api, self.context, true) };
        };

        // SAFETY: `expression` came from this document's `get_pagetext` and is
        // released below, once, after the walk that reads it.
        let text = unsafe {
            let text = DjvuPageText::from_expression(&self.api, expression);
            (self.api.miniexp_release)(handle, expression);
            text
        };
        Ok((self.texts.insert((document.0, page), text), at))
    }

    fn render_into(
        &mut self,
        request: &RenderRequest,
        target: &mut [u8],
        cancel: &dyn CancelSignal,
    ) -> Result<()> {
        request.validate()?;
        let needed = request.width as usize * request.height as usize * 4;
        if target.len() < needed {
            return Err(PdfError::Invalid(format!(
                "a {}×{} frame needs {needed} bytes, and the target holds {}",
                request.width,
                request.height,
                target.len()
            )));
        }
        let (handle, page_count) = {
            let open = self.look_up(request.document)?;
            (open.handle, open.page_count)
        };
        if request.page >= page_count {
            return Err(PdfError::PageOutOfRange {
                page: request.page,
                count: page_count,
            });
        }

        let (start_x, start_y, full_width, full_height) = page_placement(
            request.region,
            request.width,
            request.height,
            request.full_size,
        );
        // The two rounds in `page_placement` can leave a crop a pixel past
        // the edge of the page rectangle it was derived from, and djvulibre
        // refuses a render rectangle that is not inside the page one. Growing
        // the page by that pixel keeps the render; shrinking the crop would
        // return a frame smaller than the caller's buffer expects.
        let full_width = full_width.max(-start_x + request.width as i32).max(1);
        let full_height = full_height.max(-start_y + request.height as i32).max(1);
        let page_rect = sys::Rect {
            x: 0,
            y: 0,
            w: full_width as u32,
            h: full_height as u32,
        };
        let render_rect = sys::Rect {
            x: -start_x,
            y: -start_y,
            w: request.width,
            h: request.height,
        };

        // SAFETY: `handle` is a live document on this context, and the page
        // index was bounds-checked above.
        let page = unsafe { (self.api.page_create_by_pageno)(handle, request.page as c_int) };
        if page.is_null() {
            return Err(PdfError::Render(format!(
                "djvulibre would not open page {}",
                request.page + 1
            )));
        }
        let guard = PageGuard {
            api: &self.api,
            page,
        };

        // A partially decoded page renders as a blurry approximation, which
        // is exactly what the third standing rule forbids putting in front of
        // an audience: the last complete frame stays until a complete
        // replacement exists. So the decode is waited out.
        // SAFETY: context and job are live and owned by `self.api`.
        unsafe { wait_for(&self.api, self.context, (self.api.page_job)(page), cancel)? };
        if cancel.is_cancelled() {
            return Err(PdfError::Cancelled);
        }

        let row_bytes = request.width as usize * 3;
        let mut rgb = vec![0u8; row_bytes * request.height as usize];
        // SAFETY: `rgb` holds exactly `row_bytes * height` bytes, which is
        // what the rectangle and row stride below describe.
        let painted = unsafe {
            (self.api.page_render)(
                page,
                RENDER_COLOR,
                &page_rect,
                &render_rect,
                self.format,
                row_bytes as c_ulong,
                rgb.as_mut_ptr().cast::<c_char>(),
            )
        };
        drop(guard);
        if painted == 0 {
            return Err(PdfError::Render(format!(
                "djvulibre produced no image for page {}",
                request.page + 1
            )));
        }

        let (pixels, _) = target[..needed].as_chunks_mut::<4>();
        let (sources, _) = rgb.as_chunks::<3>();
        for (pixel, source) in pixels.iter_mut().zip(sources) {
            pixel[0] = source[0];
            pixel[1] = source[1];
            pixel[2] = source[2];
            // DjVu has no transparency: a page is paper.
            pixel[3] = 0xff;
        }
        Ok(())
    }
}

impl PdfBackend for DjvuBackend {
    fn name(&self) -> &'static str {
        "djvu"
    }

    fn version(&self) -> String {
        self.version.clone()
    }

    fn open(&mut self, source: &Path) -> Result<BackendDocumentId> {
        let argument = sys::path_argument(source).map_err(|reason| PdfError::Open {
            path: source.display().to_string(),
            reason,
        })?;
        let mut inner = self.locked();
        let context = inner.context;
        // SAFETY: the context is live, and `argument` outlives the call.
        let handle =
            unsafe { (inner.api.document_create_by_filename_utf8)(context, argument.as_ptr(), 1) };
        if handle.is_null() {
            return Err(PdfError::Open {
                path: source.display().to_string(),
                reason: "djvulibre would not open this file".into(),
            });
        }

        // From here on the document must be released on every failure path.
        // SAFETY: context and job are live and owned by this api.
        let decoded = unsafe {
            let job = (inner.api.document_job)(handle);
            wait_for(&inner.api, context, job, &crate::pdf::NeverCancel)
        };
        let page_count = match decoded {
            // SAFETY: the document decoded successfully and is still live.
            Ok(()) => unsafe { (inner.api.document_get_pagenum)(handle) },
            Err(error) => {
                // SAFETY: released exactly once, and nothing else holds it.
                unsafe { inner.api.document_release(handle) };
                return Err(PdfError::Open {
                    path: source.display().to_string(),
                    reason: error.to_string(),
                });
            }
        };
        if page_count <= 0 {
            // SAFETY: released exactly once, and nothing else holds it.
            unsafe { inner.api.document_release(handle) };
            return Err(PdfError::Open {
                path: source.display().to_string(),
                reason: "this DjVu file has no pages".into(),
            });
        }

        inner.next_id += 1;
        let id = inner.next_id;
        inner.documents.insert(
            id,
            OpenDocument {
                handle,
                path: source.to_path_buf(),
                page_count: page_count as usize,
                sizes: HashMap::new(),
            },
        );
        Ok(BackendDocumentId(id))
    }

    fn close(&mut self, document: BackendDocumentId) {
        let mut inner = self.locked();
        inner.texts.retain(|(id, _)| *id != document.0);
        if let Some(open) = inner.documents.remove(&document.0) {
            // SAFETY: this handle is removed from the map first, so this is
            // its only release.
            unsafe { inner.api.document_release(open.handle) };
        }
    }

    fn metadata(&self, document: BackendDocumentId) -> Result<DocumentMetadata> {
        // The lock is taken and dropped before `collect_page_sizes`, which
        // calls back into `page_size` and would deadlock against a lock still
        // held here.
        let page_count = self.locked().look_up(document)?.page_count;
        let (page_sizes, page_sizes_sampled) =
            crate::pdf::collect_page_sizes(self as &dyn PdfBackend, document, page_count);
        Ok(DocumentMetadata {
            page_count,
            first_page_size: page_sizes.first().copied().unwrap_or(PageSize {
                width: 1.0,
                height: 1.0,
            }),
            page_sizes,
            page_sizes_sampled,
            // §59.4 pins `NotesMapping::SlidesOnly` for every reader format,
            // and DjVu carries no `.pdfpc` sidecar to look in.
            metadata_text: String::new(),
            // A file, not a directory: the digest that detects a directory
            // changing under a reader has nothing to describe here
            // (`SPEC-images.md` §42.3).
            source_digest: None,
        })
    }

    fn page_size(&self, document: BackendDocumentId, page: usize) -> Result<PageSize> {
        self.locked().page_size(document, page)
    }

    fn render(&self, request: &RenderRequest, cancel: &dyn CancelSignal) -> Result<RenderedPage> {
        crate::pdf::render_via_render_into(self, request, cancel)
    }

    /// §56.4: djvulibre rasterises into a caller-supplied buffer, so the
    /// worker's shared-memory mapping is passed all the way down rather than
    /// being filled from an intermediate frame.
    fn render_into(
        &self,
        request: &RenderRequest,
        target: &mut [u8],
        cancel: &dyn CancelSignal,
    ) -> Result<()> {
        self.locked().render_into(request, target, cancel)
    }

    /// §59.2. A DjVu carries its text as hidden zones beside the scan, and a
    /// scanned book is exactly the document somebody searches rather than
    /// reads front to back.
    ///
    /// The matching is `pulpit_core::search`'s, the same matcher the PDF path
    /// and the notes run through, so a hit found here is the hit the reader
    /// finds; what this backend contributes is the geometry, one box per word
    /// (`crate::djvu::text`).
    ///
    /// A page with no text layer is not a failure — a plate, a map, a page the
    /// producer never ran OCR over — and answers no matches. A backend that
    /// could not search at all would still say so, which is a different fact
    /// and was this backend's honest answer until now.
    fn find_text(
        &self,
        document: BackendDocumentId,
        query: &pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) -> Result<Vec<pulpit_core::search::Hit>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let prepared = query.prepare();
        let mut inner = self.locked();
        // Clamped rather than refused: the caller's page count can be one
        // reload behind, and a scan that walks off the end should stop.
        let page_count = inner.look_up(document)?.page_count;
        Ok(crate::pdf::search::search_pages(
            pages.start..pages.end.min(page_count),
            MAX_HITS_PER_SEARCH,
            |page| -> Result<Vec<pulpit_core::search::Hit>> {
                let (text, at) = inner.page_text(document, page)?;
                let found = text.matches(&prepared, MAX_HITS_PER_SEARCH);
                Ok(crate::pdf::search::hits_from_matches(
                    pulpit_core::page::PageIndex(page),
                    text.text(),
                    &found,
                    |matched| text.quads(matched, &at, MAX_QUADS_PER_HIT),
                ))
            },
        ))
    }
}

impl DjvuBackend {
    /// The file behind an open document, for diagnostics.
    pub fn source(&self, document: BackendDocumentId) -> Option<PathBuf> {
        self.locked()
            .documents
            .get(&document.0)
            .map(|open| open.path.clone())
    }
}
