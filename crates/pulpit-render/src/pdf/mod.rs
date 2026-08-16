//! The narrow `PdfBackend` interface and its implementations.
//!
//! PDFium never runs in the main process; these types are used *inside* the
//! renderer worker (see `pulpit-render`). Keeping the interface this
//! small is what makes the binding a swappable decision rather than an
//! architectural commitment.

pub mod capabilities;
pub mod fixture;
pub mod overlays;
pub mod synth;

#[cfg(feature = "pdfium")]
pub mod pdfium;

use std::path::Path;

use pulpit_core::document::MAX_TRACKED_PAGE_SIZES;
use pulpit_core::navigation::Outline;
use pulpit_core::notes::Region;
use pulpit_core::{PageLink, PageSize};
use serde::{Deserialize, Serialize};

use crate::pdf::capabilities::DocumentEvidence;

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("the PDF backend is unavailable: {0}")]
    Unavailable(String),
    #[error("cannot open {path}: {reason}")]
    Open { path: String, reason: String },
    #[error("page {page} is out of range ({count} pages)")]
    PageOutOfRange { page: usize, count: usize },
    #[error("render failed: {0}")]
    Render(String),
    #[error("render was cancelled")]
    Cancelled,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("this backend cannot {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, PdfError>;

/// What a renderer worker prints before it exits because it could not bind
/// PDFium.
///
/// Every supported package installs PDFium alongside the binary, so this is a
/// broken installation rather than a normal state, and pulpit refuses to
/// render instead of substituting placeholders for the presenter's slides.
/// `reason` is the accumulated per-candidate failure from `PdfiumBackend::bind`
/// — the list of paths already tried is the most useful part of it.
pub fn missing_pdfium_message(reason: &str) -> String {
    [
        "pulpit cannot render: libpdfium was not found.",
        "",
        "PDFium is loaded at run time, so this is an installation problem, not",
        "a build one. Fixes, in order of preference:",
        "",
        "  Nix / NixOS    use the packaged build, which points the binary at a",
        "                 pinned PDFium:   nix run <flake> -- deck.pdf",
        "  Debian / RPM   install the pulpit package, which carries the pinned",
        "                 library in <prefix>/lib/pulpit",
        "  From source    ./scripts/fetch-pdfium.sh   (pinned, hash-verified)",
        "  Anywhere       PULPIT_PDFIUM_PATH=/dir/with/libpdfium pulpit deck.pdf",
        "",
        &format!("Tried: {reason}"),
    ]
    .join("\n")
}

/// Handle to a document held open by a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BackendDocumentId(pub u64);

/// A render request in *pixels*, already resolved from logical size × scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderRequest {
    pub document: BackendDocumentId,
    pub page: usize,
    /// Region of the page to render; `Region::FULL` for a whole page.
    pub region: Region,
    /// Target width in physical pixels.
    pub width: u32,
    /// Target height in physical pixels.
    pub height: u32,
    /// Draw the page's own annotations into the frame.
    ///
    /// Off for a presentation, and that is not an oversight: the presenter's
    /// marks are transient overlays drawn by the application over a clean
    /// page, and a slide that also carried the document's own commented-up
    /// `/Annots` would put somebody's review notes on the projector.
    ///
    /// On for document mode, where the annotations *are* the point: a page
    /// rendered without them is a page with the reader's own marks missing.
    #[serde(default)]
    pub with_annotations: bool,
}

impl RenderRequest {
    pub fn pixel_count(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// Decoded RGBA cost in bytes, the unit the cache budget is denominated in.
    pub fn rgba_bytes(&self) -> u64 {
        self.pixel_count() * 4
    }

    pub fn validate(&self) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(PdfError::Invalid("zero-sized render".into()));
        }
        // 16k × 16k RGBA is already a gigabyte; anything beyond is a bug or an
        // attack, and is refused before any memory is mapped.
        const MAX_DIMENSION: u32 = 16_384;
        if self.width > MAX_DIMENSION || self.height > MAX_DIMENSION {
            return Err(PdfError::Invalid(format!(
                "render {}×{} exceeds the {MAX_DIMENSION}px limit",
                self.width, self.height
            )));
        }
        if !self.region.is_valid() {
            return Err(PdfError::Invalid("region outside the page".into()));
        }
        Ok(())
    }
}

