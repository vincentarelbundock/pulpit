//! The image backend: a directory of pictures behind the renderer's
//! [`PdfBackend`] interface.
//!
//! `SPEC-images.md` §40–§49. The directory takes the role the PDF file takes,
//! each image is a page, and everything above — the frame cache, generations,
//! the overview grid, aspect fit — is untouched (§50).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use pulpit_core::PageSize;

use crate::images::decode::{self, DecodedCache, DecodedKey, ImageFailure};
use crate::images::table::{list_source, resolve_source, PageSource, PageTable};
use crate::pdf::{
    BackendDocumentId, CancelSignal, DocumentMetadata, PdfBackend, PdfError, RenderRequest,
    RenderedPage, Result,
};

/// A worker's open image documents, and the pixels it has already decoded.
pub struct ImageBackend {
    documents: HashMap<u64, PageTable>,
    next_id: u64,
    /// §47.1. Distinct from the frame cache and from `thumbnails.rs`, with
    /// its own budget: re-scaling one decoded image across the audience
    /// frame, the presenter frame and a thumbnail must not decode it three
    /// times.
    cache: Mutex<DecodedCache>,
}

impl Default for ImageBackend {
    fn default() -> ImageBackend {
        ImageBackend::new()
    }
}

impl ImageBackend {
    pub fn new() -> ImageBackend {
        ImageBackend::with_budget(decode::DEFAULT_DECODED_BUDGET_BYTES)
    }

    pub fn with_budget(budget: u64) -> ImageBackend {
        ImageBackend {
            documents: HashMap::new(),
            next_id: 0,
            cache: Mutex::new(DecodedCache::new(budget)),
        }
    }

    /// The page table of an open document, for a caller that already holds
    /// one — the digest comparison of §42.3 in particular.
    pub fn table(&self, document: BackendDocumentId) -> Option<&PageTable> {
        self.documents.get(&document.0)
    }

    fn look_up(&self, document: BackendDocumentId) -> Result<&PageTable> {
        self.documents
            .get(&document.0)
            .ok_or_else(|| PdfError::Render("unknown document".into()))
    }

    /// Measure every page, in whichever way the source makes cheapest.
    ///
    /// A directory answers per file; an archive is walked **once** (§54.5),
    /// because reading its entries one at a time is quadratic in the page
    /// count on the path that runs before the first frame appears.
    fn measure_pages(&self, table: &PageTable) -> Vec<PageSize> {
        let of = |(width, height): (u32, u32)| PageSize {
            width: width as f32,
            height: height as f32,
        };
        let measured: Vec<Option<PageSize>> = match table.source() {
            PageSource::Archive { path, kind } => {
                let sizes = crate::images::archive::measure_entries(path, *kind);
                table
                    .entries()
                    .iter()
                    .map(|entry| sizes.get(&entry.name).copied().map(of))
                    .collect()
            }
            PageSource::Directory(_) => (0..table.len())
                .map(|page| {
                    table
                        .locate(page)
                        .and_then(|at| decode::dimensions_at(&at).ok())
                        .map(of)
                })
                .collect(),
        };
        // A page that could not be measured takes the first page's size
        // rather than truncating the vector, exactly as `collect_page_sizes`
        // does: a hole in the middle would silently turn every later page's
        // lookup into a fallback.
        let mut sizes: Vec<PageSize> = Vec::with_capacity(measured.len());
        for size in measured {
            match size.or_else(|| sizes.first().copied()) {
                Some(size) => sizes.push(size),
                // The first page itself is unmeasurable: there is nothing to
                // fall back to, and the caller reads an empty vector as
                // "unknown".
                None => return Vec::new(),
            }
        }
        sizes
    }

