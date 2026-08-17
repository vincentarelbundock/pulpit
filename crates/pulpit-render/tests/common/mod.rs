//! What every PDFium integration test in this crate needs before it can
//! begin: where the workspace is, and one bound copy of the library.
//!
//! PDFium is a process-wide singleton, so a test binary may bind it exactly
//! once and every test in that binary shares the binding. Each integration
//! test is a separate executable, so the `OnceLock` below is per binary
//! rather than per workspace; centralizing it here removes ten copies of the
//! same discovery, locking and skip-message code without pretending the
//! binding itself is shared across processes.
//!
//! This module only finds and locks the backend. Tests that reach PDFium's
//! V8 form environment still go through `pulpit_testkit::on_the_pdfium_thread`,
//! which is a different boundary and stays where it is.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pulpit_render::pdf::pdfium::PdfiumBackend;

/// The workspace root, from which `lib/` and `examples/` hang.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The binary's PDFium backend, locked, or `None` with a message naming what
/// is being skipped.
///
/// `skip_context` is the caller's own description — "the AcroForm corpus",
/// "the cross-viewer tests" — so a green run on a machine without
/// `libpdfium` still says which meaningful tests it did not run.
///
/// A panicking test poisons the mutex; recovering from that keeps one real
/// failure from cascading into five that say only `PoisonError`.
pub fn pdfium(skip_context: &str) -> Option<MutexGuard<'static, PdfiumBackend>> {
    static BACKEND: OnceLock<Option<Mutex<PdfiumBackend>>> = OnceLock::new();
    let backend = BACKEND
        .get_or_init(|| {
            if std::env::var_os("PULPIT_PDFIUM_PATH").is_none() {
                std::env::set_var("PULPIT_PDFIUM_PATH", workspace_root().join("lib"));
            }
            match PdfiumBackend::bind() {
                Ok(backend) => Some(Mutex::new(backend)),
                Err(error) => {
                    eprintln!("skipping {skip_context}: {error}");
                    None
                }
            }
        })
        .as_ref()?;
    Some(
        backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}
