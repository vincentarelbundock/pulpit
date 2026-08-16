//! The PDFium document engine: native annotations in a real PDF.
//!
//! The other half of [`crate::pdf::pdfium::PdfiumBackend`], which renders. A
//! document is opened once, held by one worker execution context, mutated in
//! place and written out by Save As — never over its own source (A6).
//!
//! # What "editable" means here (§10.2)
//!
//! PDFium exposes annotation dictionary entries by *name*: it can be asked
//! whether `/InkList` is present and what it holds, but it cannot enumerate
//! the keys an annotation actually has. That has one consequence, and this
//! module is shaped around it:
//!
//! * **In-place edits are lossless.** Changing colour, opacity, geometry or
//!   contents mutates the existing dictionary, so every entry pulpit does not
//!   model survives untouched. Imported annotations of a modelled subtype are
//!   therefore [`AnnotationSupport::Editable`].
//! * **Deleting and restoring is lossless only for what can be read back.**
//!   The before-image carries the modelled content plus the annotation's own
//!   appearance stream, which is the entry that actually matters for how the
//!   mark looks in another viewer. A private entry of a kind PDFium cannot
//!   name is lost across a delete/undo cycle, and
//!   [`AnnotationBeforeImage::preserved`] carries what could be recovered
//!   rather than pretending it carried everything.
//!
//! Widening this is a matter of a dictionary-walking binding, not of a
//! different design; until there is one, the classification above is what the
//! engine can honestly promise.

use std::cell::RefCell;
use std::collections::HashMap;

use std::path::{Path, PathBuf};

use pdfium_render::prelude::*;
use pulpit_core::annotate::{
    AnnotationDraft, AnnotationId, AnnotationKind, FreeTextDraft, HighlightDraft, InkDraft,
    MarkStyle, StampDraft, StampMark, TextSource,
};
use pulpit_core::annotation::InkColor;
use pulpit_core::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect, PageRotation};

use crate::pdf::capabilities::{ActionKind, FormType};
use crate::pdf::pdfium::PdfiumBackend;
use crate::pdf::{BackendDocumentId, PdfBackend, PdfError};

use super::limits;
use super::model::{
    AnnotationBeforeImage, AnnotationContents, AnnotationSummary, AnnotationSupport,
    CompatibilityLevel, DocumentWarning, FormField, OpenDocumentInfo, SaveOptions, TextSelection,
    TextSelectionResult,
};
use super::{DocumentBackend, DocumentError, DocumentRevision, Result};

/// The key pulpit's own metadata lives under.
///
/// Namespaced because a second-class key in an annotation dictionary is shared
/// with every other producer that ever touched the file, and a bare `/Source`
/// would be somebody else's within a year.
const PULPIT_KEY: &str = "pulpit_Source";

/// PDFium's `/Subtype` enumerants, named rather than spelled as integers at
/// every call site.
mod subtype {
    use pdfium_render::prelude::*;

    pub const TEXT: FPDF_ANNOTATION_SUBTYPE = 1;
    pub const FREETEXT: FPDF_ANNOTATION_SUBTYPE = 3;
    pub const HIGHLIGHT: FPDF_ANNOTATION_SUBTYPE = 9;
    pub const INK: FPDF_ANNOTATION_SUBTYPE = 15;
    pub const STAMP: FPDF_ANNOTATION_SUBTYPE = 13;
    pub const WIDGET: FPDF_ANNOTATION_SUBTYPE = 20;
}

/// `FPDFANNOT_COLORTYPE_Color`: the annotation's own colour, `/C`, as opposed
/// to its interior colour. Spelled here because the binding crate's prelude
/// does not re-export the enumerants.
const COLOR_TYPE_COLOR: std::os::raw::c_uint = 0;

/// `FPDF_ANNOT_APPEARANCEMODE_NORMAL`: the `/AP` `/N` stream, the one every
/// viewer draws.
const APPEARANCE_NORMAL: std::os::raw::c_int = 0;

/// One open PDF, mutated in place.
///
/// The binding is *borrowed*, not owned: PDFium is bound once per process
/// (see [`PdfiumBackend::bind`]), and the worker that renders is the worker
/// that mutates — §5.1 makes document mutation a capability of the existing
/// worker rather than a new helper. Rendering and mutation therefore go
/// through the same open handle, which is what makes a frame drawn after a
/// commit contain the commit (A7).
pub struct PdfiumDocument<'a> {
    backend: &'a PdfiumBackend,
    document: BackendDocumentId,
    info: OpenDocumentInfo,
    source: Option<PathBuf>,
    /// Page geometry, measured once. A page's crop box and rotation do not
    /// change under an annotation edit, and re-measuring on every hit-test
    /// would reload the page each time.
    /// Interior mutability because measuring is a read: a hit-test, an
    /// enumeration and a precheck all need a page geometry through `&self`,
    /// and the alternative is measuring every page at open on the chance one
    /// of them is looked at.
    geometry: RefCell<HashMap<usize, PageGeometry>>,
}

// PDFium is not thread safe; the whole point of the worker process is that one
// document is owned by one execution context (§6).
unsafe impl Send for PdfiumDocument<'_> {}

