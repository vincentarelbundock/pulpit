//! The formats pulpit deliberately does not read, and what it says about each.
//!
//! `SPEC-reader-formats.md` §61. Every format named in §64 that pulpit does
//! not open is listed here with its own message, so that a presenter who hands
//! over a `.ps` or an `.epub` is told **what the file is and what to do about
//! it** rather than being told a perfectly good file is damaged.
//!
//! Three rules shape the table:
//!
//! * **By name** (§61.1). The message says which format it is and what would
//!   be needed — a library, a conversion, or nothing, because the answer is
//!   "not planned".
//! * **Never "corrupt"** (§61.2). "pulpit cannot read this kind of file" and
//!   "this file is damaged" are different facts, and telling a presenter the
//!   second when the first is true sends them looking for a problem that does
//!   not exist. This is the whole reason the table exists: without it these
//!   extensions fall through to the PDF backend and fail as broken PDFs.
//! * **Before any library is bound** (§65.2). Refusing a format must not
//!   require PDFium, djvulibre or anything else to be present first, so this
//!   is consulted at the top of every open path.
//!
//! Extension only, like every other listing decision (§41.1). §61.3 permits
//! sniffing content to name a format better and this module does not do it
//! yet; an extension is right often enough that the refusal is honest, and a
//! wrong guess here costs a wrong name in a message rather than a wrong render.

use std::path::Path;

/// A format pulpit refuses, the extensions that name it, and what to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedFormat {
    /// The extensions that name this format, lowercase, without the dot.
    pub extensions: &'static [&'static str],
    /// What the format is called, for a diagnostic that groups by format
    /// rather than by extension.
    pub name: &'static str,
    /// The refusal itself: what it is, why not, and the way forward.
    pub message: &'static str,
}

/// Every format pulpit names and refuses (§64).
///
/// The messages are written out rather than composed from a template on
/// purpose: each one ends with a different way forward — repack it, convert
/// it, install nothing because the answer is no — and a template would flatten
/// exactly the part the presenter needs.
pub const UNSUPPORTED_FORMATS: &[UnsupportedFormat] = &[
    UnsupportedFormat {
        extensions: &["cbr"],
        name: "RAR comic archive",
        message: "RAR archives (.cbr) are not supported. RAR needs the unrar library, \
                  whose licence this project cannot carry. Repacking the comic as a \
                  .cbz — a plain zip of the same images — opens here.",
    },
    UnsupportedFormat {
        extensions: &["cb7"],
        name: "7z comic archive",
        message: "7z archives (.cb7) are not supported yet. Repacking the comic as a \
                  .cbz — a plain zip of the same images — opens here.",
    },
    UnsupportedFormat {
        extensions: &["ps", "eps"],
        name: "PostScript",
        message: "PostScript (.ps) is not supported. Rendering it would need Ghostscript, \
                  which pulpit does not bundle. Nearly every PostScript file converts \
                  cleanly — ps2pdf talk.ps — and the PDF opens here.",
    },
    UnsupportedFormat {
        extensions: &["xps", "oxps"],
        name: "XPS",
        message: "XPS (.xps) is not supported. Whatever produced it can almost certainly \
                  export a PDF instead, and that opens here.",
    },
    UnsupportedFormat {
        extensions: &["dvi"],
        name: "DVI",
        message: "DVI (.dvi) is not supported: drawing one needs the fonts from a TeX \
                  installation. The TeX run that produced this file can produce a PDF \
                  instead — dvipdfmx paper.dvi — and that opens here.",
    },
    UnsupportedFormat {
        extensions: &["epub"],
        name: "EPUB",
        message: "EPUB (.epub) is not supported. A reflowable book has no fixed pages \
                  until a width is chosen, and the presenter and audience windows are \
                  different sizes, so \"page 7\" would name different text on each. \
                  Converting the book to PDF fixes the pages, and that opens here.",
    },
    UnsupportedFormat {
        extensions: &["mobi", "azw", "azw3", "prc"],
        name: "Mobipocket",
        message: "Mobipocket books (.mobi, .azw) are not supported. A reflowable book \
                  has no fixed pages until a width is chosen, and pulpit shows the same \
                  page on both displays. Converting the book to PDF opens here.",
    },
    UnsupportedFormat {
        extensions: &["fb2"],
        name: "FictionBook",
        message: "FictionBook (.fb2) is not supported. A reflowable book has no fixed \
                  pages until a width is chosen, and pulpit shows the same page on both \
                  displays. Converting the book to PDF opens here.",
    },
    UnsupportedFormat {
        extensions: &["chm"],
        name: "CHM help archive",
        message: "CHM help files (.chm) are not supported. They are reflowable HTML with \
                  no fixed pages, and pulpit shows the same page on both displays. \
                  Printing the pages you need to PDF opens here.",
    },
    UnsupportedFormat {
        extensions: &["odt"],
        name: "OpenDocument text",
        message: "OpenDocument text (.odt) is not supported: laying it out again would \
                  not match what its own editor shows. Export a PDF from the editor — \
                  every one of them does — and that opens here.",
    },
];