/// A rendered page: tightly packed RGBA8, `width * height * 4` bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RenderedPage {
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    pub fn is_consistent(&self) -> bool {
        self.pixels.len() == self.width as usize * self.height as usize * 4
    }
}

/// Everything the renderer needs to know about a document up front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub page_count: usize,
    pub first_page_size: PageSize,
    /// Size of pages `0..page_sizes.len()`, which is the whole document unless
    /// it is longer than [`MAX_TRACKED_PAGE_SIZES`]. Decks mixing 16:9 slides
    /// with a 4:3 appendix are common, so the first page is not allowed to
    /// speak for the rest.
    pub page_sizes: Vec<PageSize>,
    /// True when only a prefix of the document was measured; the pages beyond
    /// it fall back to the first page's size.
    pub page_sizes_sampled: bool,
    /// Concatenated metadata strings, searched for the notes-mapping contract.
    pub metadata_text: String,
}

/// Measure every page, up to the bound the domain sets on how many per-page
/// sizes are worth carrying.
///
/// A page that cannot be measured takes the first page's size rather than
/// truncating the vector: a hole in the middle would silently turn every
/// later page's lookup into a fallback.
pub fn collect_page_sizes(
    backend: &dyn PdfBackend,
    document: BackendDocumentId,
    page_count: usize,
) -> (Vec<PageSize>, bool) {
    let tracked = page_count.min(MAX_TRACKED_PAGE_SIZES);
    let mut sizes: Vec<PageSize> = Vec::with_capacity(tracked);
    for page in 0..tracked {
        let size = backend
            .page_size(document, page)
            .ok()
            .or_else(|| sizes.first().copied());
        match size {
            Some(size) => sizes.push(size),
            // The first page itself is unmeasurable: there is nothing to fall
            // back to, and the caller treats an empty vector as "unknown".
            None => return (Vec::new(), false),
        }
    }
    (sizes, page_count > tracked)
}

/// One page's presenter marks, ready to be stamped into a copy of a document.
///
/// The marks arrive in the normalised coordinates the domain keeps them in —
/// top-left origin, sizes as fractions of the *region*'s width — and are
/// mapped onto the page by whoever stamps them. Carrying the region rather
/// than pre-multiplying it is what lets a split-page deck, where a slide is
/// half of a physical page, export its marks over the half they were drawn
/// on rather than smeared across the sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageStamp {
    /// Physical PDF page index.
    pub page: usize,
    /// The part of that page the marks were drawn over.
    pub region: Region,
    pub strokes: Vec<pulpit_core::annotation::InkStroke>,
    pub images: Vec<StampImage>,
}

