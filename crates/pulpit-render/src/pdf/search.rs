//! Finding a string in a page's text layer, once.
//!
//! Both PDFium-backed paths search: the document engine, for the reader, and
//! the render backend, for the presenter. They are separate processes holding
//! separate handles to the same file, so what they must *not* be is separate
//! implementations — a match the reader can highlight and one the presenter
//! cannot would be two answers to one question.
//!
//! PDFium's own search is used rather than pulling the page's text across and
//! matching in Rust: it is the same code that produced the character indices
//! the rectangles are addressed by, so a match and its geometry cannot
//! disagree about where on the page the text is.

use pdfium_render::prelude::{PdfiumLibraryBindings, FPDF_PAGE, FPDF_TEXTPAGE};

use pulpit_core::page::{PageGeometry, PageIndex, PageQuad, PageRect, PageRotation};
use pulpit_core::search::{Hit, HitSource, PreparedQuery, TextMatch};

/// PDFium's own search flags: `FPDF_MATCHCASE` and `FPDF_MATCHWHOLEWORD`.
const MATCH_CASE: std::os::raw::c_ulong = 0x0000_0001;
const MATCH_WHOLE_WORD: std::os::raw::c_ulong = 0x0000_0002;

/// How much of the surrounding line a hit carries, in characters either side.
const CONTEXT_CHARS: i32 = pulpit_core::search::CONTEXT_CHARS as i32;

/// One page's canonical geometry, read from its crop box and rotation (A4).
///
/// A page without an explicit crop box crops to its media box, and PDFium
/// reports failure rather than substituting one; a page with neither falls
/// back to the rendered size, which PDFium always has, so the page is still
/// usable.
pub(crate) fn geometry_of(bindings: &dyn PdfiumLibraryBindings, handle: FPDF_PAGE) -> PageGeometry {
    let mut left = 0.0f32;
    let mut bottom = 0.0f32;
    let mut right = 0.0f32;
    let mut top = 0.0f32;
    let has_crop = unsafe {
        bindings.FPDFPage_GetCropBox(handle, &mut left, &mut bottom, &mut right, &mut top)
    } != 0;
    if !has_crop {
        let ok = unsafe {
            bindings.FPDFPage_GetMediaBox(handle, &mut left, &mut bottom, &mut right, &mut top)
        } != 0;
        if !ok {
            left = 0.0;
            bottom = 0.0;
            right = unsafe { bindings.FPDF_GetPageWidthF(handle) };
            top = unsafe { bindings.FPDF_GetPageHeightF(handle) };
        }
    }
    // PDFium reports rotation in *quarter turns* — 0, 1, 2, 3 — not in
    // degrees. Passing that straight to `from_degrees` reads every rotated
    // page as unrotated, which puts every mark on one in the wrong place.
    let quarters = unsafe { bindings.FPDFPage_GetRotation(handle) };
    let rotation = PageRotation::from_degrees(quarters.rem_euclid(4) * 90);
    PageGeometry::new(
        left.min(right),
        bottom.min(top),
        (right - left).abs(),
        (top - bottom).abs(),
        rotation,
        1.0,
    )
}

/// The quadrilaterals covering a run of characters, in canonical page space.
///
/// `/QuadPoints` is emitted per run rather than per glyph (§7.2), and PDFium's
/// rect list is exactly that: one rectangle per contiguous run. Shared by
/// selection and search, which want the same geometry for the same reason.
pub(crate) fn quads_of(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    geometry: &PageGeometry,
    start: i32,
    length: i32,
    most: usize,
) -> Vec<PageQuad> {
    let rects = unsafe { bindings.FPDFText_CountRects(text_page, start, length) };
    let mut quads = Vec::new();
    for index in 0..rects.max(0).min(most as i32) {
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
    quads
}

/// A run of the page's text, bounded before it is allocated.
pub(crate) fn text_of(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    start: i32,
    length: i32,
    most: usize,
) -> String {
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; (length as usize + 1).min(most)];
    let written = unsafe {
        bindings.FPDFText_GetText(
            text_page,
            start,
            (buffer.len() - 1) as i32,
            buffer.as_mut_ptr() as *mut _,
        )
    };
    if written > 0 {
        String::from_utf16_lossy(&buffer[..(written as usize - 1).min(buffer.len())])
    } else {
        String::new()
    }
}

