//! A PDF-free backend used by tests, CI and as the last-resort fallback when
//! PDFium is not installed.
//!
//! It renders deterministic, recognisable pages so that every layer above it
//! — IPC, generations, cache accounting, aspect fit, the two windows — can be
//! exercised without a graphical session or a PDFium binary.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use pulpit_core::notes::Region;
use pulpit_core::PageSize;

use crate::pdf::{
    BackendDocumentId, CancelSignal, DocumentMetadata, PdfBackend, PdfError, RenderRequest,
    RenderedPage, Result,
};

#[derive(Debug, Clone)]
struct FixtureDocument {
    page_count: usize,
    page_size: PageSize,
    metadata_text: String,
    /// Synthetic link annotations (`fixture:...&links`): each page carries a
    /// link to the next page in the bottom-right corner and one to the
    /// project's homepage in the bottom-left.
    links: bool,
    /// Synthetic media links (`fixture:...&media`): one well-formed
    /// `pulpit://` video declaration and one that cannot be honoured, so
    /// overlay discovery can be exercised without PDFium.
    media: bool,
    /// Mixed page sizes (`fixture:...&mixed`): odd pages are 4:3, so the
    /// per-page geometry path is exercised without a real deck.
    mixed: bool,
    /// A two-level outline (`fixture:...&outline`) over the first pages.
    outline: bool,
    /// Features pulpit ignores (`fixture:...&features`): a sound
    /// annotation, a form field and a launch action.
    features: bool,
}

/// Synthetic deck generator.
#[derive(Debug, Default)]
pub struct FixtureBackend {
    documents: HashMap<u64, FixtureDocument>,
    next_id: u64,
    /// Artificial per-render delay, used to test cancellation and priority.
    pub render_delay: Option<Duration>,
}

impl FixtureBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Page count and mapping contract can be encoded in the path so tests
    /// and the fallback path can both drive it:
    /// `fixture:pages=30&mapping=pulpit:mapping=alternating`.
    fn parse(path: &Path) -> FixtureDocument {
        let text = path.to_string_lossy();
        let mut page_count = 20;
        let mut metadata_text = String::new();
        let mut links = false;
        let mut media = false;
        let mut mixed = false;
        let mut outline = false;
        let mut features = false;
        if let Some(query) = text.strip_prefix("fixture:") {
            for field in query.split('&') {
                if let Some(value) = field.strip_prefix("pages=") {
                    page_count = value.parse().unwrap_or(20);
                } else if let Some(value) = field.strip_prefix("mapping=") {
                    metadata_text = value.to_string();
                } else if field == "links" {
                    links = true;
                } else if field == "media" {
                    media = true;
                } else if field == "mixed" {
                    mixed = true;
                } else if field == "outline" {
                    outline = true;
                } else if field == "features" {
                    features = true;
                }
            }
        }
        FixtureDocument {
            page_count,
            page_size: PageSize {
                width: 960.0,
                height: 540.0,
            },
            metadata_text,
            links,
            media,
            mixed,
            outline,
            features,
        }
    }
}

impl FixtureDocument {
    fn size_of(&self, page: usize) -> PageSize {
        if self.mixed && page % 2 == 1 {
            PageSize {
                width: 720.0,
                height: 540.0,
            }
        } else {
            self.page_size
        }
    }
}

impl PdfBackend for FixtureBackend {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn version(&self) -> String {
        format!("synthetic pages, pulpit {}", env!("CARGO_PKG_VERSION"))
    }

    fn open(&mut self, source: &Path) -> Result<BackendDocumentId> {
        if source.to_string_lossy().contains("unreadable") {
            return Err(PdfError::Open {
                path: source.display().to_string(),
                reason: "fixture configured to fail".into(),
            });
        }
        self.next_id += 1;
        self.documents.insert(self.next_id, Self::parse(source));
        Ok(BackendDocumentId(self.next_id))
    }

    fn close(&mut self, document: BackendDocumentId) {
        self.documents.remove(&document.0);
    }

