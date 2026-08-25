//! Several backends in one worker (`SPEC-images.md` §45,
//! `SPEC-reader-formats.md` §56.1).
//!
//! `select_backend()` used to choose once at startup, before any path was
//! known. A worker holds several documents at a time — always two during a
//! reload — and after the image tier they need not be the same kind, so the
//! choice moves to `open` and is made per source: a directory routes to the
//! image backend, a `.djvu` to djvulibre, anything else to PDFium.
//!
//! **This softens a documented invariant, deliberately.** A worker that
//! cannot bind PDFium still prints [`crate::pdf::missing_pdfium_message`] and
//! exits — but on the first *PDF* open, not at startup. Refusing to display a
//! JPEG because a PDF library is absent is not defensible, and the reasoning
//! behind the original rule (a deck silently rendering as blanks) does not
//! apply to a format the worker can fully decode (§45.3, §45.4).
//!
//! DjVu generalises that to a standing rule. §56.1 requires binding be lazy
//! and per-document *in both directions*: a missing djvulibre must not stop
//! this worker opening a PDF, and a missing PDFium must not stop it opening a
//! DjVu. Each route binds its own library on its own first open, and a
//! failure fails that open alone (§65.2).

use std::collections::HashMap;
use std::path::Path;

use pulpit_core::navigation::Outline;
use pulpit_core::{PageLink, PageSize};

use crate::images::backend::ImageBackend;
use crate::images::table::resolve_source;
use crate::pdf::capabilities::DocumentEvidence;
use crate::pdf::{
    BackendDocumentId, CancelSignal, DocumentMetadata, PdfBackend, PdfError, RenderRequest,
    RenderedPage, Result,
};

/// How a PDF backend is produced, the first time one is needed (§45.4).
///
/// A closure rather than a concrete type so the two worker entry points can
/// decide what "no PDFium" means for them — the shipped binaries print the
/// diagnostic and exit, and a test hands over a fixture — while this module
/// stays free of both PDFium and `std::process::exit`.
pub type BindPdf = Box<dyn FnMut() -> Result<Box<dyn PdfBackend>> + Send>;

/// How a DjVu backend is produced, the first time one is needed (§56.1).
///
/// Same shape as [`BindPdf`] and for the same reason: it keeps this module
/// free of djvulibre, and lets a test exercise the routing on a machine that
/// has no DjVu library at all — which is most of them, and is exactly the
/// case §65.2 is about.
pub type BindDjvu = Box<dyn FnMut() -> Result<Box<dyn PdfBackend>> + Send>;