    /// The decoded page, from the cache when it is there.
    ///
    /// Cancellation granularity is one image (§47.3): the flag is checked
    /// before the decode and again before the scale, but a decode is a single
    /// blocking call and cannot be interrupted the way PDFium's progressive
    /// API can. That is a known regression in responsiveness against the PDF
    /// path, bounded by the input limit and mitigated by this cache.
    fn decoded(
        &self,
        document: BackendDocumentId,
        page: usize,
        cancel: &dyn CancelSignal,
    ) -> std::result::Result<Arc<image::RgbaImage>, ImageFailure> {
        let table = self
            .documents
            .get(&document.0)
            .ok_or_else(|| ImageFailure::Unreadable {
                path: format!("document {}", document.0),
                reason: "no such document".into(),
            })?;
        let entry = table
            .entries()
            .get(page)
            .ok_or_else(|| ImageFailure::Unreadable {
                path: table.path().display().to_string(),
                reason: format!("no page {page}"),
            })?;
        let key = DecodedKey {
            document: document.0,
            page,
            len: entry.len,
            modified: entry.modified,
        };
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(image) = cache.get(&key) {
                return Ok(image);
            }
        }
        if cancel.is_cancelled() {
            return Err(ImageFailure::Cancelled);
        }
        let at = table.locate(page).ok_or_else(|| ImageFailure::Unreadable {
            path: table.path().display().to_string(),
            reason: format!("no page {page}"),
        })?;
        let image = Arc::new(decode::decode_at(&at)?);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, Arc::clone(&image));
        }
        Ok(image)
    }
}