    fn metadata(&self, document: BackendDocumentId) -> Result<DocumentMetadata> {
        let doc = self
            .documents
            .get(&document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))?;
        let (page_sizes, page_sizes_sampled) =
            crate::pdf::collect_page_sizes(self, document, doc.page_count);
        Ok(DocumentMetadata {
            page_count: doc.page_count,
            first_page_size: doc.page_size,
            page_sizes,
            page_sizes_sampled,
            metadata_text: doc.metadata_text.clone(),
        })
    }

    fn page_size(&self, document: BackendDocumentId, page: usize) -> Result<PageSize> {
        let doc = self
            .documents
            .get(&document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))?;
        if page >= doc.page_count {
            return Err(PdfError::PageOutOfRange {
                page,
                count: doc.page_count,
            });
        }
        Ok(doc.size_of(page))
    }

    fn outline(&self, document: BackendDocumentId) -> Result<pulpit_core::Outline> {
        use pulpit_core::navigation::{Outline, OutlineEntry};
        use pulpit_core::LinkTarget;
        let doc = self
            .documents
            .get(&document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))?;
        if !doc.outline || doc.page_count < 3 {
            return Ok(Outline::default());
        }
        let entry =
            |title: &str, page: usize, depth: usize, children: Vec<OutlineEntry>| OutlineEntry {
                title: title.to_string(),
                target: LinkTarget::Page { page, zoom: None },
                depth,
                children,
            };
        Ok(Outline {
            entries: vec![
                entry("Introduction", 0, 0, Vec::new()),
                entry(
                    "Method",
                    1,
                    0,
                    vec![entry("Measurements", 2, 1, Vec::new())],
                ),
            ],
        })
    }

    fn evidence(
        &self,
        document: BackendDocumentId,
    ) -> Result<crate::pdf::capabilities::DocumentEvidence> {
        use crate::pdf::capabilities::{
            ActionKind, AnnotationEvidence, AnnotationSubtype, DocumentEvidence, FormType,
            PageEvidence,
        };
        let doc = self
            .documents
            .get(&document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))?;
        if !doc.features {
            return Ok(DocumentEvidence::default());
        }
        Ok(DocumentEvidence {
            form_type: FormType::AcroForm,
            document_javascript: vec!["setUp".to_string()],
            restriction: None,
            pages: vec![PageEvidence {
                page: 0,
                annotations: vec![
                    AnnotationEvidence {
                        subtype: AnnotationSubtype::Sound,
                        action: ActionKind::None,
                        uri: None,
                        has_additional_actions: false,
                    },
                    AnnotationEvidence {
                        subtype: AnnotationSubtype::Link,
                        action: ActionKind::Launch,
                        uri: None,
                        has_additional_actions: false,
                    },
                ],
                transition: Some("Dissolve".to_string()),
            }],
            transition_styles: Vec::new(),
        })
    }

    fn render(&self, request: &RenderRequest, cancel: &dyn CancelSignal) -> Result<RenderedPage> {
        request.validate()?;
        let doc = self
            .documents
            .get(&request.document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))?;
        if request.page >= doc.page_count {
            return Err(PdfError::PageOutOfRange {
                page: request.page,
                count: doc.page_count,
            });
        }
        if let Some(delay) = self.render_delay {
            // Sleep in slices so cancellation is prompt, exactly as the real
            // backend yields through its pause callback.
            let slice = Duration::from_millis(5);
            let mut slept = Duration::ZERO;
            while slept < delay {
                if cancel.is_cancelled() {
                    return Err(PdfError::Cancelled);
                }
                std::thread::sleep(slice);
                slept += slice;
            }
        }
        if cancel.is_cancelled() {
            return Err(PdfError::Cancelled);
        }
        Ok(draw_page(
            request.page,
            request.region,
            request.width,
            request.height,
        ))
    }

    fn links(
        &self,
        document: BackendDocumentId,
        page: usize,
    ) -> Result<Vec<pulpit_core::PageLink>> {
        use pulpit_core::{LinkTarget, PageLink};
        let doc = self
            .documents
            .get(&document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))?;
        if page >= doc.page_count {
            return Err(PdfError::PageOutOfRange {
                page,
                count: doc.page_count,
            });
        }
        if doc.media {
            return Ok(vec![
                PageLink {
                    rect: Region::new(0.1, 0.1, 0.5, 0.4),
                    target: LinkTarget::Uri("pulpit://video/clip?autoplay".into()),
                },
                PageLink {
                    rect: Region::new(0.1, 0.6, 0.5, 0.2),
                    target: LinkTarget::Uri("pulpit://hologram/x".into()),
                },
            ]);
        }
        if !doc.links {
            return Ok(Vec::new());
        }
        let mut links = vec![PageLink {
            rect: Region::new(0.05, 0.85, 0.2, 0.1),
            target: LinkTarget::Uri("https://example.com/pulpit".into()),
        }];
        if page + 1 < doc.page_count {
            links.push(PageLink {
                rect: Region::new(0.75, 0.85, 0.2, 0.1),
                target: LinkTarget::Page {
                    page: page + 1,
                    zoom: None,
                },
            });
        }
        Ok(links)
    }
}