/// Every occurrence of `query` on one page, with the geometry to draw it.
pub(crate) fn find_on_page(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    geometry: &PageGeometry,
    page: PageIndex,
    query: &PreparedQuery<'_>,
    most_hits: usize,
    most_quads: usize,
) -> Vec<Hit> {
    let options = query.query();
    let needle = options.text().trim();
    if needle.is_empty() {
        return Vec::new();
    }
    if options.regex {
        return find_regex_on_page(
            bindings, text_page, geometry, page, query, most_hits, most_quads,
        );
    }
    let mut flags = 0;
    if options.case_sensitive {
        flags |= MATCH_CASE;
    }
    if options.whole_word {
        flags |= MATCH_WHOLE_WORD;
    }

    let search = unsafe { bindings.FPDFText_FindStart_str(text_page, needle, flags, 0) };
    if search.is_null() {
        return Vec::new();
    }

    let count = unsafe { bindings.FPDFText_CountChars(text_page) }.max(0);
    let mut hits = Vec::new();
    while unsafe { bindings.FPDFText_FindNext(search) } != 0 {
        let start = unsafe { bindings.FPDFText_GetSchResultIndex(search) };
        let length = unsafe { bindings.FPDFText_GetSchCount(search) };
        if start < 0 || length <= 0 {
            continue;
        }
        let quads = quads_of(bindings, text_page, geometry, start, length, most_quads);
        // The match plus a window either side, taken from the page rather
        // than reconstructed, so the results list shows the document's own
        // words — ligatures, dashes and all.
        let context_start = (start - CONTEXT_CHARS).max(0);
        let context_end = (start + length + CONTEXT_CHARS).min(count);
        let context = text_of(
            bindings,
            text_page,
            context_start,
            context_end - context_start,
            (CONTEXT_CHARS as usize) * 4 + length as usize,
        );
        let found = TextMatch {
            offset: (start - context_start) as usize,
            len: length as usize,
        };
        hits.push(Hit::from_text(
            page,
            HitSource::PageText,
            hits.len(),
            &context,
            found,
            quads,
        ));
        if hits.len() >= most_hits {
            break;
        }
    }
    unsafe { bindings.FPDFText_FindClose(search) };
    hits
}

/// Regex needs the page's text rather than PDFium's literal finder. Text stays
/// inside the worker; only bounded hits cross IPC, exactly as literal search.
fn find_regex_on_page(
    bindings: &dyn PdfiumLibraryBindings,
    text_page: FPDF_TEXTPAGE,
    geometry: &PageGeometry,
    page: PageIndex,
    query: &PreparedQuery<'_>,
    most_hits: usize,
    most_quads: usize,
) -> Vec<Hit> {
    let count = unsafe { bindings.FPDFText_CountChars(text_page) }.max(0);
    let text = text_of(
        bindings,
        text_page,
        0,
        count,
        count.saturating_add(1) as usize,
    );
    let utf16_offsets: Vec<usize> = std::iter::once(0)
        .chain(text.chars().scan(0, |offset, character| {
            *offset += character.len_utf16();
            Some(*offset)
        }))
        .collect();
    query
        .matches_in(&text)
        .into_iter()
        .take(most_hits)
        .enumerate()
        .map(|(ordinal, found)| {
            // PDFium addresses UTF-16 code units. `TextMatch` deliberately
            // addresses Rust characters so snippets remain Unicode-correct.
            let start = utf16_offsets[found.offset];
            let end = utf16_offsets[found.offset + found.len];
            let quads = quads_of(
                bindings,
                text_page,
                geometry,
                start as i32,
                end.saturating_sub(start) as i32,
                most_quads,
            );
            Hit::from_text(page, HitSource::PageText, ordinal, &text, found, quads)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn rust_character_offsets_are_mapped_back_to_pdfium_utf16_offsets() {
        let offsets: Vec<_> = std::iter::once(0)
            .chain("a😀b".chars().scan(0, |offset, character| {
                *offset += character.len_utf16();
                Some(*offset)
            }))
            .collect();
        assert_eq!(offsets, [0, 1, 3, 4]);
    }
}