impl PdfBackend for ImageBackend {
    fn name(&self) -> &'static str {
        "images"
    }

    fn version(&self) -> String {
        format!("image crate, pulpit {}", env!("CARGO_PKG_VERSION"))
    }

    fn open(&mut self, source: &Path) -> Result<BackendDocumentId> {
        // The document is the *directory* (§40.1), or the comic archive that
        // replaces it (§54.1). An image file resolves to its parent, which is
        // what makes opening a screenshot an image viewer; the application
        // says so out loud before any navigation happens (§40.3).
        //
        // A format pulpit refuses is refused by name here too, not only in
        // the application: a worker handed one directly must say what the
        // format is rather than report a damaged archive (§61.2, §61.4).
        if let Some(message) = crate::formats::unsupported_format(source) {
            return Err(PdfError::Open {
                path: source.display().to_string(),
                reason: message.to_string(),
            });
        }
        let resolved = resolve_source(source).ok_or_else(|| PdfError::Open {
            path: source.display().to_string(),
            reason: "not a directory, a comic archive or a supported image".into(),
        })?;
        let table = list_source(&resolved.source).map_err(|e| PdfError::Open {
            path: resolved.path().display().to_string(),
            reason: e.to_string(),
        })?;
        self.next_id += 1;
        self.documents.insert(self.next_id, table);
        Ok(BackendDocumentId(self.next_id))
    }

    fn close(&mut self, document: BackendDocumentId) {
        self.documents.remove(&document.0);
        if let Ok(mut cache) = self.cache.lock() {
            cache.forget_document(document.0);
        }
    }

    fn metadata(&self, document: BackendDocumentId) -> Result<DocumentMetadata> {
        let table = self.look_up(document)?;
        let page_sizes = self.measure_pages(table);
        Ok(DocumentMetadata {
            page_count: table.len(),
            first_page_size: page_sizes.first().copied().unwrap_or(PageSize {
                width: 1.0,
                height: 1.0,
            }),
            page_sizes,
            // §46.3. The sampling fallback answers with the *first* page's
            // size, which rests on pages being overwhelmingly uniform. That
            // is false for a photo directory, so the table is capped instead
            // and this is never true.
            page_sizes_sampled: false,
            // §46.5. No `.pdfpc` sidecar, no embedded attachment, no metadata
            // text — and nothing synthesised from file names.
            metadata_text: String::new(),
            // Present for a directory, absent for an archive, which is one
            // file and needs no agreement check (§42.3, §54.2).
            source_digest: table.source_digest(),
        })
    }

    fn page_size(&self, document: BackendDocumentId, page: usize) -> Result<PageSize> {
        let table = self.look_up(document)?;
        let at = table.locate(page).ok_or(PdfError::PageOutOfRange {
            page,
            count: table.len(),
        })?;
        // Header-only (§46.1): a folder of high-resolution photographs must
        // not stall on pixel data nobody has asked for yet.
        let (width, height) = decode::dimensions_at(&at)?;
        Ok(PageSize {
            width: width as f32,
            height: height as f32,
        })
    }

    fn render(&self, request: &RenderRequest, cancel: &dyn CancelSignal) -> Result<RenderedPage> {
        crate::pdf::render_via_render_into(self, request, cancel)
    }

    fn render_into(
        &self,
        request: &RenderRequest,
        target: &mut [u8],
        cancel: &dyn CancelSignal,
    ) -> Result<()> {
        request.validate()?;
        let table = self.look_up(request.document)?;
        if request.page >= table.len() {
            return Err(PdfError::PageOutOfRange {
                page: request.page,
                count: table.len(),
            });
        }
        let image = self.decoded(request.document, request.page, cancel)?;
        if cancel.is_cancelled() {
            return Err(PdfError::Cancelled);
        }
        decode::scale_into(
            &image,
            request.region,
            request.width,
            request.height,
            target,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::notes::Region;

    /// A tiny solid PNG, written through the same crate that reads it.
    fn write_png(path: &Path, width: u32, height: u32, colour: [u8; 4]) {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(colour));
        image.save(path).unwrap();
    }

    fn request(document: BackendDocumentId, page: usize) -> RenderRequest {
        RenderRequest {
            document,
            page,
            region: Region::FULL,
            width: 8,
            height: 8,
            with_annotations: false,
            full_size: None,
        }
    }

    #[test]
    fn a_directory_of_images_is_a_document_whose_pages_are_its_files() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("img2.png"), 40, 20, [1, 2, 3, 255]);
        write_png(&dir.path().join("img10.png"), 10, 10, [4, 5, 6, 255]);

        let mut backend = ImageBackend::new();
        let document = backend.open(dir.path()).unwrap();
        let metadata = backend.metadata(document).unwrap();

        assert_eq!(metadata.page_count, 2);
        assert_eq!(metadata.first_page_size.width, 40.0);
        assert_eq!(metadata.page_sizes.len(), 2);
        assert_eq!(metadata.page_sizes[1].width, 10.0);
        assert!(!metadata.page_sizes_sampled, "§46.3");
        assert!(metadata.metadata_text.is_empty(), "§46.5");
        assert!(metadata.source_digest.is_some(), "§42.3");
    }

    #[test]
    fn opening_a_file_opens_the_directory_it_is_in() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"), 4, 4, [0, 0, 0, 255]);
        write_png(&dir.path().join("b.png"), 4, 4, [0, 0, 0, 255]);

        let mut backend = ImageBackend::new();
        let document = backend.open(&dir.path().join("b.png")).unwrap();
        assert_eq!(backend.metadata(document).unwrap().page_count, 2);
    }

    #[test]
    fn a_page_renders_the_picture_that_is_at_that_index() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"), 4, 4, [10, 20, 30, 255]);
        write_png(&dir.path().join("b.png"), 4, 4, [40, 50, 60, 255]);

        let mut backend = ImageBackend::new();
        let document = backend.open(dir.path()).unwrap();
        let page = backend
            .render(&request(document, 1), &crate::pdf::NeverCancel)
            .unwrap();
        assert!(page.is_consistent());
        assert_eq!(&page.pixels[..4], &[40, 50, 60, 255]);
    }

    /// §49: a listed file that will not decode stays in the table, keeps its
    /// index, fails its own render, and leaves its neighbours renderable.
    #[test]
    fn a_corrupt_file_among_good_ones_is_still_a_page() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"), 4, 4, [10, 20, 30, 255]);
        std::fs::write(dir.path().join("b.png"), b"this is not a png").unwrap();
        write_png(&dir.path().join("c.png"), 4, 4, [70, 80, 90, 255]);

        let mut backend = ImageBackend::new();
        let document = backend.open(dir.path()).unwrap();
        let metadata = backend.metadata(document).unwrap();
        assert_eq!(metadata.page_count, 3, "still counted");

        let broken = backend.render(&request(document, 1), &crate::pdf::NeverCancel);
        assert!(
            matches!(broken, Err(PdfError::Render(_))),
            "§49.1: it fails its render rather than drawing a placeholder"
        );

        let neighbour = backend
            .render(&request(document, 2), &crate::pdf::NeverCancel)
            .unwrap();
        assert_eq!(
            &neighbour.pixels[..4],
            &[70, 80, 90, 255],
            "§49.2: still positioned, so its neighbours are where they were"
        );
    }

    #[test]
    fn an_empty_directory_opens_with_no_pages() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = ImageBackend::new();
        let document = backend.open(dir.path()).unwrap();
        assert_eq!(backend.metadata(document).unwrap().page_count, 0);
    }

    #[test]
    fn text_search_reports_unsupported_rather_than_no_matches() {
        // §48.2. The distinction is load-bearing: "this cannot be searched"
        // and "there are no matches" are different facts.
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"), 4, 4, [0, 0, 0, 255]);
        let mut backend = ImageBackend::new();
        let document = backend.open(dir.path()).unwrap();
        let query = pulpit_core::search::Query::new("anything", false, false);
        assert!(backend.find_text(document, &query, 0..1).is_err());
        assert!(backend.links(document, 0).unwrap().is_empty());
        assert!(backend.outline(document).unwrap().entries.is_empty());
    }

    #[test]
    fn one_decode_serves_every_frame_that_wants_the_page() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"), 64, 64, [1, 1, 1, 255]);
        let mut backend = ImageBackend::new();
        let document = backend.open(dir.path()).unwrap();

        for size in [8u32, 16, 32] {
            let mut request = request(document, 0);
            request.width = size;
            request.height = size;
            backend.render(&request, &crate::pdf::NeverCancel).unwrap();
        }
        let cache = backend.cache.lock().unwrap();
        assert_eq!(cache.len(), 1, "§47.1: decoded once, scaled three times");
    }

    #[test]
    fn closing_a_document_releases_its_decoded_pixels() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"), 32, 32, [1, 1, 1, 255]);
        let mut backend = ImageBackend::new();
        let document = backend.open(dir.path()).unwrap();
        backend
            .render(&request(document, 0), &crate::pdf::NeverCancel)
            .unwrap();
        backend.close(document);
        assert_eq!(backend.cache.lock().unwrap().bytes_used(), 0);
    }

    /// A comic archive holding two differently-shaped pages, one per
    /// chapter folder, plus things that are not pages.
    fn write_comic(path: &Path, kind: crate::images::ArchiveKind) {
        use std::io::Write;
        let page = |width: u32, height: u32, colour: [u8; 4]| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::RgbaImage::from_pixel(width, height, image::Rgba(colour))
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            bytes.into_inner()
        };
        let contents: Vec<(&str, Vec<u8>)> = vec![
            ("ch-1/page-10.png", page(30, 20, [90, 90, 90, 255])),
            ("ch-1/page-02.png", page(20, 30, [20, 20, 20, 255])),
            ("ComicInfo.xml", b"<ComicInfo/>".to_vec()),
        ];
        match kind {
            crate::images::ArchiveKind::Zip => {
                let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
                let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                for (name, bytes) in &contents {
                    writer.start_file(*name, options).unwrap();
                    writer.write_all(bytes).unwrap();
                }
                writer.finish().unwrap();
            }
            crate::images::ArchiveKind::Tar => {
                let mut builder = tar::Builder::new(std::fs::File::create(path).unwrap());
                for (name, bytes) in &contents {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(bytes.len() as u64);
                    header.set_mode(0o644);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, name, bytes.as_slice())
                        .unwrap();
                }
                builder.finish().unwrap();
            }
        }
    }

    /// `SPEC-reader-formats.md` §54.1: an archive is presented exactly as a
    /// directory — sorted entries, one image per page — and §54.3's flattening
    /// means the order is the sorted full path.
    #[test]
    fn a_comic_archive_is_a_document_whose_pages_are_its_entries() {
        for (name, kind) in [
            ("comic.cbz", crate::images::ArchiveKind::Zip),
            ("comic.cbt", crate::images::ArchiveKind::Tar),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(name);
            write_comic(&path, kind);

            let mut backend = ImageBackend::new();
            let document = backend.open(&path).unwrap();
            let metadata = backend.metadata(document).unwrap();

            assert_eq!(metadata.page_count, 2, "{name}: the XML is not a page");
            // page-02 before page-10: natural sort over the full path.
            assert_eq!(metadata.page_sizes[0].width, 20.0, "{name}");
            assert_eq!(metadata.page_sizes[1].width, 30.0, "{name}");
            assert!(!metadata.page_sizes_sampled, "{name}");
            assert!(metadata.metadata_text.is_empty(), "{name}");
            assert_eq!(
                metadata.source_digest, None,
                "{name}: §54.2 — one file, so there is nothing to agree about"
            );

            let frame = backend
                .render(&request(document, 1), &crate::pdf::NeverCancel)
                .unwrap();
            assert_eq!(&frame.pixels[..4], &[90, 90, 90, 255], "{name}");
        }
    }

    /// §54.5: nothing is extracted to disk. The archive is opened, its pages
    /// render, and the directory it sits in gains no files.
    #[test]
    fn an_archive_is_never_unpacked_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        write_comic(&path, crate::images::ArchiveKind::Zip);

        let mut backend = ImageBackend::new();
        let document = backend.open(&path).unwrap();
        backend
            .render(&request(document, 0), &crate::pdf::NeverCancel)
            .unwrap();

        let left_behind: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .collect();
        assert_eq!(left_behind, [std::ffi::OsString::from("comic.cbz")]);
    }

    /// §54.7 and §61.2: refused by name, and not as a damaged file.
    #[test]
    fn a_rar_comic_is_refused_by_name_rather_than_as_a_broken_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbr");
        std::fs::write(&path, b"Rar!\x1a\x07\x00").unwrap();

        let mut backend = ImageBackend::new();
        let error = backend.open(&path).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("RAR"), "{text}");
        assert!(
            !text.to_lowercase().contains("corrupt") && !text.to_lowercase().contains("damaged"),
            "{text}"
        );
    }

    #[test]
    fn a_page_that_will_not_decode_stays_in_an_archive_table_too() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        {
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            let mut good = std::io::Cursor::new(Vec::new());
            image::RgbaImage::from_pixel(8, 8, image::Rgba([7, 7, 7, 255]))
                .write_to(&mut good, image::ImageFormat::Png)
                .unwrap();
            writer.start_file("page-01.png", options).unwrap();
            writer.write_all(good.get_ref()).unwrap();
            writer.start_file("page-02.png", options).unwrap();
            writer.write_all(b"not a png at all").unwrap();
            writer.start_file("page-03.png", options).unwrap();
            writer.write_all(good.get_ref()).unwrap();
            writer.finish().unwrap();
        }

        let mut backend = ImageBackend::new();
        let document = backend.open(&path).unwrap();
        assert_eq!(
            backend.metadata(document).unwrap().page_count,
            3,
            "§49.2: still counted, so its neighbours keep their indices"
        );
        assert!(backend
            .render(&request(document, 1), &crate::pdf::NeverCancel)
            .is_err());
        assert_eq!(
            &backend
                .render(&request(document, 2), &crate::pdf::NeverCancel)
                .unwrap()
                .pixels[..4],
            &[7, 7, 7, 255]
        );
    }
}