/// Is this a format pulpit refuses to read, and what should it say?
///
/// `None` means only "not on this list": a PDF, an image, a DjVu and an
/// unrecognised file all answer `None` and are settled further along.
pub fn unsupported_format(path: &Path) -> Option<&'static str> {
    format_of(path).map(|format| format.message)
}

/// The table entry for a path, for callers that want the format's name as
/// well as the message.
pub fn format_of(path: &Path) -> Option<&'static UnsupportedFormat> {
    let extension = path.extension().and_then(|e| e.to_str())?;
    UNSUPPORTED_FORMATS.iter().find(|format| {
        format
            .extensions
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §61.1 and §61.2 as one table-driven check, over every extension in the
    /// table: each is refused, the refusal names the format, and none of them
    /// reads as a damaged file.
    #[test]
    fn every_refused_extension_is_named_and_never_called_damaged() {
        for format in UNSUPPORTED_FORMATS {
            for extension in format.extensions {
                let path = std::path::PathBuf::from(format!("/talks/thing.{extension}"));
                let message = unsupported_format(&path)
                    .unwrap_or_else(|| panic!(".{extension} must be refused by name (§61.1)"));
                assert!(
                    message.contains(&format!(".{extension}"))
                        || message.contains(format.name)
                        || format
                            .extensions
                            .iter()
                            .any(|other| message.contains(&format!(".{other}"))),
                    ".{extension}: {message}"
                );
                let lower = message.to_lowercase();
                for wrong in ["corrupt", "damaged", "malformed", "invalid"] {
                    assert!(
                        !lower.contains(wrong),
                        ".{extension} says {wrong}: {message}"
                    );
                }
            }
        }
    }

    /// A refusal is only worth reading if it says what to do next, so every
    /// message ends by naming the way forward.
    #[test]
    fn every_refusal_offers_a_way_forward() {
        for format in UNSUPPORTED_FORMATS {
            let lower = format.message.to_lowercase();
            assert!(
                lower.contains("opens here"),
                "{}: {}",
                format.name,
                format.message
            );
        }
    }

    #[test]
    fn extensions_are_matched_whatever_their_case() {
        assert!(unsupported_format(Path::new("/comics/book.CBR")).is_some());
        assert!(unsupported_format(Path::new("/talks/slides.PS")).is_some());
        assert!(unsupported_format(Path::new("/books/novel.EPUB")).is_some());
    }

    /// The formats pulpit does read are not in the table, and neither is a
    /// file with no extension at all — those are settled by the router, not
    /// refused here.
    #[test]
    fn what_pulpit_reads_is_not_refused() {
        for name in [
            "/talks/deck.pdf",
            "/comics/book.cbz",
            "/comics/book.cbt",
            "/books/scan.djvu",
            "/pictures/shot.png",
            "/pictures/shot.jpg",
            "/talks/README",
        ] {
            assert!(
                unsupported_format(Path::new(name)).is_none(),
                "{name} must not be refused"
            );
        }
    }

    /// One extension, one format: a duplicate would make the message that
    /// wins depend on the order of the table.
    #[test]
    fn no_extension_appears_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for format in UNSUPPORTED_FORMATS {
            for extension in format.extensions {
                assert!(seen.insert(*extension), "{extension} is listed twice");
                assert_eq!(
                    *extension,
                    extension.to_lowercase(),
                    "the table is matched case-insensitively against lowercase entries"
                );
            }
        }
    }

    /// §64's table and this one are the same list; the ones that are read, or
    /// that are read through a library, are deliberately absent.
    #[test]
    fn the_table_covers_every_format_the_spec_names() {
        for extension in [
            "cbr", "cb7", "ps", "xps", "dvi", "epub", "mobi", "fb2", "chm", "odt",
        ] {
            let path = std::path::PathBuf::from(format!("/x.{extension}"));
            assert!(
                unsupported_format(&path).is_some(),
                "§64 names .{extension}"
            );
        }
    }
}
