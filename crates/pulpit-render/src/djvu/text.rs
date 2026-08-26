//! The hidden text layer of a DjVu page (`SPEC-reader-formats.md` §59.2).
//!
//! djvulibre hands the text back as the s-expression `djvused print-txt`
//! prints — nested zones, each one a symbol, four coordinates, and either a
//! string or more zones:
//!
//! ```text
//! (page 0 0 120 80
//!   (line 10 50 110 70
//!     (word 10 50 55 70 "hello")
//!     (word 60 50 110 70 "world")))
//! ```
//!
//! Walking that produces the two things a search needs: the page's text as
//! one string, and the box each word occupies. Matching then happens in
//! [`pulpit_core::search`] — the same matcher the PDF path runs, and the same
//! one that runs over speaker notes — so a hit found in the presenter is the
//! hit found in the reader (§59.2), and DjVu contributes geometry rather than
//! a second idea of what "matches" means.
//!
//! **Two coordinate traps, both measured on djvulibre 3.5.30.**
//!
//! The first is §56.6's, in mirror image. `ddjvu_document_get_pageinfo`
//! reports a rotated page's *turned* dimensions; `ddjvu_document_get_pagetext`
//! reports its text in the *stored*, unturned image space. On a page stored
//! 120×80 and rotated a quarter turn, `pageinfo` says 80×120 and the text
//! s-expression still says `(page 0 0 120 80)`. So the rotation the renderer
//! applies for free has to be applied to these coordinates by hand, or every
//! highlight on a rotated scan lands somewhere else on the page. §56.6 said
//! each Class B library needs this checked against a rotated fixture rather
//! than against its documentation; this is that check, on the other call.
//!
//! The second is the direction. djvulibre's rotation is **counter-clockwise**
//! and its text origin is the **bottom left**, while canonical page space has
//! its origin at the top left with y growing downward. [`Placement`] is the
//! one place both are undone.

use std::ffi::CStr;

use pulpit_core::page::{PageQuad, PageRect};
use pulpit_core::search::TextMatch;

use crate::djvu::sys::{self, Api, MiniExp};

/// The finest granularity asked of djvulibre.
///
/// `"word"` rather than `"char"`: a hit is highlighted as a run of words, and
/// per-character zones would multiply the size of the page's text layer by
/// the length of its words for geometry nothing draws.
pub(crate) const MAX_DETAIL: &[u8] = b"word\0";

/// The most text one page may contribute.
///
/// A page's text layer is document-controlled input, and this one is walked
/// into memory whole. A megabyte is far more than a dense scanned page of
/// prose and far less than a file that means harm.
const MAX_PAGE_TEXT_BYTES: usize = 1024 * 1024;

/// The most zones one page may contribute geometry for, for the same reason.
const MAX_WORDS: usize = 32_768;

/// How deep the zone tree may nest.
///
/// The format's own nesting is page, column, region, paragraph, line, word —
/// six. Anything past sixteen is a file built to make this recurse.
const MAX_DEPTH: usize = 16;

/// One zone with text in it, and where it sits on the stored page image.
///
/// Coordinates are djvulibre's: pixels of the unrotated image, origin at the
/// bottom left. [`Placement`] turns them into canonical page space.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Word {
    /// Character offset of this word's text within the page's text, and its
    /// length in characters. Characters rather than bytes because
    /// [`TextMatch`] addresses characters, so snippets stay Unicode-correct.
    start: usize,
    len: usize,
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
}

/// One page's text layer, pulled out of djvulibre once.
///
/// Kept for the same reason the PDF path keeps its own: the extraction is the
/// expensive half, and after the first query a page with no match for the
/// second costs nothing at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DjvuPageText {
    text: String,
    words: Vec<Word>,
    /// How many characters `text` holds.
    ///
    /// Counted as it is built rather than measured per word: a word is
    /// appended once and its offset is wanted once, and re-walking the page
    /// to find the end of it would make extraction quadratic in a document
    /// that supplied the page.
    chars: usize,
}

/// How the stored image of one page maps onto canonical page space.
///
/// Built from the same `ddjvu_pageinfo_t` the page's size comes from, so the
/// rectangles a hit is drawn with cannot disagree with the page they are
/// drawn on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    /// Quarter turns counter-clockwise, 0–3, as `ddjvu_page_rotation_t`.
    rotation: i32,
    /// The stored image's size in pixels, *before* the rotation is applied —
    /// which is the space the text s-expression speaks in.
    stored_width: f32,
    stored_height: f32,
    /// Points per pixel, from the page's resolution.
    scale: f32,
}

impl Placement {
    /// From a page's info, whose `width` and `height` are already turned.
    pub(crate) fn new(width: i32, height: i32, rotation: i32, scale: f32) -> Placement {
        let rotation = rotation.rem_euclid(4);
        // `width` and `height` arrive turned, and the text arrives unturned,
        // so a quarter or three-quarter turn swaps them back here.
        let (stored_width, stored_height) = if rotation % 2 == 1 {
            (height as f32, width as f32)
        } else {
            (width as f32, height as f32)
        };
        Placement {
            rotation,
            stored_width,
            stored_height,
            scale,
        }
    }

