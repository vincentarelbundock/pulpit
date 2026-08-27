//! A DjVu file behind the reader's [`DocumentBackend`] interface.
//!
//! `SPEC-reader-formats.md` §60.1: the reader can turn the pages of a DjVu
//! book and render them, and **every other operation reports `Unsupported`** —
//! annotations, form fields, text selection, save and signing are PDF
//! semantics with nothing honest to map them onto.
//!
//! §60.2 calls this the largest limitation of the whole design, and requires
//! it be *stated* rather than discovered by pressing a control that refuses:
//! the document opens at [`CompatibilityLevel::ViewOnly`], which is what the
//! UI reads to decide what to offer. §60.3 records why annotating a scanned
//! book is not simply a later feature — it would need a per-format sidecar,
//! and a second copy of the annotations that can drift from the document is
//! the one thing `SPEC-document.md` refuses for PDF.

use std::path::{Path, PathBuf};

use pulpit_core::annotate::{AnnotationDraft, AnnotationId};
use pulpit_core::notes::Region;
use pulpit_core::page::{PageGeometry, PageIndex};

use crate::djvu::backend::DjvuBackend;
use crate::document::model::{
    AnnotationBeforeImage, AnnotationSummary, CompatibilityLevel, FormField, OpenDocumentInfo,
    SaveOptions, TextSelection, TextSelectionResult,
};
use crate::document::{limits, unsupported_pdf_semantics, DocumentBackend, DocumentError, Result};
use crate::pdf::{BackendDocumentId, NeverCancel, PdfBackend, RenderRequest};

/// One open DjVu file, for the document worker.
pub struct DjvuDocument {
    backend: DjvuBackend,
    handle: BackendDocumentId,
    info: OpenDocumentInfo,
    path: PathBuf,
}

/// Why an operation that only a PDF can answer was refused, in the words the
/// reader shows.
fn unsupported(what: &str) -> DocumentError {
    DocumentError::Unsupported(format!("{what}: this document is a DjVu file"))
}

impl DjvuDocument {
    /// Open one DjVu file on an installed djvulibre.
    ///
    /// A machine with no DjVu library fails here, carrying
    /// [`crate::djvu::missing_djvu_message`] — which names the format and says
    /// what would install it, rather than calling the file damaged (§61.1,
    /// §61.2).
    pub fn open(source: &Path) -> Result<DjvuDocument> {
        let mut backend = DjvuBackend::bind().map_err(|e| DocumentError::Backend(e.to_string()))?;
        let handle = backend
            .open(source)
            .map_err(|e| DocumentError::Backend(e.to_string()))?;
        let metadata = backend
            .metadata(handle)
            .map_err(|e| DocumentError::Backend(e.to_string()))?;
        let first = metadata.first_page_size;
        Ok(DjvuDocument {
            info: OpenDocumentInfo {
                page_count: metadata.page_count,
                // It renders and turns its pages, and nothing else (§60.1).
                level: CompatibilityLevel::ViewOnly,
                warnings: Vec::new(),
                first_page: PageGeometry::upright(first.width, first.height),
                has_form: false,
            },
            backend,
            handle,
            path: source.to_path_buf(),
        })
    }
}

impl DocumentBackend for DjvuDocument {
    fn info(&self) -> &OpenDocumentInfo {
        &self.info
    }

    fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry> {
        let size = self
            .backend
            .page_size(self.handle, page.get())
            .map_err(|e| DocumentError::Backend(e.to_string()))?;
        Ok(PageGeometry::upright(size.width, size.height))
    }

    fn render_page(
        &self,
        page: PageIndex,
        region: Region,
        width: u32,
        height: u32,
        full_size: Option<(u32, u32)>,
        rgba: &mut [u8],
    ) -> Result<()> {
        let request = RenderRequest {
            document: self.handle,
            page: page.get(),
            region,
            width,
            height,
            // The reader's own marks are what a document-mode page is *for*,
            // but a DjVu carries none, so this changes nothing here; it is
            // set the way document mode sets it so the two paths do not
            // differ for a reason nobody can find later.
            with_annotations: true,
            full_size,
        };
        self.backend
            .render_into(&request, rgba, &NeverCancel)
            .map_err(|e| DocumentError::Backend(e.to_string()))
    }

    fn source(&self) -> Option<&Path> {
        Some(&self.path)
    }

    // Everything below is a PDF semantic. `Unsupported` rather than an empty
    // answer, everywhere: "this cannot be searched" and "there are no
    // matches" are different facts about a document (§60.1, §59.2).

    unsupported_pdf_semantics!(except find_text);

    /// §59.2. The one operation on this list a DjVu can answer.
    ///
    /// It is the *same* search the presenter runs — the same text layer, read
    /// through the same backend, matched by the same matcher — because the
    /// reader and the render worker are two processes holding two handles to
    /// one file, and a hit the presenter can highlight and the reader cannot
    /// would be two answers to one question.
    fn find_text(
        &self,
        query: &pulpit_core::search::Query,
        pages: std::ops::Range<usize>,
    ) -> Result<pulpit_core::search::HitChunk> {
        let mut chunk = pulpit_core::search::HitChunk {
            from_page: pages.start,
            to_page: pages.end,
            ..Default::default()
        };
        if query.is_empty() {
            return Ok(chunk);
        }
        chunk.hits = self
            .backend
            .find_text(self.handle, query, pages)
            .map_err(|e| DocumentError::Backend(e.to_string()))?;
        if chunk.hits.len() >= limits::MAX_HITS_PER_SEARCH {
            chunk.hits.truncate(limits::MAX_HITS_PER_SEARCH);
            chunk.truncated = true;
        }
        Ok(chunk)
    }
}
