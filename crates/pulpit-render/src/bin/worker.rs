//! The renderer worker executable.
//!
//! Spawned by the supervisor, one per worker slot. A worker that cannot bind
//! PDFium prints the diagnostic and exits: PDFium ships with every supported
//! package, so failing to find it is a broken installation, and rendering
//! placeholders in its place would put something on the projector that is not
//! the presenter's deck. The backend it got is still reported in the
//! handshake, for diagnostics.
//!
//! Since `SPEC-images.md` §45 that exit happens on the first **PDF** open
//! rather than at startup: a worker holds several documents at a time and
//! they need not be the same kind, and refusing to display a JPEG because a
//! PDF library is absent is not defensible. The diagnostic is unchanged; only
//! its timing moved.

use std::io::{stdin, stdout};

use pulpit_render::pdf::PdfBackend;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PULPIT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let backend: Box<dyn PdfBackend> = select_backend();
    tracing::info!(backend = backend.name(), "renderer worker starting");

    match pulpit_render::worker::run(stdin(), stdout(), backend) {
        Ok(()) => {}
        Err(e) => {
            tracing::error!(error = %e, "renderer worker exiting");
            std::process::exit(1);
        }
    }
}

/// The real backend, wrapped in failure injection for CI.
///
/// The failure injection wraps the router rather than sitting under it: it is
/// keyed by page number, and which backend answers that page is not its
/// business.
fn select_backend() -> Box<dyn PdfBackend> {
    Box::new(failure_injection(pulpit_render::worker::default_backend()))
}

/// Failure injection for CI: `PULPIT_WORKER_CRASH_ON_PAGE=N` aborts the
/// process while rendering page `N`, and `..._HANG_ON_PAGE=N` sleeps forever.
/// Both exercise supervisor recovery paths that are otherwise only reachable
/// with a genuinely malformed PDF.
fn failure_injection(inner: Box<dyn PdfBackend>) -> FailureInjectingBackend {
    FailureInjectingBackend {
        crash_on: read_page("PULPIT_WORKER_CRASH_ON_PAGE"),
        hang_on: read_page("PULPIT_WORKER_HANG_ON_PAGE"),
        inner,
    }
}

fn read_page(variable: &str) -> Option<usize> {
    std::env::var(variable)
        .ok()
        .and_then(|value| value.parse().ok())
}

struct FailureInjectingBackend {
    crash_on: Option<usize>,
    hang_on: Option<usize>,
    inner: Box<dyn PdfBackend>,
}

impl PdfBackend for FailureInjectingBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn version(&self) -> String {
        self.inner.version()
    }

    fn open(
        &mut self,
        source: &std::path::Path,
    ) -> pulpit_render::pdf::Result<pulpit_render::pdf::BackendDocumentId> {
        self.inner.open(source)
    }

    fn close(&mut self, document: pulpit_render::pdf::BackendDocumentId) {
        self.inner.close(document)
    }

    fn metadata(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
    ) -> pulpit_render::pdf::Result<pulpit_render::pdf::DocumentMetadata> {
        self.inner.metadata(document)
    }

    fn page_count(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
    ) -> pulpit_render::pdf::Result<usize> {
        self.inner.page_count(document)
    }

    fn page_size(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
        page: usize,
    ) -> pulpit_render::pdf::Result<pulpit_core::PageSize> {
        self.inner.page_size(document, page)
    }

    fn render(
        &self,
        request: &pulpit_render::pdf::RenderRequest,
        cancel: &dyn pulpit_render::pdf::CancelSignal,
    ) -> pulpit_render::pdf::Result<pulpit_render::pdf::RenderedPage> {
        if self.crash_on == Some(request.page) {
            tracing::error!(page = request.page, "failure injection: aborting");
            std::process::abort();
        }
        if self.hang_on == Some(request.page) {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        self.inner.render(request, cancel)
    }

    fn render_into(
        &self,
        request: &pulpit_render::pdf::RenderRequest,
        target: &mut [u8],
        cancel: &dyn pulpit_render::pdf::CancelSignal,
    ) -> pulpit_render::pdf::Result<()> {
        if self.crash_on == Some(request.page) {
            tracing::error!(page = request.page, "failure injection: aborting");
            std::process::abort();
        }
        if self.hang_on == Some(request.page) {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        self.inner.render_into(request, target, cancel)
    }

    fn links(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
        page: usize,
    ) -> pulpit_render::pdf::Result<Vec<pulpit_core::PageLink>> {
        self.inner.links(document, page)
    }

    fn find_text(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
        query: &pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) -> pulpit_render::pdf::Result<Vec<pulpit_core::search::Hit>> {
        self.inner.find_text(document, query, pages)
    }

    fn attachment(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
        name: &str,
    ) -> pulpit_render::pdf::Result<Vec<u8>> {
        self.inner.attachment(document, name)
    }

    fn attachment_names(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
    ) -> pulpit_render::pdf::Result<Vec<String>> {
        self.inner.attachment_names(document)
    }

    fn page_labels(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
    ) -> pulpit_render::pdf::Result<pulpit_core::overlay::PageLabels> {
        self.inner.page_labels(document)
    }

    fn outline(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
    ) -> pulpit_render::pdf::Result<pulpit_core::navigation::Outline> {
        self.inner.outline(document)
    }

    fn evidence(
        &self,
        document: pulpit_render::pdf::BackendDocumentId,
    ) -> pulpit_render::pdf::Result<pulpit_render::pdf::capabilities::DocumentEvidence> {
        self.inner.evidence(document)
    }
}
