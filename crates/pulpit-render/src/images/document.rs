//! An image directory behind the reader's [`DocumentBackend`] interface.
//!
//! `SPEC-images.md` §48. The reader can turn the pages of a folder and render
//! them, and **every other operation reports `Unsupported`**: annotations,
//! form fields, text selection, save and signing are PDF semantics and there
//! is nothing honest to map them onto. The UI reflects that rather than
//! offering controls that refuse when pressed (§48.3).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pulpit_core::annotate::{AnnotationDraft, AnnotationId};
use pulpit_core::notes::Region;
use pulpit_core::page::{PageGeometry, PageIndex};

use crate::document::model::{
    AnnotationBeforeImage, AnnotationSummary, CompatibilityLevel, FormField, OpenDocumentInfo,
    SaveOptions, TextSelection, TextSelectionResult,
};
use crate::document::{DocumentBackend, DocumentError, Result};
use crate::images::decode::{self, DecodedCache, DecodedKey};
use crate::images::table::{list_directory, resolve_source, PageTable};

/// One open image directory, for the document worker.
pub struct ImageDocument {
    table: PageTable,
    info: OpenDocumentInfo,
    cache: Mutex<DecodedCache>,
}

/// Why an operation that only a PDF can answer was refused, in the words the
/// reader shows.
fn unsupported(what: &str) -> DocumentError {
    DocumentError::Unsupported(format!("{what}: this document is a folder of images"))
}

impl ImageDocument {
    /// Open `source` as an image document. A file resolves to its parent
    /// directory (§40.2), exactly as the renderer's backend does.
    pub fn open(source: &Path) -> Result<ImageDocument> {
        let resolved = resolve_source(source).ok_or_else(|| {
            DocumentError::Backend(format!(
                "{} is neither a directory nor a supported image",
                source.display()
            ))
        })?;
        let table = list_directory(&resolved.directory)
            .map_err(|e| DocumentError::Backend(e.to_string()))?;
        let first_page = table
            .path(0)
            .and_then(|path| decode::dimensions(&path).ok())
            .map(|(width, height)| PageGeometry::upright(width as f32, height as f32))
            .unwrap_or_default();
        Ok(ImageDocument {
            info: OpenDocumentInfo {
                page_count: table.len(),
                // §48: it renders and turns its pages, and nothing else.
                level: CompatibilityLevel::ViewOnly,
                warnings: Vec::new(),
                first_page,
                has_form: false,
            },
            table,
            cache: Mutex::new(DecodedCache::default()),
        })
    }

    pub fn table(&self) -> &PageTable {
        &self.table
    }

    pub fn directory(&self) -> &Path {
        self.table.directory()
    }

    fn page_path(&self, page: PageIndex) -> Result<PathBuf> {
        self.table
            .path(page.get())
            .ok_or(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.table.len(),
            })
    }
}

impl DocumentBackend for ImageDocument {
    fn info(&self) -> &OpenDocumentInfo {
        &self.info
    }

    fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry> {
        let path = self.page_path(page)?;
        // Header-only, like every other size read on this path (§46.1).
        let (width, height) =
            decode::dimensions(&path).map_err(|e| DocumentError::Backend(e.to_string()))?;
        Ok(PageGeometry::upright(width as f32, height as f32))
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
                let path = self.table.directory().join(&entry.name);
                let decoded = std::sync::Arc::new(
                    decode::decode(&path).map_err(|e| DocumentError::Backend(e.to_string()))?,
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
        Some(self.table.directory())
    }

    // Everything below is a PDF semantic. `Unsupported` rather than an empty
    // answer, everywhere: "this cannot be searched" and "there are no
    // matches" are different facts about a document (§48.1, §48.2).

    fn annotations(&self, _page: PageIndex) -> Result<Vec<AnnotationSummary>> {
        Err(unsupported("carry annotations"))
    }

    fn annotation(&self, _id: &AnnotationId) -> Result<AnnotationSummary> {
        Err(unsupported("carry annotations"))
    }

    fn create(
        &mut self,
        _id: &AnnotationId,
        _draft: &AnnotationDraft,
    ) -> Result<AnnotationSummary> {
        Err(unsupported("be annotated"))
    }

    fn replace(
        &mut self,
        _id: &AnnotationId,
        _draft: &AnnotationDraft,
    ) -> Result<AnnotationSummary> {
        Err(unsupported("be annotated"))
    }

    fn delete(&mut self, _id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        Err(unsupported("be annotated"))
    }

    fn restore(
        &mut self,
        _id: &AnnotationId,
        _before: &AnnotationBeforeImage,
    ) -> Result<AnnotationSummary> {
        Err(unsupported("be annotated"))
    }

    fn before_image(&self, _id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        Err(unsupported("be annotated"))
    }

    fn fields(&self) -> Result<Vec<FormField>> {
        Err(unsupported("hold form fields"))
    }

    fn field(&self, _name: &str) -> Result<Option<FormField>> {
        Err(unsupported("hold form fields"))
    }

    fn set_field(&mut self, _name: &str, _value: &str, _selected: &[u32]) -> Result<String> {
        Err(unsupported("hold form fields"))
    }

    fn field_value(&self, _name: &str) -> Result<String> {
        Err(unsupported("hold form fields"))
    }

    fn select_text(
        &self,
        _page: PageIndex,
        _selection: TextSelection,
    ) -> Result<TextSelectionResult> {
        Err(unsupported("have its text selected"))
    }

    fn find_text(
        &self,
        _query: &pulpit_core::search::Query,
        _pages: std::ops::Range<usize>,
    ) -> Result<pulpit_core::search::HitChunk> {
        Err(unsupported("be searched"))
    }

    fn write_to(&mut self, _destination: &Path, _options: SaveOptions) -> Result<u64> {
        Err(unsupported("be saved"))
    }
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
}
