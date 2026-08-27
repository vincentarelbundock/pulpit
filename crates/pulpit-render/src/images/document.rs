//! An image directory behind the reader's [`DocumentBackend`] interface.
//!
//! `SPEC-images.md` §48. The reader can turn the pages of a folder and render
//! them, and **every other operation reports `Unsupported`**: annotations,
//! form fields, text selection, save and signing are PDF semantics and there
//! is nothing honest to map them onto. The UI reflects that rather than
//! offering controls that refuse when pressed (§48.3).

use std::path::Path;
use std::sync::Mutex;

use pulpit_core::annotate::{AnnotationDraft, AnnotationId};
use pulpit_core::notes::Region;
use pulpit_core::page::{PageGeometry, PageIndex};

use crate::document::model::{
    AnnotationBeforeImage, AnnotationSummary, CompatibilityLevel, FormField, OpenDocumentInfo,
    SaveOptions, TextSelection, TextSelectionResult,
};
use crate::document::unsupported_pdf_semantics;
use crate::document::{DocumentBackend, DocumentError, Result};
use crate::images::decode::{self, DecodedCache, DecodedKey};
use crate::images::table::{list_source, resolve_source, PageSource, PageTable};

/// One open image document — a folder or a comic archive — for the document
/// worker.
pub struct ImageDocument {
    table: PageTable,
    info: OpenDocumentInfo,
    /// Every page's geometry, measured once at open.
    ///
    /// The reader asks for geometries in runs as it lays the column out, and
    /// answering each from the source would re-walk an archive per page. One
    /// pass at open is the same total work, done once.
    geometry: Vec<PageGeometry>,
    cache: Mutex<DecodedCache>,
}

/// Why an operation that only a PDF can answer was refused, in the words the
/// reader shows.
fn unsupported(what: &str) -> DocumentError {
    DocumentError::Unsupported(format!("{what}: this document is a folder of images"))
}

impl ImageDocument {
    /// Open `source` as an image document. A file resolves to its parent
    /// directory (§40.2) and a comic archive is the document itself (§54.1),
    /// exactly as the renderer's backend does.
    pub fn open(source: &Path) -> Result<ImageDocument> {
        // §61.2 and §61.4: a format pulpit does not read is named, not
        // reported as a damaged archive.
        if let Some(message) = crate::formats::unsupported_format(source) {
            return Err(DocumentError::UnsupportedFormat(message.to_string()));
        }
        let resolved = resolve_source(source).ok_or_else(|| {
            DocumentError::Backend(format!(
                "{} is not a directory, a comic archive or a supported image",
                source.display()
            ))
        })?;
        let table =
            list_source(&resolved.source).map_err(|e| DocumentError::Backend(e.to_string()))?;
        let geometry = measure(&table);
        Ok(ImageDocument {
            info: OpenDocumentInfo {
                page_count: table.len(),
                // §48: it renders and turns its pages, and nothing else.
                level: CompatibilityLevel::ViewOnly,
                warnings: Vec::new(),
                first_page: geometry.first().copied().unwrap_or_default(),
                has_form: false,
            },
            table,
            geometry,
            cache: Mutex::new(DecodedCache::default()),
        })
    }

    pub fn table(&self) -> &PageTable {
        &self.table
    }

    /// The document's own path: the folder, or the archive file.
    pub fn path(&self) -> &Path {
        self.table.path()
    }
}

/// Every page's geometry, in one pass over the source.
fn measure(table: &PageTable) -> Vec<PageGeometry> {
    let sizes: Vec<Option<PageGeometry>> = match table.source() {
        PageSource::Archive { path, kind } => {
            let measured = crate::images::archive::measure_entries(path, *kind);
            table
                .entries()
                .iter()
                .map(|entry| {
                    measured
                        .get(&entry.name)
                        .map(|(width, height)| PageGeometry::upright(*width as f32, *height as f32))
                })
                .collect()
        }
        PageSource::Directory(_) => (0..table.len())
            .map(|page| {
                table
                    .locate(page)
                    .and_then(|at| decode::dimensions_at(&at).ok())
                    .map(|(width, height)| PageGeometry::upright(width as f32, height as f32))
            })
            .collect(),
    };
    // A page that will not decode keeps its place and takes a plausible
    // shape, so the column lays out and the failure shows up where it
    // belongs — in that page's own render (§49).
    let mut geometry: Vec<PageGeometry> = Vec::with_capacity(sizes.len());
    for size in sizes {
        let resolved = size
            .or_else(|| geometry.first().copied())
            .unwrap_or_default();
        geometry.push(resolved);
    }
    geometry
}

