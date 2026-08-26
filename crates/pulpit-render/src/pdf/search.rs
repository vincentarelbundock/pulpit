//! Finding a string in a page's text layer, once.
//!
//! Both PDFium-backed paths search: the document engine, for the reader, and
//! the render backend, for the presenter. They are separate processes holding
//! separate handles to the same file, so what they must *not* be is separate
//! implementations — a match the reader can highlight and one the presenter
//! cannot would be two answers to one question.
//!
//! The page's text is pulled out of PDFium once and matched by
//! [`pulpit_core::search`], the same matcher that runs over speaker notes and
//! bookmark titles. That is one implementation for a document rather than
//! three, and the geometry still cannot disagree with the match: every hit's
//! character offsets are mapped back through the text PDFium itself produced,
//! so the rectangles are addressed by the indices they were built from.
//!
//! Extracting the text is also the expensive half — `FPDF_LoadPage` parses a
//! content stream and `FPDFText_LoadPage` lays out every glyph on it — so the
//! result is worth keeping. [`PageText`] is what a backend caches: after the
//! first query, a page with no match for the second costs no PDFium call at
//! all, and a page that does match pays only for its rectangles.

use pdfium_render::prelude::{PdfiumLibraryBindings, FPDF_PAGE, FPDF_TEXTPAGE};

use pulpit_core::page::{PageGeometry, PageIndex, PageQuad, PageRect, PageRotation};
use pulpit_core::search::{Hit, HitSource, IndexedText, PreparedQuery, TextMatch};

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

/// One page's text layer, pulled out of PDFium once.
///
/// Cheap to hold — a slide's worth of text is a page of prose at most — and
/// it is what makes the second query over a document nearly free: matching
/// happens against this, and PDFium is only asked again for the rectangles of
/// the pages that actually matched.
#[derive(Debug, Clone, Default)]
pub(crate) struct PageText {
    text: String,
    /// Character offset to UTF-16 offset, one entry per character plus a
    /// final total. PDFium addresses UTF-16 code units; [`TextMatch`]
    /// deliberately addresses Rust characters so snippets stay
    /// Unicode-correct, and this is the map between them.
    utf16_offsets: Vec<usize>,
}

impl PageText {
    /// Read a whole text page. Bounded by what PDFium says it holds.
    pub(crate) fn extract(bindings: &dyn PdfiumLibraryBindings, text_page: FPDF_TEXTPAGE) -> Self {
        let count = unsafe { bindings.FPDFText_CountChars(text_page) }.max(0);
        let text = text_of(
            bindings,
            text_page,
            0,
            count,
            count.saturating_add(1) as usize,
        );
        Self::from_text(text)
    }

    fn from_text(text: String) -> Self {
        let utf16_offsets = std::iter::once(0)
            .chain(text.chars().scan(0, |offset, character| {
                *offset += character.len_utf16();
                Some(*offset)
            }))
            .collect();
        PageText {
            text,
            utf16_offsets,
        }
    }

    /// Every match on this page, without asking PDFium anything.
    ///
    /// The overwhelmingly common answer is "none", and that answer now costs
    /// a string scan rather than a page load.
    pub(crate) fn matches(&self, query: &PreparedQuery<'_>, most: usize) -> Vec<TextMatch> {
        let mut found = query.matches_in(&self.text);
        found.truncate(most);
        found
    }

    /// The UTF-16 range PDFium addresses a character match by.
    fn utf16_range(&self, found: TextMatch) -> Option<(i32, i32)> {
        let start = *self.utf16_offsets.get(found.offset)?;
        let end = *self.utf16_offsets.get(found.offset + found.len)?;
        Some((start as i32, end.saturating_sub(start) as i32))
    }
}

/// How much memory one cached page of text is holding.
///
/// A trait rather than a method because the cache below holds whatever a
/// backend extracted — PDFium's characters, or DjVu's words and their boxes —
/// and the budget is in bytes either way.
pub(crate) trait Weigh {
    fn weight(&self) -> usize;
}