    /// One point of the stored image in canonical page space.
    ///
    /// Two changes at once, and neither can be skipped: the counter-clockwise
    /// rotation djvulibre applies when it renders but not when it reports
    /// text, and the flip from a bottom-left origin to a top-left one.
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        let (width, height) = (self.stored_width, self.stored_height);
        // Counter-clockwise, so the stored right edge becomes the top one.
        let (turned_x, turned_y, turned_height) = match self.rotation {
            1 => (height - y, x, width),
            2 => (width - x, height - y, height),
            3 => (y, width - x, width),
            _ => (x, y, height),
        };
        (
            turned_x * self.scale,
            (turned_height - turned_y) * self.scale,
        )
    }

    /// One word's zone as a canonical-page-space rectangle.
    fn rect(&self, word: &Word) -> PageRect {
        let (x0, y0) = self.point(word.left, word.top);
        let (x1, y1) = self.point(word.right, word.bottom);
        PageRect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }
}

impl DjvuPageText {
    /// Walk the s-expression djvulibre returned for one page.
    ///
    /// # Safety
    ///
    /// `root` must be an s-expression from `api`'s
    /// `ddjvu_document_get_pagetext`, on a document that has not been
    /// released and whose expression has not been released either.
    pub(crate) unsafe fn from_expression(api: &Api, root: MiniExp) -> DjvuPageText {
        let mut page = DjvuPageText::default();
        let mut separator = None;
        page.walk(api, root, 0, &mut separator);
        page.words.shrink_to_fit();
        page
    }

    /// One zone: `(symbol x0 y0 x1 y1 . rest)`, where `rest` is this zone's
    /// text or the zones inside it.
    ///
    /// A malformed zone is skipped rather than refused. The text layer is a
    /// convenience on top of a page that renders perfectly well without it,
    /// and a book whose fifth line is unreadable is still a book to search.
    unsafe fn walk(
        &mut self,
        api: &Api,
        zone: MiniExp,
        depth: usize,
        separator: &mut Option<char>,
    ) {
        if depth > MAX_DEPTH || !sys::is_cons(zone) || self.text.len() >= MAX_PAGE_TEXT_BYTES {
            return;
        }
        let name = zone_name(api, sys::car(zone));
        let mut rest = sys::cdr(zone);
        let mut box_edges = [0f32; 4];
        for edge in &mut box_edges {
            let head = sys::car(rest);
            if !sys::is_number(head) {
                return;
            }
            *edge = sys::to_int(head) as f32;
            rest = sys::cdr(rest);
        }

        let mut wrote = false;
        while sys::is_cons(rest) {
            let item = sys::car(rest);
            if (api.miniexp_stringp)(item) != 0 {
                let text = borrowed((api.miniexp_to_str)(item));
                if !text.is_empty() {
                    self.push(text, box_edges, separator);
                    wrote = true;
                }
            } else if sys::is_cons(item) {
                self.walk(api, item, depth + 1, separator);
                wrote = true;
            }
            rest = sys::cdr(rest);
        }

        // A line ends a run of words, and so does everything a line sits
        // inside. Without this the last word of one line and the first of the
        // next read as one word, and a search for that pair would match text
        // nobody wrote.
        if wrote && name.is_some_and(|name| name != b"word" && name != b"char") {
            *separator = Some('\n');
        }
    }

    /// Append one zone's text, and remember where it sits.
    fn push(&mut self, text: &str, edges: [f32; 4], separator: &mut Option<char>) {
        if !self.text.is_empty() {
            self.text.push(separator.unwrap_or(' '));
            self.chars += 1;
        }
        let start = self.chars;
        let remaining = MAX_PAGE_TEXT_BYTES.saturating_sub(self.text.len());
        let text = &text[..floor_char_boundary(text, remaining)];
        self.text.push_str(text);
        let length = text.chars().count();
        self.chars += length;
        if self.words.len() < MAX_WORDS {
            self.words.push(Word {
                start,
                len: length,
                left: edges[0].min(edges[2]),
                bottom: edges[1].min(edges[3]),
                right: edges[0].max(edges[2]),
                top: edges[1].max(edges[3]),
            });
        }
        *separator = Some(' ');
    }

    /// This page's text, for the matcher.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Every match on this page, without asking djvulibre anything.
    ///
    /// The overwhelmingly common answer is "none", and on a book that answer
    /// is a string scan rather than a fetch and a walk.
    pub(crate) fn matches(
        &self,
        query: &pulpit_core::search::PreparedQuery<'_>,
        most: usize,
    ) -> Vec<TextMatch> {
        let mut found = query.matches_in(&self.text);
        found.truncate(most);
        found
    }

