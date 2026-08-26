//! DjVu, the first Class B format (`SPEC-reader-formats.md` §55, §56).
//!
//! Class B means paginated behind a native library: pages exist, have fixed
//! sizes and render independently, so DjVu fits [`crate::pdf::PdfBackend`]
//! without touching it (§55.1). The architecture was never what blocked it —
//! the cost is a library on the machine, and §55.3 answers that by
//! **discovering an installed djvulibre and never bundling one**, the same
//! way `pulpit-media` prefers an installed browser over shipping one.
//!
//! DjVu is the format §55.5 names as the one most likely to be worth doing:
//! scanned books are exactly the case document mode was built for.
//!
//! What lives where:
//!
//! * [`discover`] — where an installed djvulibre might be, and why looking
//!   only at the loader path is not enough (§55.4).
//! * [`sys`] — the `dlopen`'d slice of `ddjvuapi`.
//! * [`backend`] — the renderer's view.
//! * [`document`] — the reader's view, where every PDF semantic reports
//!   `Unsupported` (§60.1).
//! * [`text`] — the hidden text layer, which is what makes a scanned book
//!   searchable (§59.2).
//!
//! [`is_djvu`] and [`missing_djvu_message`] are outside the feature gate on
//! purpose. A build compiled without DjVu support must still *recognise* a
//! `.djvu` and refuse it by name, saying what would be needed — §61.1 and
//! §61.2 make that a requirement rather than a nicety, and a build that could
//! not name the format would report a DjVu book as a damaged PDF.
//!
//! Not here, deliberately: annotations and any other mutation (§60.3 — a
//! per-format sidecar is the one thing `SPEC-document.md` refuses).

use std::path::Path;

#[cfg(feature = "djvu")]
pub mod backend;
#[cfg(feature = "djvu")]
pub mod discover;
#[cfg(feature = "djvu")]
pub mod document;
pub mod sys;
#[cfg(feature = "djvu")]
pub(crate) mod text;

#[cfg(feature = "djvu")]
pub use backend::DjvuBackend;
#[cfg(feature = "djvu")]
pub use document::DjvuDocument;

/// Every extension a DjVu document may be opened from.
///
/// **The one definition.** The router's predicate, the document worker's
/// dispatch and the file-dialog filters all read this rather than restating
/// it, for the reason `SPEC-images.md` §41.5 gives: hand-maintained copies
/// drift, and the symptom — a format the backend reads but the picker will
/// not offer — is invisible until somebody tries.
pub const DJVU_EXTENSIONS: &[&str] = &["djvu", "djv"];

/// Is this a file the DjVu backend should be given?
///
/// Extension alone, like the image tier's listing rule (`SPEC-images.md`
/// §41.1): routing must not depend on whether a file happens to be readable
/// at the instant it is routed. Content sniffing is for *refusal* messages,
/// where naming the format correctly is worth reading a few bytes (§61.3).
pub fn is_djvu(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            DJVU_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// What a worker prints, or hands back, when a DjVu opens on a machine with
/// no djvulibre (§55.3, §61.1).
///
/// Deliberately not shaped like [`crate::pdf::missing_pdfium_message`]: that
/// one calls a missing PDFium a broken installation, because every package
/// ships it. This is a normal state on a normal machine, so this message says
/// how to change it rather than what went wrong.
///
/// One sentence, and it is the fix — "installing djvulibre would open this"
/// says the library is missing without a sentence spent saying so. No install table, and nothing the
/// caller has already said — this is shown under a heading that names the
/// file that would not open and inside an error that names the backend, so
/// repeating either here is one sentence of noise in front of somebody who is
/// about to give a talk. The install instructions are in the manual, and
/// `reason` — the loader's own account of the names it tried — goes to the log,
/// where a library that is installed but not found is diagnosed.
pub fn missing_djvu_message(reason: &str) -> String {
    tracing::debug!(%reason, "no djvulibre found");
    "Install djvulibre to open this DjVu file.".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_djvu_extensions_route_and_nothing_else_does() {
        assert!(is_djvu(Path::new("/books/scan.djvu")));
        assert!(is_djvu(Path::new("/books/scan.djv")));
        assert!(is_djvu(Path::new("/books/SCAN.DjVu")), "case-insensitive");
        assert!(!is_djvu(Path::new("/decks/talk.pdf")));
        assert!(!is_djvu(Path::new("/photos/a.png")));
        assert!(!is_djvu(Path::new("/books/djvu")), "not an extension");
    }

    /// §61.1 and §61.2: the refusal names the format and what would install
    /// it, never suggests the file is damaged, and stays short enough to read
    /// at a glance — the loader's diagnostic belongs in the log, not in front
    /// of somebody about to give a talk.
    #[test]
    fn the_refusal_names_djvu_and_does_not_call_the_file_broken() {
        let message = missing_djvu_message("libdjvulibre.so.21: not found");
        assert!(message.contains("DjVu"));
        assert!(message.contains("djvulibre"));
        assert!(
            !message.contains("libdjvulibre.so.21"),
            "the loader's account of what it tried is a log line: {message}"
        );
        assert!(
            message.lines().count() == 1 && message.len() < 80,
            "short enough to read at a glance, and it repeats nothing the \
             caller already said: {message}"
        );
        for wrong in ["damaged", "corrupt", "invalid"] {
            assert!(
                !message.contains(wrong),
                "a missing library is not a broken file"
            );
        }
    }
}