impl Weigh for PageText {
    fn weight(&self) -> usize {
        self.text.len() + self.utf16_offsets.len() * std::mem::size_of::<usize>()
    }
}

/// Extracted page text, bounded by total size rather than by page count.
///
/// A deck of picture slides holds almost nothing; a book of dense pages fills
/// the budget and then keeps what it has. Dropping the *whole* cache when it
/// is full, rather than evicting one page, keeps this a few lines instead of
/// an LRU: the budget is large enough that a document either fits or is one no
/// cache was going to help twice.
///
/// Keyed by whatever identifies a page to its holder — a page number for an
/// engine that has one document, a document and page for a backend that has
/// several.
#[derive(Debug)]
pub(crate) struct PageTextCache<K, V = PageText> {
    pages: std::collections::HashMap<K, std::sync::Arc<V>>,
    weight: usize,
    budget: usize,
}

impl<K: std::hash::Hash + Eq, V> Default for PageTextCache<K, V> {
    fn default() -> Self {
        PageTextCache {
            pages: std::collections::HashMap::new(),
            weight: 0,
            // Enough for a very long book of prose, small beside one frame.
            budget: 32 * 1024 * 1024,
        }
    }
}

impl<K: std::hash::Hash + Eq, V: Weigh> PageTextCache<K, V> {
    pub(crate) fn get(&self, key: &K) -> Option<std::sync::Arc<V>> {
        self.pages.get(key).cloned()
    }

    pub(crate) fn insert(&mut self, key: K, text: V) -> std::sync::Arc<V> {
        let weight = text.weight();
        if self.weight.saturating_add(weight) > self.budget {
            self.clear();
        }
        let text = std::sync::Arc::new(text);
        if self.pages.insert(key, text.clone()).is_none() {
            self.weight = self.weight.saturating_add(weight);
        }
        text
    }

    pub(crate) fn clear(&mut self) {
        self.pages.clear();
        self.weight = 0;
    }

    /// Forget every page whose key the predicate rejects — one document
    /// closing, and not the others open beside it.
    pub(crate) fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.pages.retain(|key, text| {
            let kept = keep(key);
            if !kept {
                self.weight = self.weight.saturating_sub(text.weight());
            }
            kept
        });
    }
}

/// Turn matches into hits, asking `quads` for the geometry of each one.
///
/// The geometry is a closure so that a caller holding a cached page of text
/// decides *when* to go and get it: a page with no matches never needs to be
/// opened at all. It is also what lets a backend with a text layer of its own
/// shape — DjVu, whose zones are boxes rather than character runs — produce
/// the same hits from the same matcher (§59.2).
pub(crate) fn hits_from_matches(
    page: PageIndex,
    text: &str,
    found: &[TextMatch],
    mut quads: impl FnMut(TextMatch) -> Vec<PageQuad>,
) -> Vec<Hit> {
    if found.is_empty() {
        return Vec::new();
    }
    // The context windows are cut from the page's own characters, so the
    // results list shows the document's words — ligatures, dashes and all.
    let indexed = IndexedText::new(text);
    found
        .iter()
        .enumerate()
        .map(|(ordinal, matched)| {
            let geometry = quads(*matched);
            Hit::from_indexed_text(
                page,
                HitSource::PageText,
                ordinal,
                &indexed,
                *matched,
                geometry,
            )
        })
        .collect()
}

/// The same, for a PDFium text page, which addresses a match by the UTF-16
/// range it occupies rather than by characters.
pub(crate) fn hits_from_pdfium_matches(
    page: PageIndex,
    text: &PageText,
    found: &[TextMatch],
    mut quads: impl FnMut(i32, i32) -> Vec<PageQuad>,
) -> Vec<Hit> {
    hits_from_matches(page, &text.text, found, |matched| {
        match text.utf16_range(matched) {
            Some((start, length)) => quads(start, length),
            None => Vec::new(),
        }
    })
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