impl<'a> PdfiumDocument<'a> {
    /// Open `source` for reading and annotating.
    ///
    /// Opening needs the binding mutably — it registers a handle — and
    /// everything afterwards does not, which is why this takes `&mut` and the
    /// document keeps a shared borrow.
    pub fn open(backend: &'a mut PdfiumBackend, source: &Path) -> Result<PdfiumDocument<'a>> {
        let document = backend
            .open(source)
            .map_err(|error| DocumentError::Backend(error.to_string()))?;
        let mut engine = PdfiumDocument {
            backend,
            document,
            info: OpenDocumentInfo {
                page_count: 0,
                level: CompatibilityLevel::AnnotateOnly,
                warnings: Vec::new(),
                first_page: PageGeometry::default(),
                has_form: false,
            },
            source: Some(source.to_path_buf()),
            geometry: RefCell::new(HashMap::new()),
        };
        engine.info = engine.survey()?;
        Ok(engine)
    }

    /// The backend, for rendering the same open document.
    pub fn backend(&self) -> &PdfiumBackend {
        self.backend
    }

    pub fn backend_document(&self) -> BackendDocumentId {
        self.document
    }

    /// Close the open document.
    ///
    /// The binding outlives it: PDFium is bound once per process, so closing
    /// a file and opening another is a document lifetime, not a library one.
    /// Taking `&mut PdfiumBackend` is what proves nothing else is still
    /// reading this document when it goes.
    pub fn close(self, backend: &mut PdfiumBackend) {
        backend.close(self.document);
    }

    /// What pulpit can tell the user about this document before they start
    /// (§3.4). Every warning here is evidence-based: a finding is never
    /// invented, and a document with none is reported as clean.
    fn survey(&mut self) -> Result<OpenDocumentInfo> {
        let bindings = self.backend.bindings();
        let handle = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;
        let page_count = unsafe { bindings.FPDF_GetPageCount(handle) }.max(0) as usize;

        let mut warnings = Vec::new();
        // A non-zero security-handler revision means the file is encrypted,
        // whether or not it needed a password to open.
        if unsafe { bindings.FPDF_GetSecurityHandlerRevision(handle) } >= 0 {
            warnings.push(DocumentWarning::Encrypted);
            // Bit 6 (value 32) of the permissions is "modify annotations".
            // PDFium reports all bits set for an unencrypted document, so this
            // is only consulted once encryption is established.
            let permissions = unsafe { bindings.FPDF_GetDocPermissions(handle) };
            if permissions & 0b10_0000 == 0 {
                warnings.push(DocumentWarning::MutationForbidden);
            }
        }

        // The same evidence the presenter's capability scan collects — read
        // once, interpreted here for a different question. A finding is never
        // invented: a document with no evidence of a feature is reported as
        // not having it.
        let evidence = PdfBackend::evidence(self.backend, self.document).unwrap_or_default();
        let mut has_form = false;
        let mut level = CompatibilityLevel::AnnotateOnly;
        match evidence.form_type {
            FormType::None => {}
            FormType::AcroForm => {
                has_form = true;
                level = CompatibilityLevel::Native;
            }
            FormType::Xfa => {
                has_form = true;
                warnings.push(DocumentWarning::XfaForm);
                level = CompatibilityLevel::NativeWithLimitations;
            }
        }
        let has_javascript = !evidence.document_javascript.is_empty()
            || evidence.pages.iter().any(|page| {
                page.annotations.iter().any(|annotation| {
                    annotation.has_additional_actions
                        || annotation.action == ActionKind::Unrecognised
                })
            });
        if has_javascript {
            warnings.push(DocumentWarning::JavaScript);
            if level == CompatibilityLevel::Native {
                level = CompatibilityLevel::NativeWithLimitations;
            }
        }

        // A9: an existing signature is detected and warned about *before* the
        // first mutation, not discovered after a save. PDFium exposes no
        // accessor for the signature dictionary without a form-fill
        // environment, so this is a bounded scan of the file's own bytes —
        // the same technique the capability scan already uses for `/Trans`.
        if self
            .source
            .as_deref()
            .map(carries_a_signature)
            .unwrap_or(false)
        {
            warnings.push(DocumentWarning::Signed);
        }

        let first_page = if page_count > 0 {
            self.measure(PageIndex(0))?
        } else {
            PageGeometry::default()
        };
        if page_count == 0 {
            level = CompatibilityLevel::Unsupported;
        }

        Ok(OpenDocumentInfo {
            page_count,
            level,
            warnings,
            first_page,
            has_form,
        })
    }

    /// Measure a page's canonical geometry from its crop box and `/Rotate`.
    fn measure(&self, page: PageIndex) -> Result<PageGeometry> {
        if let Some(geometry) = self.geometry.borrow().get(&page.get()) {
            return Ok(*geometry);
        }
        let bindings = self.backend.bindings();
        let geometry = self
            .backend
            .on_page(self.document, page.get(), |handle| {
                let mut left = 0.0f32;
                let mut bottom = 0.0f32;
                let mut right = 0.0f32;
                let mut top = 0.0f32;
                // A page without an explicit crop box crops to its media box,
                // and PDFium reports failure rather than substituting one.
                let has_crop = unsafe {
                    bindings.FPDFPage_GetCropBox(
                        handle,
                        &mut left,
                        &mut bottom,
                        &mut right,
                        &mut top,
                    )
                } != 0;
                if !has_crop {
                    let ok = unsafe {
                        bindings.FPDFPage_GetMediaBox(
                            handle,
                            &mut left,
                            &mut bottom,
                            &mut right,
                            &mut top,
                        )
                    } != 0;
                    if !ok {
                        // Neither box: fall back to the rendered size, which
                        // PDFium always has, so the page is still usable.
                        left = 0.0;
                        bottom = 0.0;
                        right = unsafe { bindings.FPDF_GetPageWidthF(handle) };
                        top = unsafe { bindings.FPDF_GetPageHeightF(handle) };
                    }
                }
                // PDFium reports rotation in *quarter turns* — 0, 1, 2, 3 —
                // not in degrees. Passing that straight to `from_degrees`
                // read every rotated page as unrotated, which put every mark
                // on one in the wrong place; the cross-viewer rotation test
                // is what found it.
                let quarters = unsafe { bindings.FPDFPage_GetRotation(handle) };
                let rotation = PageRotation::from_degrees(quarters.rem_euclid(4) * 90);
                Ok(PageGeometry::new(
                    left.min(right),
                    bottom.min(top),
                    (right - left).abs(),
                    (top - bottom).abs(),
                    rotation,
                    1.0,
                ))
            })
            .map_err(to_document_error)?;
        if !geometry.is_valid() {
            return Err(DocumentError::Backend(format!(
                "page {page} has no usable geometry"
            )));
        }
        self.geometry.borrow_mut().insert(page.get(), geometry);
        Ok(geometry)
    }

    /// A page's geometry, measured if this is the first time it is asked for.
    fn geometry_of(&self, page: PageIndex) -> Result<PageGeometry> {
        self.measure(page)
    }

    /// Run `f` over every annotation on a page, in `/Annots` order.
    fn on_annotations<T>(
        &self,
        page: PageIndex,
        mut f: impl FnMut(usize, FPDF_ANNOTATION, &PageGeometry) -> Option<T>,
    ) -> Result<Vec<T>> {
        let geometry = self.geometry_of(page)?;
        let bindings = self.backend.bindings();
        self.backend
            .on_page(self.document, page.get(), |handle| {
                let count = unsafe { bindings.FPDFPage_GetAnnotCount(handle) }.max(0) as usize;
                let count = count.min(limits::MAX_ANNOTATIONS_PER_PAGE);
                let mut collected = Vec::new();
                for index in 0..count {
                    let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index as i32) };
                    if annotation.is_null() {
                        continue;
                    }
                    if let Some(value) = f(index, annotation, &geometry) {
                        collected.push(value);
                    }
                    unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                }
                Ok(collected)
            })
            .map_err(to_document_error)
    }

    /// Find an annotation by identity, returning the page and index it sits
    /// at. Linear, because `/Annots` is an array and there is no index by
    /// `/NM` in a PDF; bounded by the page limit above.
    fn locate(&self, id: &AnnotationId) -> Result<(PageIndex, usize)> {
        for page in 0..self.info.page_count {
            let page = PageIndex(page);
            self.measure(page)?;
            let found = self.on_annotations(page, |index, annotation, _| {
                let name = self.string_value(annotation, "NM")?;
                (AnnotationId::imported(&name).as_ref() == Some(id)).then_some(index)
            })?;
            if let Some(index) = found.first() {
                return Ok((page, *index));
            }
        }
        Err(DocumentError::NoSuchAnnotation(id.clone()))
    }

    /// Every page measured, so [`Self::locate`] can search them.
    ///
    /// Measuring is what fills the geometry cache, and an annotation on a page
    /// nobody has looked at yet would otherwise be invisible to a search.
    fn measure_all(&self) -> Result<()> {
        for page in 0..self.info.page_count {
            self.measure(PageIndex(page))?;
        }
        Ok(())
    }

    fn string_value(&self, annotation: FPDF_ANNOTATION, key: &str) -> Option<String> {
        let bindings = self.backend.bindings();
        let length =
            unsafe { bindings.FPDFAnnot_GetStringValue(annotation, key, std::ptr::null_mut(), 0) };
        // Two bytes is the terminator alone: the key is absent or empty.
        if length <= 2 {
            return None;
        }
        let length = (length as usize).min(limits::MAX_TEXT_BYTES * 2 + 2);
        let mut buffer = vec![0u8; length];
        unsafe {
            bindings.FPDFAnnot_GetStringValue(
                annotation,
                key,
                buffer.as_mut_ptr() as *mut FPDF_WCHAR,
                length as std::os::raw::c_ulong,
            )
        };
        Some(decode_utf16(&buffer))
    }

    fn rect_of(&self, annotation: FPDF_ANNOTATION, geometry: &PageGeometry) -> PageRect {
        let bindings = self.backend.bindings();
        let mut rect = FS_RECTF {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        if unsafe { bindings.FPDFAnnot_GetRect(annotation, &mut rect) } == 0 {
            return PageRect::default();
        }
        geometry.rect_from_user_space([
            rect.left.min(rect.right),
            rect.bottom.min(rect.top),
            rect.left.max(rect.right),
            rect.bottom.max(rect.top),
        ])
    }

    fn style_of(&self, annotation: FPDF_ANNOTATION) -> MarkStyle {
        let bindings = self.backend.bindings();
        let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 255u32);
        let read = unsafe {
            bindings.FPDFAnnot_GetColor(
                annotation,
                COLOR_TYPE_COLOR,
                &mut r,
                &mut g,
                &mut b,
                &mut a,
            )
        } != 0;
        let mut style = MarkStyle::default();
        if read {
            style.color = InkColor::Custom {
                red: r.min(255) as u8,
                green: g.min(255) as u8,
                blue: b.min(255) as u8,
            };
            style.opacity = (a.min(255) as f32) / 255.0;
        }
        let (mut horizontal, mut vertical, mut width) = (0.0f32, 0.0f32, 0.0f32);
        if unsafe {
            bindings.FPDFAnnot_GetBorder(annotation, &mut horizontal, &mut vertical, &mut width)
        } != 0
            && width > 0.0
        {
            style.width = width;
        }
        style.sanitised()
    }

    fn ink_path(&self, annotation: FPDF_ANNOTATION, geometry: &PageGeometry) -> Vec<PagePoint> {
        let bindings = self.backend.bindings();
        let paths = unsafe { bindings.FPDFAnnot_GetInkListCount(annotation) } as usize;
        let mut points = Vec::new();
        for path in 0..paths.min(64) {
            let count = unsafe {
                bindings.FPDFAnnot_GetInkListPath(
                    annotation,
                    path as std::os::raw::c_ulong,
                    std::ptr::null_mut(),
                    0,
                )
            } as usize;
            if count == 0 || count > limits::MAX_POINTS_PER_INK {
                continue;
            }
            let mut buffer = vec![FS_POINTF { x: 0.0, y: 0.0 }; count];
            unsafe {
                bindings.FPDFAnnot_GetInkListPath(
                    annotation,
                    path as std::os::raw::c_ulong,
                    buffer.as_mut_ptr(),
                    count as std::os::raw::c_ulong,
                )
            };
            points.extend(
                buffer
                    .into_iter()
                    .map(|point| geometry.from_user_space(point.x, point.y)),
            );
            if points.len() >= limits::MAX_POINTS_PER_INK {
                points.truncate(limits::MAX_POINTS_PER_INK);
                break;
            }
        }
        points
    }

    fn quads_of(&self, annotation: FPDF_ANNOTATION, geometry: &PageGeometry) -> Vec<PageQuad> {
        let bindings = self.backend.bindings();
        if unsafe { bindings.FPDFAnnot_HasAttachmentPoints(annotation) } == 0 {
            return Vec::new();
        }
        let count = unsafe { bindings.FPDFAnnot_CountAttachmentPoints(annotation) };
        let count = count.min(limits::MAX_QUADS_PER_ANNOTATION);
        let mut quads = Vec::with_capacity(count);
        for index in 0..count {
            let mut quad = FS_QUADPOINTSF {
                x1: 0.0,
                y1: 0.0,
                x2: 0.0,
                y2: 0.0,
                x3: 0.0,
                y3: 0.0,
                x4: 0.0,
                y4: 0.0,
            };
            if unsafe { bindings.FPDFAnnot_GetAttachmentPoints(annotation, index, &mut quad) } == 0
            {
                continue;
            }
            quads.push(geometry.quad_from_user_space([
                quad.x1, quad.y1, quad.x2, quad.y2, quad.x3, quad.y3, quad.x4, quad.y4,
            ]));
        }
        quads
    }

    /// Turn one annotation into a summary, or `None` for a form widget, which
    /// is classified separately and never offered to the annotation editor
    /// (§8.6).
    fn summarise(
        &self,
        annotation: FPDF_ANNOTATION,
        page: PageIndex,
        geometry: &PageGeometry,
    ) -> Option<AnnotationSummary> {
        let bindings = self.backend.bindings();
        let subtype = unsafe { bindings.FPDFAnnot_GetSubtype(annotation) };
        if subtype == subtype::WIDGET {
            return None;
        }
        let kind = match subtype {
            subtype::INK => AnnotationKind::Ink,
            subtype::HIGHLIGHT => AnnotationKind::Highlight,
            subtype::FREETEXT => AnnotationKind::FreeText,
            subtype::TEXT => AnnotationKind::Note,
            subtype::STAMP => AnnotationKind::Stamp,
            _ => AnnotationKind::Other,
        };
        let name = self.string_value(annotation, "NM");
        let id = name
            .as_deref()
            .and_then(AnnotationId::imported)
            // A missing or unusable `/NM` gets a session identity, and the
            // annotation is written a real one the first time it is modified
            // or saved (A3). The session identity is derived from where it
            // sits, which is stable for as long as nothing renumbers.
            .unwrap_or_else(|| {
                AnnotationId::imported(&format!("session-{}-{:p}", page.get(), annotation))
                    .expect("a derived name is well formed")
            });

        let text = self
            .string_value(annotation, "Contents")
            .unwrap_or_default();
        let truncated = text.len() > limits::MAX_TEXT_BYTES;
        let mut text = text;
        if truncated {
            text.truncate(
                (0..=limits::MAX_TEXT_BYTES)
                    .rev()
                    .find(|at| text.is_char_boundary(*at))
                    .unwrap_or(0),
            );
        }

        let support = match kind {
            // A modelled subtype is edited *in place*, so every entry pulpit
            // does not know about survives the edit (see the module note).
            AnnotationKind::Ink
            | AnnotationKind::Highlight
            | AnnotationKind::FreeText
            | AnnotationKind::Note
            | AnnotationKind::Stamp => AnnotationSupport::Editable,
            AnnotationKind::Other => AnnotationSupport::Unsupported,
        };

        Some(AnnotationSummary {
            id,
            page,
            kind,
            bounds: self.rect_of(annotation, geometry),
            style: self.style_of(annotation),
            contents: AnnotationContents {
                text,
                truncated,
                pulpit_source: self.string_value(annotation, PULPIT_KEY),
            },
            support,
            revision: DocumentRevision::INITIAL,
            path: if kind == AnnotationKind::Ink {
                self.ink_path(annotation, geometry)
            } else {
                Vec::new()
            },
            quads: if kind == AnnotationKind::Highlight {
                self.quads_of(annotation, geometry)
            } else {
                Vec::new()
            },
            geometry_elided: false,
        })
    }

    /// Write a draft's content into a freshly created or existing annotation.
    fn write_draft(
        &self,
        page_handle: FPDF_PAGE,
        annotation: FPDF_ANNOTATION,
        draft: &AnnotationDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        let style = draft.style();
        let (red, green, blue) = style.color.rgb();
        let alpha = (style.opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
        unsafe {
            bindings.FPDFAnnot_SetColor(
                annotation,
                COLOR_TYPE_COLOR,
                (red * 255.0).round().clamp(0.0, 255.0) as u32,
                (green * 255.0).round().clamp(0.0, 255.0) as u32,
                (blue * 255.0).round().clamp(0.0, 255.0) as u32,
                alpha,
            )
        };

        if let Some(bounds) = draft.bounds() {
            let rect = geometry.rect_to_user_space(bounds);
            let rect = FS_RECTF {
                left: rect[0],
                bottom: rect[1],
                right: rect[2],
                top: rect[3],
            };
            unsafe { bindings.FPDFAnnot_SetRect(annotation, &rect) };
        }

        match draft {
            AnnotationDraft::Ink(ink) => self.write_ink(annotation, ink, geometry)?,
            AnnotationDraft::Highlight(highlight) => {
                self.write_highlight(annotation, highlight, geometry)?
            }
            AnnotationDraft::FreeText(free) => self.write_free_text(annotation, free)?,
            AnnotationDraft::Note(note) => {
                set_string(bindings, annotation, "Contents", &note.text)?;
            }
            AnnotationDraft::Stamp(stamp) => {
                self.write_stamp(page_handle, annotation, stamp, geometry)?
            }
        }
        // The border width is what a viewer regenerating an appearance uses,
        // so it is written even though pulpit supplies its own `/AP`.
        unsafe { bindings.FPDFAnnot_SetBorder(annotation, 0.0, 0.0, style.width) };
        Ok(())
    }

    fn write_ink(
        &self,
        annotation: FPDF_ANNOTATION,
        ink: &InkDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        // A replace rewrites the geometry rather than appending to it: an
        // `/InkList` that grew by one path per edit would draw every earlier
        // version of the stroke as well as the current one.
        unsafe { bindings.FPDFAnnot_RemoveInkList(annotation) };
        let points: Vec<FS_POINTF> = ink
            .points
            .iter()
            .map(|point| {
                let (x, y) = geometry.to_user_space(point.at);
                FS_POINTF { x, y }
            })
            .collect();
        if points.is_empty() {
            return Err(DocumentError::Backend("an empty stroke".into()));
        }
        let added =
            unsafe { bindings.FPDFAnnot_AddInkStroke(annotation, points.as_ptr(), points.len()) };
        if added < 0 {
            return Err(DocumentError::Backend(
                "PDFium refused the ink stroke".into(),
            ));
        }
        Ok(())
    }

    fn write_highlight(
        &self,
        annotation: FPDF_ANNOTATION,
        highlight: &HighlightDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        let quads: Vec<FS_QUADPOINTSF> = highlight
            .quads
            .iter()
            .map(|quad| {
                let v = geometry.quad_to_user_space(*quad);
                FS_QUADPOINTSF {
                    x1: v[0],
                    y1: v[1],
                    x2: v[2],
                    y2: v[3],
                    x3: v[4],
                    y3: v[5],
                    x4: v[6],
                    y4: v[7],
                }
            })
            .collect();
        if quads.is_empty() {
            return Err(DocumentError::Backend("a highlight with no quads".into()));
        }
        // PDFium can set an existing quad by index and append past the end,
        // but it cannot shorten the list. So the runs the annotation already
        // has are overwritten in place and the rest appended — which is exact
        // for a new annotation (none existing) and for a re-marked one whose
        // selection grew, and leaves a stale tail only when a selection
        // shrank. The tail is collapsed onto the last real run rather than
        // left describing text that is no longer selected.
        let existing = unsafe { bindings.FPDFAnnot_CountAttachmentPoints(annotation) };
        let last = *quads.last().expect("checked above");
        for (index, quad) in quads.iter().enumerate() {
            let ok = if index < existing {
                unsafe { bindings.FPDFAnnot_SetAttachmentPoints(annotation, index, quad) }
            } else {
                unsafe { bindings.FPDFAnnot_AppendAttachmentPoints(annotation, quad) }
            };
            if ok == 0 {
                return Err(DocumentError::Backend(
                    "PDFium refused the highlight geometry".into(),
                ));
            }
        }
        for index in quads.len()..existing {
            unsafe { bindings.FPDFAnnot_SetAttachmentPoints(annotation, index, &last) };
        }
        // §7.2: the selected text goes into `/Contents` as an accessibility
        // and search fallback, so the mark survives re-extraction.
        set_string(bindings, annotation, "Contents", &highlight.text)?;
        Ok(())
    }

    fn write_free_text(&self, annotation: FPDF_ANNOTATION, free: &FreeTextDraft) -> Result<()> {
        let bindings = self.backend.bindings();
        set_string(bindings, annotation, "Contents", &free.text)?;
        // `/DA` names the font and colour a viewer uses when it regenerates
        // the appearance. Helvetica because it is one of the fourteen standard
        // faces every conforming viewer has without embedding.
        let (r, g, b) = free.style.color.rgb();
        let da = format!(
            "/Helv {:.2} Tf {r:.3} {g:.3} {b:.3} rg",
            free.style.font_size
        );
        set_string(bindings, annotation, "DA", &da)?;
        // `/Q` 0 is left-aligned, which is what the editor writes.
        if free.source == TextSource::Typst {
            // §7.4: the source is kept in pulpit's namespaced entry so the
            // annotation reopens for editing, while other viewers show the
            // appearance and are not asked to understand Typst.
            set_string(bindings, annotation, PULPIT_KEY, &free.text)?;
        }
        Ok(())
    }

    fn write_stamp(
        &self,
        page_handle: FPDF_PAGE,
        annotation: FPDF_ANNOTATION,
        stamp: &StampDraft,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let bindings = self.backend.bindings();
        // The `/Contents` of a stamp is its description, which is what a
        // screen reader announces; §7.6 says these are called marks and never
        // cryptographic signatures.
        set_string(bindings, annotation, "Contents", stamp.mark.label())?;

        if let StampMark::Image {
            pixel_width,
            pixel_height,
            rgba,
        } = &stamp.mark
        {
            self.write_stamp_image(
                page_handle,
                annotation,
                Picture {
                    rect: stamp.rect,
                    pixel_width: *pixel_width,
                    pixel_height: *pixel_height,
                    rgba,
                },
                geometry,
            )?;
        }
        Ok(())
    }

    /// Put a decoded picture into a `/Stamp` as an image XObject.
    ///
    /// The bytes are carried in and embedded; no path to the source file goes
    /// into the annotation (§7.6, §12). An annotation that pointed at a file
    /// on disk would either break when the file moved or read a file the
    /// *document* chose, and neither is something a signature should do.
    fn write_stamp_image(
        &self,
        page_handle: FPDF_PAGE,
        annotation: FPDF_ANNOTATION,
        picture: Picture<'_>,
        geometry: &PageGeometry,
    ) -> Result<()> {
        let Picture {
            rect,
            pixel_width,
            pixel_height,
            rgba,
        } = picture;
        let bindings = self.backend.bindings();
        let document = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;

        // Bounded before anything is allocated (A8). The draft's own
        // validation has already checked this; checking again here is the
        // rule that a value crossing into a C library is checked at the
        // boundary it crosses.
        let pixels = u64::from(pixel_width) * u64::from(pixel_height);
        if pixel_width == 0
            || pixel_height == 0
            || pixels > StampMark::MAX_PIXELS
            || rgba.len() != pixel_width as usize * pixel_height as usize * 4
        {
            return Err(DocumentError::Backend("a malformed stamp picture".into()));
        }

        // PDFium reads BGRA for `FPDFBitmap_BGRA`, and the caller hands over
        // RGBA, so the channels are swapped into a buffer this function owns
        // for as long as PDFium is looking at it.
        let mut bgra = Vec::with_capacity(rgba.len());
        for chunk in rgba.chunks_exact(4) {
            bgra.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
        }

        /// `FPDFBitmap_BGRA`.
        const BGRA: std::os::raw::c_int = 4;
        let bitmap = unsafe {
            bindings.FPDFBitmap_CreateEx(
                pixel_width as i32,
                pixel_height as i32,
                BGRA,
                bgra.as_mut_ptr() as *mut std::ffi::c_void,
                pixel_width as i32 * 4,
            )
        };
        if bitmap.is_null() {
            return Err(DocumentError::Backend(
                "PDFium refused the stamp picture".into(),
            ));
        }

        let image = unsafe { bindings.FPDFPageObj_NewImageObj(document) };
        if image.is_null() {
            unsafe { bindings.FPDFBitmap_Destroy(bitmap) };
            return Err(DocumentError::Backend(
                "PDFium refused to make an image object".into(),
            ));
        }

        let outcome = (|| {
            // The page the object will live on: PDFium wants it while the
            // bitmap is attached, and passing nothing here is what made this
            // fall over rather than fail.
            let mut pages = [page_handle];
            if unsafe { bindings.FPDFImageObj_SetBitmap(pages.as_mut_ptr(), 1, image, bitmap) } == 0
            {
                return Err(DocumentError::Backend(
                    "PDFium refused the stamp's bitmap".into(),
                ));
            }
            // An image object is a unit square until it is told otherwise, so
            // the matrix is what places and sizes it. In PDF user space, which
            // is where a page object lives.
            let rect = geometry.rect_to_user_space(rect);
            let matrix = FS_MATRIX {
                a: rect[2] - rect[0],
                b: 0.0,
                c: 0.0,
                d: rect[3] - rect[1],
                e: rect[0],
                f: rect[1],
            };
            if unsafe { bindings.FPDFPageObj_SetMatrix(image, &matrix) } == 0 {
                return Err(DocumentError::Backend(
                    "PDFium refused the stamp's placement".into(),
                ));
            }
            if unsafe { bindings.FPDFAnnot_AppendObject(annotation, image) } == 0 {
                return Err(DocumentError::Backend(
                    "PDFium refused to put the picture in the annotation".into(),
                ));
            }
            Ok(())
        })();

        // The annotation owns the object once it has been appended; before
        // that it is this function's to destroy.
        if outcome.is_err() {
            unsafe { bindings.FPDFPageObj_Destroy(image) };
        }
        unsafe { bindings.FPDFBitmap_Destroy(bitmap) };
        outcome
    }
}