/// A picture stamped onto a page: a text annotation, already rasterised.
///
/// Text marks are Typst, and Typst's glyphs are not PDFium's — the only way
/// the exported file says what the projector said is to carry the pixels that
/// were on it. Vector strokes take the other path and stay vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StampImage {
    /// Top-left corner, normalised within the region.
    pub x: f32,
    pub y: f32,
    /// Fraction of the region's width the picture spans, before it is scaled
    /// down to fit whatever height is left below it.
    pub width: f32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Tightly packed RGBA8, `pixel_width * pixel_height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl StampImage {
    /// The largest picture worth stamping: 4096² is well past the resolution
    /// any projector shows a text annotation at, and bounds one message.
    pub const MAX_PIXELS: u64 = 4096 * 4096;

    pub fn is_consistent(&self) -> bool {
        self.pixel_width > 0
            && self.pixel_height > 0
            && u64::from(self.pixel_width) * u64::from(self.pixel_height) <= Self::MAX_PIXELS
            && self.rgba.len() == self.pixel_width as usize * self.pixel_height as usize * 4
    }
}

impl PageStamp {
    /// More marks than a slide a presenter drew on by hand ever carries;
    /// bounds what one export message can be asked to hold.
    pub const MAX_STROKES: usize = pulpit_core::annotation::Annotations::MAX_STROKES * 4;
    pub const MAX_IMAGES: usize = 256;

    pub fn validate(&self) -> Result<()> {
        if !self.region.is_valid() {
            return Err(PdfError::Invalid("stamp region outside the page".into()));
        }
        if self.strokes.len() > Self::MAX_STROKES || self.images.len() > Self::MAX_IMAGES {
            return Err(PdfError::Invalid(format!(
                "page {} carries more marks than an export accepts",
                self.page
            )));
        }
        if !self.images.iter().all(StampImage::is_consistent) {
            return Err(PdfError::Invalid(format!(
                "page {} carries a malformed picture",
                self.page
            )));
        }
        Ok(())
    }
}

/// A cancellation flag shared with the caller. The PDFium progressive API
/// polls it through its pause callback, so obsolete work yields promptly
/// instead of holding a worker hostage.
pub trait CancelSignal: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// A signal that never cancels.
pub struct NeverCancel;

impl CancelSignal for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl CancelSignal for std::sync::atomic::AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub trait PdfBackend: Send {
    fn name(&self) -> &'static str;

    /// What this backend *is*, in enough detail to explain a rendering
    /// difference between two machines: a library path, a build number. Only
    /// ever shown in diagnostics.
    fn version(&self) -> String {
        "unknown".to_string()
    }

    fn open(&mut self, source: &Path) -> Result<BackendDocumentId>;

    fn close(&mut self, document: BackendDocumentId);

    fn metadata(&self, document: BackendDocumentId) -> Result<DocumentMetadata>;

    fn page_count(&self, document: BackendDocumentId) -> Result<usize> {
        Ok(self.metadata(document)?.page_count)
    }

    fn page_size(&self, document: BackendDocumentId, page: usize) -> Result<PageSize>;

    /// Render a page. Implementations must check `cancel` regularly and
    /// return [`PdfError::Cancelled`] promptly when it fires.
    fn render(&self, request: &RenderRequest, cancel: &dyn CancelSignal) -> Result<RenderedPage>;

    /// Render a page straight into `target`, which must hold at least
    /// `width * height * 4` bytes. Backends that can point their rasteriser
    /// at the buffer override this and skip the intermediate frame the
    /// default allocates — the worker passes the shared-memory mapping here,
    /// so an override renders directly into it.
    fn render_into(
        &self,
        request: &RenderRequest,
        target: &mut [u8],
        cancel: &dyn CancelSignal,
    ) -> Result<()> {
        let page = self.render(request, cancel)?;
        target[..page.pixels.len()].copy_from_slice(&page.pixels);
        Ok(())
    }

    /// Link annotations on a page, in normalised page coordinates. Backends
    /// without link support report none rather than failing.
    fn links(&self, _document: BackendDocumentId, _page: usize) -> Result<Vec<PageLink>> {
        Ok(Vec::new())
    }

    /// The bytes of one entry in the embedded-files name tree. A backend
    /// without attachment support says so rather than pretending the deck
    /// has none: an overlay whose asset is missing must fall back visibly.
    fn attachment(&self, _document: BackendDocumentId, _name: &str) -> Result<Vec<u8>> {
        Err(PdfError::Invalid("attachments unsupported".into()))
    }

    /// Every attachment name, for preflight. An empty list is an honest
    /// answer from a backend that cannot enumerate them.
    fn attachment_names(&self, _document: BackendDocumentId) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Producer-supplied page labels, which mark logical-slide boundaries.
    /// Absent labels leave the grouping rule to geometry and content alone.
    fn page_labels(
        &self,
        _document: BackendDocumentId,
    ) -> Result<pulpit_core::overlay::PageLabels> {
        Ok(Default::default())
    }

    /// The document outline (bookmark tree). A backend that cannot read one
    /// reports an empty outline: section display simply has nothing to show,
    /// which is also what a deck without bookmarks looks like.
    fn outline(&self, _document: BackendDocumentId) -> Result<Outline> {
        Ok(Outline::default())
    }

    /// Write a copy of `source` to `destination` with `pages`' marks stamped
    /// into the page content.
    ///
    /// Never touches `source`, and never touches a document this backend has
    /// open for rendering: the copy is loaded, drawn on and closed on its own.
    /// The audience frame is unaffected by an export, including a failed one.
    fn export_annotated(
        &self,
        _source: &Path,
        _destination: &Path,
        _pages: &[PageStamp],
    ) -> Result<()> {
        Err(PdfError::Unsupported("write an annotated PDF"))
    }

    /// Evidence of features pulpit will flatten or ignore. A backend that
    /// cannot inspect a document reports none rather than guessing; the
    /// analysis in [`capabilities`] never invents a finding without evidence.
    fn evidence(&self, _document: BackendDocumentId) -> Result<DocumentEvidence> {
        Ok(DocumentEvidence::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_validated_before_any_allocation() {
        let base = RenderRequest {
            document: BackendDocumentId(1),
            page: 0,
            region: Region::FULL,
            width: 1920,
            height: 1080,
            with_annotations: false,
        };
        assert!(base.validate().is_ok());
        assert_eq!(base.rgba_bytes(), 1920 * 1080 * 4);

        assert!(RenderRequest {
            width: 0,
            ..base.clone()
        }
        .validate()
        .is_err());
        assert!(RenderRequest {
            width: 40_000,
            ..base.clone()
        }
        .validate()
        .is_err());
        assert!(RenderRequest {
            region: Region::new(0.5, 0.0, 0.9, 1.0),
            ..base.clone()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn page_sizes_are_measured_exactly_up_to_the_bound_and_then_sampled() {
        use crate::pdf::fixture::FixtureBackend;
        let mut backend = FixtureBackend::new();
        let document = backend
            .open(std::path::Path::new(&format!(
                "fixture:pages={}&mixed",
                MAX_TRACKED_PAGE_SIZES + 10
            )))
            .unwrap();
        let (sizes, sampled) = collect_page_sizes(&backend, document, MAX_TRACKED_PAGE_SIZES + 10);
        assert_eq!(sizes.len(), MAX_TRACKED_PAGE_SIZES);
        assert!(sampled, "a deck longer than the bound says it was sampled");

        let short = backend
            .open(std::path::Path::new("fixture:pages=3&mixed"))
            .unwrap();
        let (sizes, sampled) = collect_page_sizes(&backend, short, 3);
        assert_eq!(sizes.len(), 3);
        assert!(!sampled);
    }

    #[test]
    fn a_document_whose_first_page_cannot_be_measured_reports_no_sizes() {
        use crate::pdf::fixture::FixtureBackend;
        let mut backend = FixtureBackend::new();
        let document = backend
            .open(std::path::Path::new("fixture:pages=2"))
            .unwrap();
        // Claiming more pages than the document has makes every measurement
        // fail, which is the shape of a backend that cannot answer at all.
        let (sizes, sampled) = collect_page_sizes(&backend, BackendDocumentId(999), 4);
        assert!(sizes.is_empty());
        assert!(!sampled);
        assert!(!collect_page_sizes(&backend, document, 2).0.is_empty());
    }

    #[test]
    fn a_4k_rgba_frame_costs_what_the_spec_says() {
        let request = RenderRequest {
            document: BackendDocumentId(1),
            page: 0,
            region: Region::FULL,
            width: 3840,
            height: 2160,
            with_annotations: false,
        };
        assert_eq!(request.rgba_bytes(), 33_177_600);
    }
}
