//! Presenting a folder of images as a paginated, read-only document.
//!
//! `SPEC-images.md`. The directory takes the role the PDF file takes, each
//! image is a page, and the existing overview grid becomes a contact sheet
//! for the folder without a line of its own code (§50.1).
//!
//! The tier is deliberately one tier: **raster images decoded in-process by
//! the `image` crate**. PostScript, DjVu, DVI, XPS and the reflowable formats
//! are out of scope and are not deferred features of this design — they need
//! a different page model or a bundled native library, and §52 records why.
//!
//! `SPEC-reader-formats.md` §54 adds one more source without changing any of
//! that: a `.cbz` or `.cbt` comic archive is a directory that happens to be
//! one file, so it produces the same page table and everything above it is
//! untouched.
//!
//! What lives where:
//!
//! * [`archive`] — reading `.cbz` and `.cbt`, the bounds that stop a zip bomb,
//!   and the named refusals for the formats pulpit does not read.
//! * [`table`] — the ordered page table, the extension set, the natural sort,
//!   the source digest and the name-anchored re-indexing. Pure apart from one
//!   `read_dir`, which is what lets the application and the worker derive the
//!   *same* table independently (§42).
//! * [`decode`] — header-only dimension reads, bounded decoding and the
//!   worker's byte-bounded cache of decoded images (§46.1, §47).
//! * [`backend`] — the renderer's [`crate::pdf::PdfBackend`] view.
//! * [`document`] — the reader's [`crate::document::DocumentBackend`] view,
//!   where everything a PDF can do and a folder cannot reports `Unsupported`
//!   (§48).

pub mod archive;
pub mod backend;
pub mod decode;
pub mod document;
pub mod table;

pub use archive::{ArchiveKind, ARCHIVE_EXTENSIONS};
pub use backend::ImageBackend;
pub use decode::{DecodedCache, ImageFailure};
pub use document::ImageDocument;
pub use table::{
    directory_stamp, is_supported_image, list_directory, list_source, openable_extensions, reindex,
    resolve_source, ImageEntry, ListError, PageLocation, PageSource, PageTable, ResolvedSource,
    IMAGE_EXTENSIONS,
};