/// Draw a recognisable placeholder slide: a per-page background tint, a
/// border, and the page number in a 5×7 bitmap font.
pub fn draw_page(page: usize, region: Region, width: u32, height: u32) -> RenderedPage {
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let tint = |channel: usize| -> u8 {
        let base = [40u8, 44, 52][channel];
        base.saturating_add(((page * 37 + channel * 53) % 60) as u8)
    };
    let background = [tint(0), tint(1), tint(2), 255];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&background);
    }

    let mut put = |x: i64, y: i64, colour: [u8; 4]| {
        if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
            return;
        }
        let index = (y as usize * width as usize + x as usize) * 4;
        pixels[index..index + 4].copy_from_slice(&colour);
    };

    // Border, so aspect-fit and letterboxing bugs are visible at a glance.
    let border = [200u8, 200, 200, 255];
    for x in 0..width as i64 {
        for t in 0..3 {
            put(x, t, border);
            put(x, height as i64 - 1 - t, border);
        }
    }
    for y in 0..height as i64 {
        for t in 0..3 {
            put(t, y, border);
            put(width as i64 - 1 - t, y, border);
        }
    }

    // Page number, centred, scaled to the frame.
    let label = format!("{}", page + 1);
    let scale = ((height / 12).max(2)) as i64;
    let glyph_width = 6 * scale;
    let total = glyph_width * label.len() as i64;
    let origin_x = (width as i64 - total) / 2;
    let origin_y = (height as i64 - 7 * scale) / 2;
    for (index, digit) in label.chars().enumerate() {
        let bitmap = digit_bitmap(digit);
        for (row, bits) in bitmap.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        put(
                            origin_x + index as i64 * glyph_width + column * scale + dx,
                            origin_y + row as i64 * scale + dy,
                            [235, 235, 235, 255],
                        );
                    }
                }
            }
        }
    }

    // A region marker so split-page mappings are visibly different.
    if !region.is_full() {
        let stripe = [120u8, 180, 255, 255];
        for x in 0..width as i64 {
            put(x, (height as i64 / 2).max(0), stripe);
        }
    }

    RenderedPage {
        width,
        height,
        pixels,
    }
}

