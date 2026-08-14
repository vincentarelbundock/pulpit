//! The renderer worker executable.
//!
//! Spawned by the supervisor, one per worker slot. It binds PDFium or exits:
//! PDFium ships with every supported package, so failing to find it is a
//! broken installation, and rendering placeholders in its place would put
//! something on the projector that is not the presenter's deck. The backend
//! it got is still reported in the handshake, for diagnostics.

use std::io::{stdin, stdout};

use pulpit_render::pdf::fixture::FixtureBackend;
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

fn select_backend() -> Box<dyn PdfBackend> {
    if std::env::var_os("PULPIT_FORCE_FIXTURE_BACKEND").is_some() {
        return Box::new(failure_injection(Box::new(FixtureBackend::new())));
    }
    #[cfg(feature = "pdfium")]
    {
        match pulpit_render::pdf::pdfium::PdfiumBackend::bind() {
            Ok(backend) => Box::new(backend),
            Err(e) => fail_without_pdfium(&e.to_string()),
        }
    }
    #[cfg(not(feature = "pdfium"))]
    fail_without_pdfium("this build was compiled without the pdfium feature");
}

/// PDFium is a hard requirement: every supported package installs it, and a
/// worker that cannot bind it exits rather than rendering placeholder pages
/// that the presenter might mistake for their deck. The fixture backend
/// remains reachable, but only when a test asks for it by name.
fn fail_without_pdfium(reason: &str) -> ! {
    eprintln!("{}", pulpit_render::pdf::missing_pdfium_message(reason));
    std::process::exit(1);
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
}