impl DocumentBackend for ImageDocument {
    fn info(&self) -> &OpenDocumentInfo {
        &self.info
    }

    fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry> {
        self.geometry
            .get(page.get())
            .copied()
            .ok_or(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.geometry.len(),
            })
    }

    fn render_page(
        &self,
        page: PageIndex,
        region: Region,
        width: u32,
        height: u32,
        _full_size: Option<(u32, u32)>,
        rgba: &mut [u8],
    ) -> Result<()> {
        let entry = self
            .table
            .entries()
            .get(page.get())
            .ok_or(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.table.len(),
            })?;
        let key = DecodedKey {
            document: 0,
            page: page.get(),
            len: entry.len,
            modified: entry.modified,
        };
        let cached = self.cache.lock().ok().and_then(|mut cache| cache.get(&key));
        let image = match cached {
            Some(image) => image,
            None => {
                let at = self
                    .table
                    .locate(page.get())
                    .ok_or(DocumentError::NoSuchPage {
                        page: page.get(),
                        count: self.table.len(),
                    })?;
                let decoded = std::sync::Arc::new(
                    decode::decode_at(&at).map_err(|e| DocumentError::Backend(e.to_string()))?,
                );
                if let Ok(mut cache) = self.cache.lock() {
                    cache.insert(key, std::sync::Arc::clone(&decoded));
                }
                decoded
            }
        };
        decode::scale_into(&image, region, width, height, rgba)
            .map_err(|e| DocumentError::Backend(e.to_string()))
    }

    fn source(&self) -> Option<&Path> {
        Some(self.table.path())
    }

    // Everything below is a PDF semantic. `Unsupported` rather than an empty
    // answer, everywhere: "this cannot be searched" and "there are no
    // matches" are different facts about a document (§48.1, §48.2).

    unsupported_pdf_semantics!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(20, 10, image::Rgba([7, 8, 9, 255]))
            .save(dir.path().join("a.png"))
            .unwrap();
        image::RgbaImage::from_pixel(10, 20, image::Rgba([1, 2, 3, 255]))
            .save(dir.path().join("b.png"))
            .unwrap();
        dir
    }

    #[test]
    fn a_folder_opens_as_a_document_the_reader_can_turn_the_pages_of() {
        let dir = folder();
        let document = ImageDocument::open(dir.path()).unwrap();
        assert_eq!(document.page_count(), 2);
        assert_eq!(document.info().first_page.width, 20.0);
        assert_eq!(document.page_geometry(PageIndex(1)).unwrap().width, 10.0);
        assert_eq!(document.source(), Some(dir.path()));
        assert!(!document.info().has_form);
    }

    #[test]
    fn a_page_renders() {
        let dir = folder();
        let document = ImageDocument::open(dir.path()).unwrap();
        let mut rgba = vec![0u8; 4 * 4 * 4];
        document
            .render_page(PageIndex(1), Region::FULL, 4, 4, None, &mut rgba)
            .unwrap();
        assert_eq!(&rgba[..4], &[1, 2, 3, 255]);
    }

    /// §48.1 and §48.2, as one test: every PDF semantic refuses, and search
    /// refuses rather than answering "no matches".
    #[test]
    fn every_pdf_semantic_reports_unsupported() {
        let dir = folder();
        let mut document = ImageDocument::open(dir.path()).unwrap();
        let id = AnnotationId::imported("pulpit-1").expect("a usable name");

        assert!(matches!(
            document.annotations(PageIndex(0)),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(matches!(
            document.fields(),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(matches!(
            document.field_value("name"),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(matches!(
            document.set_field("name", "value", &[]),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(matches!(
            document.before_image(&id),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(matches!(
            document.select_text(
                PageIndex(0),
                TextSelection::Word {
                    at: pulpit_core::page::PagePoint { x: 0.0, y: 0.0 },
                },
            ),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(matches!(
            document.find_text(&pulpit_core::search::Query::new("x", false, false), 0..2),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(matches!(
            document.write_to(Path::new("/dev/null"), SaveOptions::default()),
            Err(DocumentError::Unsupported(_))
        ));
        assert!(
            document.outline().unwrap().entries.is_empty(),
            "§48.4: an empty outline is indistinguishable from a PDF carrying none"
        );
    }

    #[test]
    fn opening_an_image_file_opens_the_folder_around_it() {
        let dir = folder();
        let document = ImageDocument::open(&dir.path().join("b.png")).unwrap();
        assert_eq!(document.page_count(), 2);
    }

    /// `SPEC-reader-formats.md` §54: a comic archive reaches the reader the
    /// same way a folder does, and answers the same way — including refusing
    /// every PDF semantic (§60.1).
    #[test]
    fn a_comic_archive_opens_in_the_reader_and_turns_its_pages() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        {
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            for (name, width, height, colour) in [
                ("ch-1/page-02.png", 20u32, 30u32, [4u8, 5, 6, 255]),
                ("ch-1/page-10.png", 30, 20, [8, 9, 10, 255]),
            ] {
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::RgbaImage::from_pixel(width, height, image::Rgba(colour))
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                writer.start_file(name, options).unwrap();
                writer.write_all(bytes.get_ref()).unwrap();
            }
            writer.finish().unwrap();
        }

        let document = ImageDocument::open(&path).unwrap();
        assert_eq!(document.page_count(), 2);
        assert!(document.info().level.is_view_only());
        assert_eq!(document.source(), Some(path.as_path()));
        // page-02 before page-10, by natural sort over the full entry path.
        assert_eq!(document.page_geometry(PageIndex(0)).unwrap().width, 20.0);
        assert_eq!(document.page_geometry(PageIndex(1)).unwrap().width, 30.0);

        let mut rgba = vec![0u8; 4 * 4 * 4];
        document
            .render_page(PageIndex(1), Region::FULL, 4, 4, None, &mut rgba)
            .unwrap();
        assert_eq!(&rgba[..4], &[8, 9, 10, 255]);

        assert!(matches!(
            document.find_text(&pulpit_core::search::Query::new("x", false, false), 0..2),
            Err(DocumentError::Unsupported(_))
        ));
    }

    /// §61.1 and §61.4 in the reader: a format pulpit does not read is
    /// refused by name here too, and the message reaches the presenter as
    /// written.
    ///
    /// `UnsupportedFormat` rather than `Unsupported` is the whole point.
    /// `Unsupported` takes a verb phrase and prints "this document cannot
    /// {it}", which turned a refusal into "this document cannot RAR archives
    /// (.cbr) are not supported" — neither fact, and the presenter reads it.
    #[test]
    fn a_format_pulpit_does_not_read_is_refused_by_name_in_the_reader_too() {
        let dir = tempfile::tempdir().unwrap();
        for (name, expected) in [
            ("comic.cbr", "RAR"),
            ("talk.ps", "PostScript"),
            ("book.epub", "EPUB"),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"not that it is ever read").unwrap();
            let Err(error) = ImageDocument::open(&path) else {
                panic!("{name} is refused, not opened");
            };
            assert!(
                matches!(error, DocumentError::UnsupportedFormat(_)),
                "{name}"
            );
            let said = error.to_string();
            assert!(said.contains(expected), "{said}");
            assert!(
                !said.contains("this document cannot"),
                "a refusal is a whole sentence, not a verb phrase — {said}"
            );
        }
    }
}