/// 5×7 digits, one bit per column, MSB left.
fn digit_bitmap(digit: char) -> [u8; 7] {
    match digit {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        _ => [0b00000; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::NeverCancel;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn request(backend_document: BackendDocumentId, page: usize) -> RenderRequest {
        RenderRequest {
            document: backend_document,
            page,
            region: Region::FULL,
            width: 320,
            height: 180,
            with_annotations: false,
        }
    }

    #[test]
    fn renders_consistent_frames() {
        let mut backend = FixtureBackend::new();
        let document = backend.open(Path::new("fixture:pages=12")).unwrap();
        assert_eq!(backend.page_count(document).unwrap(), 12);

        let page = backend.render(&request(document, 3), &NeverCancel).unwrap();
        assert!(page.is_consistent());
        assert_eq!(page.byte_len(), 320 * 180 * 4);

        let again = backend.render(&request(document, 3), &NeverCancel).unwrap();
        assert_eq!(page, again, "rendering is deterministic");
        let other = backend.render(&request(document, 4), &NeverCancel).unwrap();
        assert_ne!(page, other, "pages are visually distinguishable");
    }

    #[test]
    fn out_of_range_pages_are_refused() {
        let mut backend = FixtureBackend::new();
        let document = backend.open(Path::new("fixture:pages=3")).unwrap();
        assert!(matches!(
            backend.render(&request(document, 9), &NeverCancel),
            Err(PdfError::PageOutOfRange { .. })
        ));
    }

    #[test]
    fn cancellation_is_prompt() {
        let mut backend = FixtureBackend::new();
        backend.render_delay = Some(Duration::from_secs(30));
        let document = backend.open(Path::new("fixture:pages=3")).unwrap();
        let cancel = AtomicBool::new(false);
        cancel.store(true, Ordering::Relaxed);
        let start = std::time::Instant::now();
        assert!(matches!(
            backend.render(&request(document, 0), &cancel),
            Err(PdfError::Cancelled)
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn the_metadata_contract_survives_the_round_trip() {
        let mut backend = FixtureBackend::new();
        let document = backend
            .open(Path::new(
                "fixture:pages=8&mapping=pulpit:mapping=alternating",
            ))
            .unwrap();
        let metadata = backend.metadata(document).unwrap();
        assert_eq!(
            pulpit_core::NotesMapping::from_metadata(&metadata.metadata_text),
            Some(pulpit_core::NotesMapping::PairedPages(
                pulpit_core::PairedRule::Alternating { notes_first: false }
            ))
        );
    }

    #[test]
    fn a_mixed_deck_reports_a_size_for_every_page() {
        let mut backend = FixtureBackend::new();
        let document = backend.open(Path::new("fixture:pages=4&mixed")).unwrap();
        let metadata = backend.metadata(document).unwrap();
        assert_eq!(metadata.page_sizes.len(), 4);
        assert!(!metadata.page_sizes_sampled);
        assert_eq!(metadata.page_sizes[0], metadata.first_page_size);
        assert_ne!(metadata.page_sizes[1], metadata.first_page_size);
    }

    #[test]
    fn the_outline_a_backend_cannot_read_is_empty_rather_than_an_error() {
        let mut backend = FixtureBackend::new();
        let plain = backend.open(Path::new("fixture:pages=4")).unwrap();
        assert!(backend.outline(plain).unwrap().is_empty());

        let annotated = backend.open(Path::new("fixture:pages=4&outline")).unwrap();
        let outline = backend.outline(annotated).unwrap();
        assert_eq!(outline.len(), 3);
        assert_eq!(outline.entries_at_depth(1).len(), 1);
    }

    #[test]
    fn evidence_is_only_reported_for_a_deck_that_declares_features() {
        let mut backend = FixtureBackend::new();
        let plain = backend.open(Path::new("fixture:pages=4")).unwrap();
        assert!(crate::pdf::capabilities::analyse(&backend.evidence(plain).unwrap()).is_empty());

        let rich = backend.open(Path::new("fixture:pages=4&features")).unwrap();
        let capabilities = crate::pdf::capabilities::analyse(&backend.evidence(rich).unwrap());
        use crate::pdf::capabilities::FindingKind;
        for kind in [
            FindingKind::FormFields,
            FindingKind::DocumentJavaScript,
            FindingKind::PageTransition,
            FindingKind::UnplayableMedia,
            FindingKind::UnsupportedAction,
        ] {
            assert!(capabilities.has(kind), "{kind:?} should be reported");
        }
    }

    #[test]
    fn an_unreadable_document_reports_instead_of_panicking() {
        let mut backend = FixtureBackend::new();
        assert!(backend.open(Path::new("fixture:unreadable")).is_err());
    }
}