/// Does this file carry a cryptographic signature?
///
/// A byte scan, because the alternative is a form-fill environment that this
/// engine does not otherwise need. It errs towards saying yes: a false warning
/// costs the user one dismissal, and a missed one costs them the belief that a
/// signature survived their edits, which A9 exists to prevent.
fn carries_a_signature(source: &Path) -> bool {
    /// Signature dictionaries live in the trailer's neighbourhood, but an
    /// incrementally updated file can carry them anywhere; 32 MiB covers every
    /// document a person opens by hand and bounds the read.
    const MAX_SCAN_BYTES: u64 = 32 << 20;

    let Ok(metadata) = std::fs::metadata(source) else {
        return false;
    };
    if metadata.len() > MAX_SCAN_BYTES {
        return false;
    }
    let Ok(bytes) = std::fs::read(source) else {
        return false;
    };
    // `/Type /Sig` with any amount of whitespace between the two names, and
    // `/FT /Sig` for the field that carries it.
    contains_pdf_name_pair(&bytes, b"/Type", b"/Sig")
        || contains_pdf_name_pair(&bytes, b"/FT", b"/Sig")
}

fn contains_pdf_name_pair(bytes: &[u8], first: &[u8], second: &[u8]) -> bool {
    let mut at = 0usize;
    while let Some(found) = find(&bytes[at..], first) {
        let after = at + found + first.len();
        let tail = &bytes[after..(after + 8).min(bytes.len())];
        let trimmed = tail
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .unwrap_or(0);
        if tail[trimmed..].starts_with(second) {
            return true;
        }
        at = after;
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A decoded picture and where it goes, so the call that embeds one takes a
/// subject rather than a list of loose measurements.
struct Picture<'a> {
    rect: PageRect,
    pixel_width: u32,
    pixel_height: u32,
    rgba: &'a [u8],
}

/// Enough of the annotation to put it back, as far as PDFium can be asked.
fn capture_appearance(
    bindings: &dyn PdfiumLibraryBindings,
    annotation: FPDF_ANNOTATION,
) -> Vec<u8> {
    let length =
        unsafe { bindings.FPDFAnnot_GetAP(annotation, APPEARANCE_NORMAL, std::ptr::null_mut(), 0) };
    if length <= 2 || length as usize > limits::MAX_APPEARANCE_BYTES {
        return Vec::new();
    }
    let mut buffer = vec![0u8; length as usize];
    unsafe {
        bindings.FPDFAnnot_GetAP(
            annotation,
            APPEARANCE_NORMAL,
            buffer.as_mut_ptr() as *mut FPDF_WCHAR,
            length as std::os::raw::c_ulong,
        )
    };
    buffer
}

fn set_string(
    bindings: &dyn PdfiumLibraryBindings,
    annotation: FPDF_ANNOTATION,
    key: &str,
    value: &str,
) -> Result<()> {
    if value.len() > limits::MAX_TEXT_BYTES {
        return Err(DocumentError::Limit(limits::LimitExceeded {
            what: "annotation text",
            limit: limits::MAX_TEXT_BYTES,
        }));
    }
    if unsafe { bindings.FPDFAnnot_SetStringValue_str(annotation, key, value) } == 0 {
        return Err(DocumentError::Backend(format!(
            "PDFium refused to write /{key}"
        )));
    }
    Ok(())
}

/// PDFium hands text back as UTF-16LE with a terminator.
fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn to_document_error(error: PdfError) -> DocumentError {
    match error {
        PdfError::PageOutOfRange { page, count } => DocumentError::NoSuchPage { page, count },
        other => DocumentError::Backend(other.to_string()),
    }
}

impl DocumentBackend for PdfiumDocument<'_> {
    fn info(&self) -> &OpenDocumentInfo {
        &self.info
    }

    fn page_geometry(&self, page: PageIndex) -> Result<PageGeometry> {
        if page.get() >= self.info.page_count {
            return Err(DocumentError::NoSuchPage {
                page: page.get(),
                count: self.info.page_count,
            });
        }
        self.measure(page)
    }

    fn annotations(&self, page: PageIndex) -> Result<Vec<AnnotationSummary>> {
        self.on_annotations(page, |_, annotation, geometry| {
            self.summarise(annotation, page, geometry)
        })
    }

    fn annotation(&self, id: &AnnotationId) -> Result<AnnotationSummary> {
        let (page, index) = self.locate(id)?;
        let summaries = self.annotations(page)?;
        summaries
            .into_iter()
            .nth(index)
            .filter(|summary| summary.id == *id)
            // The index is an `/Annots` index and the summary list skips form
            // widgets, so the two can disagree; the name is the identity.
            .or_else(|| {
                self.annotations(page)
                    .ok()?
                    .into_iter()
                    .find(|summary| summary.id == *id)
            })
            .ok_or_else(|| DocumentError::NoSuchAnnotation(id.clone()))
    }

    fn create(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary> {
        let page = draft.page();
        let geometry = self.measure(page)?;
        let subtype = match draft.kind() {
            AnnotationKind::Ink => subtype::INK,
            AnnotationKind::Highlight => subtype::HIGHLIGHT,
            AnnotationKind::FreeText => subtype::FREETEXT,
            AnnotationKind::Note => subtype::TEXT,
            AnnotationKind::Stamp => subtype::STAMP,
            AnnotationKind::Other => {
                return Err(DocumentError::Backend(
                    "an annotation of no known kind".into(),
                ))
            }
        };
        let bindings = self.backend.bindings();
        let document = self.document;
        // All of it inside one loaded page: creating the annotation, writing
        // it and generating the page's content. A stamp's picture is a page
        // object and PDFium wants the page it belongs to while it is being
        // attached, so the handle has to be in scope for the whole of it.
        self.backend
            .on_page(document, page.get(), |handle| {
                let annotation = unsafe { bindings.FPDFPage_CreateAnnot(handle, subtype) };
                if annotation.is_null() {
                    return Err(PdfError::Render(
                        "PDFium refused to create the annotation".into(),
                    ));
                }
                let outcome = (|| {
                    set_string(bindings, annotation, "NM", id.as_str())?;
                    self.write_draft(handle, annotation, draft, &geometry)
                })();
                unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                outcome.map_err(|error| PdfError::Render(error.to_string()))?;

                // Content generation is what commits the new annotation's
                // appearance into the page; without it the mark is in
                // `/Annots` and invisible.
                unsafe { bindings.FPDFPage_GenerateContent(handle) };
                Ok(())
            })
            .map_err(to_document_error)?;

        self.annotation(id)
    }

    fn replace(&mut self, id: &AnnotationId, draft: &AnnotationDraft) -> Result<AnnotationSummary> {
        self.measure_all()?;
        let (page, index) = self.locate(id)?;
        if draft.page() != page {
            return Err(DocumentError::Backend(
                "an annotation cannot be replaced onto another page".into(),
            ));
        }
        let geometry = self.measure(page)?;
        let bindings = self.backend.bindings();
        let document = self.document;
        self.backend
            .on_page(document, page.get(), |handle| {
                let annotation = unsafe { bindings.FPDFPage_GetAnnot(handle, index as i32) };
                if annotation.is_null() {
                    return Err(PdfError::Render("the annotation went away".into()));
                }
                let outcome = self.write_draft(handle, annotation, draft, &geometry);
                unsafe { bindings.FPDFPage_CloseAnnot(annotation) };
                unsafe { bindings.FPDFPage_GenerateContent(handle) };
                outcome.map_err(|error| PdfError::Render(error.to_string()))
            })
            .map_err(to_document_error)?;
        self.annotation(id)
    }

    fn delete(&mut self, id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        self.measure_all()?;
        let before = self.before_image(id)?;
        let (page, index) = self.locate(id)?;
        let bindings = self.backend.bindings();
        let document = self.document;
        self.backend
            .on_page(document, page.get(), |handle| {
                if unsafe { bindings.FPDFPage_RemoveAnnot(handle, index as i32) } == 0 {
                    return Err(PdfError::Render("PDFium refused the deletion".into()));
                }
                unsafe { bindings.FPDFPage_GenerateContent(handle) };
                Ok(())
            })
            .map_err(to_document_error)?;
        Ok(before)
    }

    fn restore(
        &mut self,
        id: &AnnotationId,
        before: &AnnotationBeforeImage,
    ) -> Result<AnnotationSummary> {
        let draft = before
            .draft
            .clone()
            .ok_or_else(|| DocumentError::NotEditable(id.clone()))?;
        self.measure_all()?;
        // Restoring over an annotation that is still there is the undo of a
        // replace; restoring one that is gone is the undo of a delete.
        if self.locate(id).is_ok() {
            return self.replace(id, &draft);
        }
        self.create(id, &draft)
    }

    fn before_image(&self, id: &AnnotationId) -> Result<AnnotationBeforeImage> {
        let (page, _) = self.locate(id)?;
        let summary = self.annotation(id)?;
        let geometry = self.geometry_of(page)?;
        let _ = geometry;
        // The same conversion the editor uses to build a replacement, so an
        // undo puts back exactly what a replace would have written.
        let draft = summary.to_draft();
        // The appearance stream is the entry that decides how the mark looks
        // elsewhere, so it is the one worth carrying even though the general
        // dictionary cannot be walked (see the module note).
        let preserved = self
            .on_annotations(page, |_, annotation, _| {
                let name = self.string_value(annotation, "NM")?;
                (AnnotationId::imported(&name).as_ref() == Some(id))
                    .then(|| capture_appearance(self.backend.bindings(), annotation))
            })?
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok(AnnotationBeforeImage {
            page,
            draft,
            preserved,
        })
    }

    fn outline(&self) -> Result<pulpit_core::navigation::Outline> {
        PdfBackend::outline(self.backend, self.document).map_err(to_document_error)
    }

    fn fields(&self) -> Result<Vec<FormField>> {
        // Reading fields needs the form-fill environment, which is initialised
        // per document; until the write path exists (§8.6) an inspector over
        // fields nobody can change would be a control that does nothing, so
        // this build reports none rather than a read-only list.
        Ok(Vec::new())
    }

    fn set_field(&mut self, name: &str, _value: &str) -> Result<String> {
        Err(DocumentError::NoSuchField(name.to_string()))
    }

    fn field_value(&self, name: &str) -> Result<String> {
        Err(DocumentError::NoSuchField(name.to_string()))
    }

    fn select_text(
        &self,
        page: PageIndex,
        selection: TextSelection,
    ) -> Result<TextSelectionResult> {
        let geometry = self.geometry_of(page)?;
        let bindings = self.backend.bindings();
        self.backend
            .on_page(self.document, page.get(), |handle| {
                let text_page = unsafe { bindings.FPDFText_LoadPage(handle) };
                if text_page.is_null() {
                    // No text layer is an empty result, not a failure (§6.3).
                    return Ok(TextSelectionResult::default());
                }
                let result = resolve_selection(bindings, text_page, &geometry, selection);
                unsafe { bindings.FPDFText_ClosePage(text_page) };
                Ok(result)
            })
            .map_err(to_document_error)
    }

    fn write_to(&mut self, destination: &Path, _options: SaveOptions) -> Result<u64> {
        let handle = self
            .backend
            .document_handle(self.document)
            .map_err(to_document_error)?;
        let bytes = self
            .backend
            .save_to_memory(handle)
            .map_err(|error| DocumentError::Save(error.to_string()))?;
        crate::pdf::pdfium::write_atomically(destination, &bytes)
            .map_err(|error| DocumentError::Save(error.to_string()))?;
        Ok(bytes.len() as u64)
    }

    fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Rasterise through the same backend that renders the presenter's
    /// slides, from the document this engine holds.
    ///
    /// The same handle, deliberately: a frame drawn after a commit contains
    /// the commit (A7), which a separate read-only copy of the file could not
    /// promise. Annotations are drawn, because a page rendered without them
    /// would be a page with the user's marks missing.
    fn render_page(&self, page: PageIndex, width: u32, height: u32, rgba: &mut [u8]) -> Result<()> {
        let request = crate::pdf::RenderRequest {
            document: self.document,
            page: page.get(),
            region: pulpit_core::notes::Region::FULL,
            width,
            height,
            with_annotations: true,
        };
        request.validate().map_err(to_document_error)?;
        PdfBackend::render_into(self.backend, &request, rgba, &crate::pdf::NeverCancel)
            .map_err(to_document_error)
    }
}

/// Turn a canonical selection into the runs of text it covers.
fn resolve_selection(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    geometry: &PageGeometry,
    selection: TextSelection,
) -> TextSelectionResult {
    let count = unsafe { bindings.FPDFText_CountChars(text_page) };
    if count <= 0 {
        return TextSelectionResult::default();
    }

    let index_at = |point: PagePoint| -> Option<i32> {
        let (x, y) = geometry.to_user_space(point);
        // A generous tolerance: a click between two lines should land on one
        // of them rather than on nothing.
        let index = unsafe {
            bindings.FPDFText_GetCharIndexAtPos(text_page, x as f64, y as f64, 12.0, 12.0)
        };
        (index >= 0).then_some(index)
    };

    let (start, end) = match selection {
        TextSelection::Range { anchor, head } => {
            let (Some(a), Some(b)) = (index_at(anchor), index_at(head)) else {
                return TextSelectionResult::default();
            };
            (a.min(b), a.max(b))
        }
        TextSelection::Word { at } => {
            let Some(index) = index_at(at) else {
                return TextSelectionResult::default();
            };
            expand(bindings, text_page, index, count, |c| {
                c.is_alphanumeric() || c == '\'' || c == '-'
            })
        }
        TextSelection::Line { at } => {
            let Some(index) = index_at(at) else {
                return TextSelectionResult::default();
            };
            expand(bindings, text_page, index, count, |c| {
                c != '\n' && c != '\r'
            })
        }
    };
    let length = (end - start + 1).max(0);
    if length == 0 {
        return TextSelectionResult::default();
    }

    // `/QuadPoints` is emitted per run rather than per glyph (§7.2), and
    // PDFium's rect list is exactly that: one rectangle per contiguous run.
    let rects = unsafe { bindings.FPDFText_CountRects(text_page, start, length) };
    let mut quads = Vec::new();
    for index in 0..rects.max(0).min(limits::MAX_QUADS_PER_SELECTION as i32) {
        let (mut left, mut top, mut right, mut bottom) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        if unsafe {
            bindings.FPDFText_GetRect(
                text_page,
                index,
                &mut left,
                &mut top,
                &mut right,
                &mut bottom,
            )
        } == 0
        {
            continue;
        }
        let a = geometry.from_user_space(left as f32, top as f32);
        let b = geometry.from_user_space(right as f32, bottom as f32);
        let quad = PageQuad::from_rect(PageRect::new(
            a.x.min(b.x),
            a.y.min(b.y),
            a.x.max(b.x),
            a.y.max(b.y),
        ));
        if !quad.is_degenerate() {
            quads.push(quad);
        }
    }

    let mut buffer = vec![0u16; (length as usize + 1).min(limits::MAX_TEXT_BYTES)];
    let written = unsafe {
        bindings.FPDFText_GetText(text_page, start, length, buffer.as_mut_ptr() as *mut _)
    };
    let text = if written > 0 {
        String::from_utf16_lossy(&buffer[..(written as usize - 1).min(buffer.len())])
    } else {
        String::new()
    };

    TextSelectionResult {
        quads,
        text,
        truncated: length as usize > limits::MAX_TEXT_BYTES,
    }
}

/// Grow a selection outwards from `index` while `keep` holds.
fn expand(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    index: i32,
    count: i32,
    keep: impl Fn(char) -> bool,
) -> (i32, i32) {
    let character = |at: i32| -> Option<char> {
        let unicode = unsafe { bindings.FPDFText_GetUnicode(text_page, at) };
        char::from_u32(unicode)
    };
    let mut start = index;
    while start > 0 {
        match character(start - 1) {
            Some(c) if keep(c) => start -= 1,
            _ => break,
        }
    }
    let mut end = index;
    while end + 1 < count {
        match character(end + 1) {
            Some(c) if keep(c) => end += 1,
            _ => break,
        }
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_from_pdfium_stops_at_the_terminator() {
        // "hi" followed by the terminator and trailing slack, which is what a
        // caller-sized buffer looks like coming back.
        let bytes = [b'h', 0, b'i', 0, 0, 0, 0xff, 0xff];
        assert_eq!(decode_utf16(&bytes), "hi");
        assert_eq!(decode_utf16(&[]), "");
        assert_eq!(decode_utf16(&[0, 0]), "");
    }

    #[test]
    fn a_page_out_of_range_keeps_its_shape_through_the_error_conversion() {
        let converted = to_document_error(PdfError::PageOutOfRange { page: 9, count: 3 });
        assert!(matches!(
            converted,
            DocumentError::NoSuchPage { page: 9, count: 3 }
        ));
        assert!(matches!(
            to_document_error(PdfError::Render("boom".into())),
            DocumentError::Backend(_)
        ));
    }
}