/// Bind the real djvulibre, discovered on the machine.
///
/// The `not(feature)` arm is not a stub: a build compiled without the backend
/// must still refuse a `.djvu` *by name*, saying what is missing, rather than
/// letting it fall through to PDFium and be reported as a damaged PDF
/// (§61.1, §61.2).
pub fn bind_system_djvu() -> Result<Box<dyn PdfBackend>> {
    #[cfg(feature = "djvu")]
    {
        crate::djvu::DjvuBackend::bind().map(|backend| Box::new(backend) as Box<dyn PdfBackend>)
    }
    #[cfg(not(feature = "djvu"))]
    {
        Err(PdfError::Unavailable(crate::djvu::missing_djvu_message(
            "this build was compiled without the djvu feature",
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Images(BackendDocumentId),
    Pdf(BackendDocumentId),
    Djvu(BackendDocumentId),
}

/// Dispatches per [`BackendDocumentId`], so one worker can hold a deck, a
/// scanned book and a folder of pictures at the same time.
pub struct RoutingBackend {
    images: ImageBackend,
    pdf: Option<Box<dyn PdfBackend>>,
    bind: BindPdf,
    djvu: Option<Box<dyn PdfBackend>>,
    bind_djvu: BindDjvu,
    routes: HashMap<u64, Route>,
    next_id: u64,
}

impl RoutingBackend {
    pub fn new(bind: BindPdf) -> RoutingBackend {
        RoutingBackend {
            images: ImageBackend::new(),
            pdf: None,
            bind,
            djvu: None,
            bind_djvu: Box::new(bind_system_djvu),
            routes: HashMap::new(),
            next_id: 0,
        }
    }

    /// Replace how a DjVu backend is bound. Only tests ask, so that the
    /// routing can be checked without djvulibre installed.
    pub fn with_djvu(mut self, bind: BindDjvu) -> RoutingBackend {
        self.bind_djvu = bind;
        self
    }

    /// Has a PDF backend been bound yet? Only diagnostics and tests ask.
    pub fn pdf_is_bound(&self) -> bool {
        self.pdf.is_some()
    }

    /// Has a DjVu backend been bound yet? Only diagnostics and tests ask.
    pub fn djvu_is_bound(&self) -> bool {
        self.djvu.is_some()
    }

    fn route(&self, document: BackendDocumentId) -> Result<Route> {
        self.routes
            .get(&document.0)
            .copied()
            .ok_or_else(|| PdfError::Render("unknown document".into()))
    }

    /// The backend one document lives in, and its id inside that backend.
    fn resolve(&self, document: BackendDocumentId) -> Result<(&dyn PdfBackend, BackendDocumentId)> {
        match self.route(document)? {
            Route::Images(inner) => Ok((&self.images, inner)),
            Route::Pdf(inner) => match self.pdf.as_deref() {
                Some(backend) => Ok((backend, inner)),
                None => Err(PdfError::Unavailable(
                    "no PDF backend is bound in this worker".into(),
                )),
            },
            Route::Djvu(inner) => match self.djvu.as_deref() {
                Some(backend) => Ok((backend, inner)),
                None => Err(PdfError::Unavailable(
                    "no DjVu backend is bound in this worker".into(),
                )),
            },
        }
    }
}

impl PdfBackend for RoutingBackend {
    fn name(&self) -> &'static str {
        "images+pdf+djvu"
    }

    fn version(&self) -> String {
        let pdf = match self.pdf.as_deref() {
            Some(pdf) => pdf.version(),
            None => "PDF backend not bound yet".to_string(),
        };
        let djvu = match self.djvu.as_deref() {
            Some(djvu) => djvu.version(),
            None => "DjVu backend not bound yet".to_string(),
        };
        format!("{} / {pdf} / {djvu}", self.images.version())
    }

    fn open(&mut self, source: &Path) -> Result<BackendDocumentId> {
        self.next_id += 1;
        let outer = BackendDocumentId(self.next_id);
        // A directory source — or a bare image file, which resolves to one —
        // is an image document; a `.djvu` goes to djvulibre; everything else
        // is a PDF (§45.2, §56.1).
        let route = if resolve_source(source).is_some() {
            Route::Images(self.images.open(source)?)
        } else if crate::djvu::is_djvu(source) {
            if self.djvu.is_none() {
                self.djvu = Some((self.bind_djvu)()?);
            }
            let djvu = self.djvu.as_mut().expect("just bound");
            Route::Djvu(djvu.open(source)?)
        } else {
            if self.pdf.is_none() {
                self.pdf = Some((self.bind)()?);
            }
            let pdf = self.pdf.as_mut().expect("just bound");
            Route::Pdf(pdf.open(source)?)
        };
        self.routes.insert(outer.0, route);
        Ok(outer)
    }

    fn close(&mut self, document: BackendDocumentId) {
        match self.routes.remove(&document.0) {
            Some(Route::Images(inner)) => self.images.close(inner),
            Some(Route::Pdf(inner)) => {
                if let Some(pdf) = self.pdf.as_mut() {
                    pdf.close(inner);
                }
            }
            Some(Route::Djvu(inner)) => {
                if let Some(djvu) = self.djvu.as_mut() {
                    djvu.close(inner);
                }
            }
            None => {}
        }
    }

    fn metadata(&self, document: BackendDocumentId) -> Result<DocumentMetadata> {
        let (backend, inner) = self.resolve(document)?;
        backend.metadata(inner)
    }

    fn page_size(&self, document: BackendDocumentId, page: usize) -> Result<PageSize> {
        let (backend, inner) = self.resolve(document)?;
        backend.page_size(inner, page)
    }

    fn render(&self, request: &RenderRequest, cancel: &dyn CancelSignal) -> Result<RenderedPage> {
        let (backend, inner) = self.resolve(request.document)?;
        backend.render(&rewritten(request, inner), cancel)
    }

    fn render_into(
        &self,
        request: &RenderRequest,
        target: &mut [u8],
        cancel: &dyn CancelSignal,
    ) -> Result<()> {
        let (backend, inner) = self.resolve(request.document)?;
        backend.render_into(&rewritten(request, inner), target, cancel)
    }

    fn links(&self, document: BackendDocumentId, page: usize) -> Result<Vec<PageLink>> {
        let (backend, inner) = self.resolve(document)?;
        backend.links(inner, page)
    }

    fn find_text(
        &self,
        document: BackendDocumentId,
        query: &pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) -> Result<Vec<pulpit_core::search::Hit>> {
        let (backend, inner) = self.resolve(document)?;
        backend.find_text(inner, query, pages)
    }

    fn attachment(&self, document: BackendDocumentId, name: &str) -> Result<Vec<u8>> {
        let (backend, inner) = self.resolve(document)?;
        backend.attachment(inner, name)
    }

    fn attachment_names(&self, document: BackendDocumentId) -> Result<Vec<String>> {
        let (backend, inner) = self.resolve(document)?;
        backend.attachment_names(inner)
    }

    fn page_labels(&self, document: BackendDocumentId) -> Result<pulpit_core::overlay::PageLabels> {
        let (backend, inner) = self.resolve(document)?;
        backend.page_labels(inner)
    }

    fn outline(&self, document: BackendDocumentId) -> Result<Outline> {
        let (backend, inner) = self.resolve(document)?;
        backend.outline(inner)
    }

    fn evidence(&self, document: BackendDocumentId) -> Result<DocumentEvidence> {
        let (backend, inner) = self.resolve(document)?;
        backend.evidence(inner)
    }
}

/// The same request, addressed to the inner backend's own document id.
fn rewritten(request: &RenderRequest, document: BackendDocumentId) -> RenderRequest {
    RenderRequest {
        document,
        ..request.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::fixture::FixtureBackend;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn router(binds: Arc<AtomicUsize>) -> RoutingBackend {
        RoutingBackend::new(Box::new(move || {
            binds.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FixtureBackend::new()) as Box<dyn PdfBackend>)
        }))
    }

    fn write_png(path: &Path) {
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save(path)
            .unwrap();
    }

    /// §45.4: binding is attempted on the first PDF open, not at startup, so
    /// a worker that only ever sees pictures never needs a PDF library.
    #[test]
    fn a_worker_that_only_opens_images_never_binds_a_pdf_backend() {
        let binds = Arc::new(AtomicUsize::new(0));
        let mut router = router(Arc::clone(&binds));
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"));

        let document = router.open(dir.path()).unwrap();
        assert_eq!(router.metadata(document).unwrap().page_count, 1);
        assert_eq!(binds.load(Ordering::SeqCst), 0);
        assert!(!router.pdf_is_bound());
    }

    #[test]
    fn a_pdf_open_binds_the_pdf_backend_once() {
        let binds = Arc::new(AtomicUsize::new(0));
        let mut router = router(Arc::clone(&binds));
        router.open(Path::new("fixture:pages=3")).unwrap();
        router.open(Path::new("fixture:pages=4")).unwrap();
        assert_eq!(binds.load(Ordering::SeqCst), 1, "bound once, reused after");
        assert!(router.pdf_is_bound());
    }

    /// §45.1: a reload holds two documents at once, and after this change
    /// they need not be the same kind.
    #[test]
    fn one_worker_holds_a_deck_and_a_folder_at_the_same_time() {
        let binds = Arc::new(AtomicUsize::new(0));
        let mut router = router(binds);
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"));
        write_png(&dir.path().join("b.png"));

        let deck = router.open(Path::new("fixture:pages=7")).unwrap();
        let folder = router.open(dir.path()).unwrap();
        assert_ne!(deck, folder, "the router hands out its own ids");
        assert_eq!(router.metadata(deck).unwrap().page_count, 7);
        assert_eq!(router.metadata(folder).unwrap().page_count, 2);

        // And each renders through its own backend.
        let request = |document, page| RenderRequest {
            document,
            page,
            region: pulpit_core::notes::Region::FULL,
            width: 8,
            height: 8,
            with_annotations: false,
            full_size: None,
        };
        assert!(router
            .render(&request(deck, 6), &crate::pdf::NeverCancel)
            .is_ok());
        let picture = router
            .render(&request(folder, 1), &crate::pdf::NeverCancel)
            .unwrap();
        assert_eq!(&picture.pixels[..4], &[1, 2, 3, 255]);

        router.close(folder);
        assert!(router.metadata(folder).is_err());
        assert_eq!(router.metadata(deck).unwrap().page_count, 7, "unaffected");
    }

    #[test]
    fn a_pdf_open_that_cannot_bind_fails_that_open_and_nothing_else() {
        let mut router = RoutingBackend::new(Box::new(|| {
            Err(PdfError::Unavailable("no libpdfium here".into()))
        }));
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"));

        assert!(router.open(Path::new("/decks/talk.pdf")).is_err());
        let folder = router.open(dir.path()).unwrap();
        assert_eq!(router.metadata(folder).unwrap().page_count, 1);
    }

    /// A router whose DjVu route is a stand-in, so the routing itself can be
    /// checked on a machine with no djvulibre — which is most of them.
    fn router_with_djvu(binds: Arc<AtomicUsize>) -> RoutingBackend {
        RoutingBackend::new(Box::new(|| Ok(Box::new(FixtureBackend::new())))).with_djvu(Box::new(
            move || {
                binds.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FixtureBackend::new()) as Box<dyn PdfBackend>)
            },
        ))
    }

    /// §56.1: both extensions reach the DjVu route, and nothing else does.
    #[test]
    fn a_djvu_routes_to_the_djvu_backend_and_a_pdf_does_not() {
        let binds = Arc::new(AtomicUsize::new(0));
        let mut router = router_with_djvu(Arc::clone(&binds));

        router.open(Path::new("fixture:pages=2")).unwrap();
        assert_eq!(
            binds.load(Ordering::SeqCst),
            0,
            "a PDF binds no DjVu library"
        );
        assert!(!router.djvu_is_bound());

        router.open(Path::new("/books/scan.djvu")).unwrap();
        router.open(Path::new("/books/other.djv")).unwrap();
        assert_eq!(binds.load(Ordering::SeqCst), 1, "bound once, reused after");
        assert!(router.djvu_is_bound());
    }

    /// §56.1 and §65.2, in both directions: a format's absence must never
    /// break another format. This is the whole reason binding is per-route.
    #[test]
    fn a_missing_djvu_library_does_not_stop_a_pdf_or_an_image_opening() {
        let mut router = RoutingBackend::new(Box::new(|| Ok(Box::new(FixtureBackend::new()))))
            .with_djvu(Box::new(|| {
                Err(PdfError::Unavailable("no djvulibre here".into()))
            }));
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"));

        assert!(router.open(Path::new("/books/scan.djvu")).is_err());
        let deck = router.open(Path::new("fixture:pages=5")).unwrap();
        let folder = router.open(dir.path()).unwrap();
        assert_eq!(router.metadata(deck).unwrap().page_count, 5);
        assert_eq!(router.metadata(folder).unwrap().page_count, 1);

        // And a second DjVu still fails on its own rather than poisoning the
        // worker: the failed bind was not cached as a bound backend.
        assert!(router.open(Path::new("/books/again.djvu")).is_err());
        assert_eq!(router.metadata(deck).unwrap().page_count, 5);
    }

    #[test]
    fn a_missing_pdf_library_does_not_stop_a_djvu_opening() {
        let binds = Arc::new(AtomicUsize::new(0));
        let mut router = RoutingBackend::new(Box::new(|| {
            Err(PdfError::Unavailable("no libpdfium".into()))
        }))
        .with_djvu(Box::new(move || {
            binds.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FixtureBackend::new()) as Box<dyn PdfBackend>)
        }));

        assert!(router.open(Path::new("/decks/talk.pdf")).is_err());
        let book = router.open(Path::new("/books/scan.djvu")).unwrap();
        assert!(router.metadata(book).unwrap().page_count > 0);
    }

    /// Three kinds at once, each closed independently — the reload path holds
    /// two documents by construction and they need not be the same kind.
    #[test]
    fn one_worker_holds_a_deck_a_book_and_a_folder() {
        let mut router = router_with_djvu(Arc::new(AtomicUsize::new(0)));
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"));

        let deck = router.open(Path::new("fixture:pages=7")).unwrap();
        let book = router.open(Path::new("/books/scan.djvu")).unwrap();
        let folder = router.open(dir.path()).unwrap();
        assert_eq!(router.metadata(deck).unwrap().page_count, 7);
        assert_eq!(router.metadata(folder).unwrap().page_count, 1);

        router.close(book);
        assert!(router.metadata(book).is_err());
        assert_eq!(router.metadata(deck).unwrap().page_count, 7, "unaffected");
        assert_eq!(router.metadata(folder).unwrap().page_count, 1, "unaffected");
    }
}
