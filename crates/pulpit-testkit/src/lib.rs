//! Shared machinery for pulpit's document tests: fixtures to build, corpora
//! to break things with, and independent PDF implementations to check the
//! results against.
//!
//! This crate is a development dependency. Nothing in the shipped application
//! links against it.

pub mod builder;
pub mod corpus;
pub mod guard;
pub mod mutate;
pub mod verify;

pub use builder::{stream_body, utf16_string, Page, Pdf};
pub use corpus::{corpus, Case, Expect};
pub use guard::{nothing_written, Unchanged};
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
