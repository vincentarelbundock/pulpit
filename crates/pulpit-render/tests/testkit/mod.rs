//! Shared machinery for pulpit's document tests: fixtures to build, corpora
//! to break things with, and independent PDF implementations to check the
//! results against.
//!
//! This is test-only code. It lives under `tests/` rather than in the library
//! so that nothing here can be compiled into the shipped crate or enabled by
//! anything that depends on it: `SPEC-document.md` §5.1 asks for the corpus
//! to be unable to ship, and a directory the library never names is a firmer
//! guarantee of that than a feature flag would be.
//!
//! Every integration test binary declares `mod testkit;` and takes what it
//! needs, so most of what is here is unused in any one of them.
#![allow(dead_code, unused_imports)]

/// Assembling small PDFs byte by byte.
///
/// The corpus needs documents that are wrong in one specific, chosen way. No
/// PDF producer will emit those on request, so the tests write the file
/// format directly: an object per entry, a cross-reference table counted from
/// the actual byte offsets, and a trailer pointing at object 1 as the catalog.
///
/// The file carries no `//!` header of its own because two unit tests inside
/// `src/` reach it with `include!`, which cannot take one.
pub mod builder;
pub mod corpus;
pub mod guard;
pub mod mutate;
/// One thread for every PDFium form-fill call in a test binary.
///
/// The pinned PDFium is a V8 build. PDFium creates its V8 isolate lazily —
/// `FORM_OnAfterLoadPage` is what triggers it, so *any* form-fill work does,
/// not only a document that carries scripts — and the isolate belongs to the
/// thread that created it. Touching it from a second thread is a segmentation
/// fault inside V8's snapshot deserialiser: no error return, no unwind, the
/// whole test binary dies and takes the results of the tests that already
/// passed with it.
///
/// libtest gives every `#[test]` its own thread, including under
/// `--test-threads=1`, so a suite with two form-fill tests in it crashes
/// however it is invoked. That is a property of the harness rather than of
/// pulpit: the document worker's `serve` loop is a synchronous
/// read-handle-write on the worker process's main thread, so a running pulpit
/// only ever calls PDFium from one thread.
///
/// This restores that property for tests. Bodies are handed to one long-lived
/// thread and run there, one at a time, in the order they arrive.
///
/// The file carries no `//!` header of its own because the one lib test that
/// binds PDFium reaches it with `include!`, which cannot take one.
pub mod pdfium_thread;

pub mod verify;

pub use builder::{stream_body, utf16_string, Page, Pdf};
pub use corpus::{corpus, Case, Expect};
pub use guard::{nothing_written, Unchanged};
pub use pdfium_thread::on_the_pdfium_thread;
pub use verify::Engines;

use std::path::{Path, PathBuf};

/// Write `bytes` into `directory` as `name.pdf` and return the path.
pub fn write_pdf(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(format!("{name}.pdf"));
    std::fs::write(&path, bytes)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", path.display()));
    path
}

/// How many cases a property test should run.
///
/// The default is small on purpose: the whole suite is meant to stay fast
/// enough to run on every save, and these properties do real work per case —
/// a PDF written, filled, exported, and reopened. Continuous integration
/// should raise it, which is what `PROPTEST_CASES` is for.
pub fn property_cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