    /// The quadrilaterals covering one match, in canonical page space.
    ///
    /// One per word rather than one per line: djvulibre gives boxes per zone,
    /// and a match that starts mid-line would otherwise be highlighted from
    /// the line's left edge. A match with no word behind it — text past
    /// [`MAX_WORDS`] — has no geometry rather than wrong geometry.
    pub(crate) fn quads(&self, matched: TextMatch, at: &Placement, most: usize) -> Vec<PageQuad> {
        let end = matched.offset.saturating_add(matched.len);
        self.words
            .iter()
            .filter(|word| word.start < end && word.start + word.len > matched.offset)
            .take(most)
            .map(|word| PageQuad::from_rect(at.rect(word)))
            .filter(|quad| !quad.is_degenerate())
            .collect()
    }
}

/// How much memory this page is holding, for the bounded cache the PDF path
/// also uses: one budget, one eviction rule, whichever backend filled it.
impl crate::pdf::search::Weigh for DjvuPageText {
    fn weight(&self) -> usize {
        self.text.len() + self.words.len() * std::mem::size_of::<Word>()
    }
}

/// The symbol naming a zone, as bytes, or `None` if it is not a symbol.
///
/// # Safety
///
/// `expression` must belong to a live document of `api`'s.
unsafe fn zone_name<'a>(api: &Api, expression: MiniExp) -> Option<&'a [u8]> {
    if !sys::is_symbol(expression) {
        return None;
    }
    let name = (api.miniexp_to_name)(expression);
    (!name.is_null()).then(|| CStr::from_ptr(name).to_bytes())
}

/// A string from djvulibre's heap, as UTF-8.
///
/// # Safety
///
/// `text` must be NUL-terminated and valid for the duration of the call.
unsafe fn borrowed<'a>(text: *const std::ffi::c_char) -> &'a str {
    if text.is_null() {
        return "";
    }
    // djvulibre produces UTF-8 here. A file that does not is a file whose
    // text layer is partly unreadable, which is not a reason to refuse the
    // rest of the page.
    CStr::from_ptr(text).to_str().unwrap_or("")
}

/// The largest index at or below `at` that is a character boundary.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    let mut at = at;
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured case (§56.6, and the module's own header): a page stored
    /// 120×80 and turned a quarter turn reports 80×120 from `pageinfo` while
    /// its text still speaks in 120×80. A word in the stored image's
    /// top-left corner belongs at the *bottom* left of the rotated page,
    /// because djvulibre turns counter-clockwise.
    #[test]
    fn a_quarter_turn_moves_a_word_the_way_the_renderer_does() {
        let at = Placement::new(80, 120, 1, 1.0);
        let word = Word {
            start: 0,
            len: 1,
            left: 10.0,
            bottom: 50.0,
            right: 55.0,
            top: 70.0,
        };
        let rect = at.rect(&word);
        // Stored y ∈ [50, 70] out of 80 is near the top; a counter-clockwise
        // turn puts it near the left, and x ∈ [10, 55] out of 120 puts it
        // most of the way down a page that is now 120 tall.
        assert_eq!(rect, PageRect::new(10.0, 65.0, 30.0, 110.0));
    }

    /// The unrotated case is only the origin flip, and the page it lands on
    /// is the one `page_size` reports: an upright 120×80 page.
    #[test]
    fn an_upright_page_is_only_flipped_end_over_end() {
        let at = Placement::new(120, 80, 0, 1.0);
        let word = Word {
            start: 0,
            len: 1,
            left: 10.0,
            bottom: 50.0,
            right: 55.0,
            top: 70.0,
        };
        assert_eq!(at.rect(&word), PageRect::new(10.0, 10.0, 55.0, 30.0));
    }

    /// Points, not pixels: a 300dpi scan is a page of a size somebody could
    /// print, and its words sit at the same fraction of it.
    #[test]
    fn the_resolution_scales_a_word_with_its_page() {
        let at = Placement::new(120, 80, 0, 0.24);
        let word = Word {
            start: 0,
            len: 1,
            left: 0.0,
            bottom: 0.0,
            right: 120.0,
            top: 80.0,
        };
        let rect = at.rect(&word);
        assert!(
            (rect.right - 28.8).abs() < 0.01 && (rect.bottom - 19.2).abs() < 0.01,
            "{rect:?}"
        );
        assert_eq!((rect.left, rect.top), (0.0, 0.0));
    }

    /// A match is highlighted word by word, so a query spanning two of them
    /// gets two boxes and one that touches neither gets none.
    #[test]
    fn a_match_takes_the_boxes_of_the_words_it_covers() {
        let page = DjvuPageText {
            text: "hello world".into(),
            chars: 11,
            words: vec![
                Word {
                    start: 0,
                    len: 5,
                    left: 0.0,
                    bottom: 0.0,
                    right: 50.0,
                    top: 20.0,
                },
                Word {
                    start: 6,
                    len: 5,
                    left: 60.0,
                    bottom: 0.0,
                    right: 110.0,
                    top: 20.0,
                },
            ],
        };
        let at = Placement::new(120, 20, 0, 1.0);
        let across = page.quads(TextMatch { offset: 3, len: 5 }, &at, 8);
        assert_eq!(across.len(), 2);
        let inside = page.quads(TextMatch { offset: 6, len: 5 }, &at, 8);
        assert_eq!(inside.len(), 1);
        assert_eq!(inside[0].bounds(), PageRect::new(60.0, 0.0, 110.0, 20.0));
    }
}
