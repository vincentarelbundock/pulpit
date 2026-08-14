//! Speaker notes carried inside the PDF, in the pdfpc interchange format.
//!
//! This is the one notes channel both authoring toolchains can write without
//! a post-processing step and every console already reads. beamer emits it
//! with `\usepackage{pdfpc}` and `\pdfpcnote`; a Typst deck emits it with
//! `pdf.attach`, which takes computed bytes, so the notes the document already
//! carries can be serialised into the file that contains them. The payload
//! lands in the PDF's `/EmbeddedFiles` name tree under a `*.pdfpc` name — a
//! sidecar in content, but not a companion file to lose.
//!
//! Two dialects are read. Current pdfpc, BeamerPresenter and `polylux2pdfpc`
//! write JSON; older decks and the older LaTeX package write a sectioned text
//! file. Both are accepted, because a presenter's four-year-old talk is
//! exactly the deck they reopen under time pressure.
//!
//! Notes here are *text*, not a cropped region of a page, so they carry no
//! layout. A note whose layout matters belongs in a split-page deck, which
//! [`crate::notes`] maps instead.

use crate::notes::NotesMapping;
use serde::Deserialize;
use std::collections::BTreeMap;

/// A pdfpc payload larger than this is not a set of speaker notes.
///
/// Notes are prose: a densely annotated hundred-slide deck runs to tens of
/// kilobytes. The ceiling exists so a malformed or hostile attachment cannot
/// be parsed into memory, and it is generous by two orders of magnitude.
pub const MAX_PAYLOAD_BYTES: usize = 4 << 20;

/// Speaker notes keyed by *physical* PDF page, zero-based.
///
/// Physical rather than logical because that is what the format states and
/// what the file can be checked against. The mapping from a logical slide to
/// its page is pulpit's, may change while the document is open, and is
/// applied at lookup by [`TextNotes::for_slide`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextNotes {
    by_page: BTreeMap<usize, String>,
    /// Whether the producer said the bodies are Markdown. Recorded rather than
    /// acted on: the pane renders text either way today, and a renderer that
    /// arrives later needs to know which bodies it may format.
    markdown: bool,
}

impl TextNotes {
    /// Read either pdfpc dialect. `None` when the payload is not pdfpc at all,
    /// or carries no notes — an empty set of notes is not worth announcing.
    pub fn parse(payload: &str) -> Option<TextNotes> {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return None;
        }
        let notes = if payload.trim_start().starts_with('{') {
            parse_json(payload)?
        } else {
            parse_sectioned(payload)?
        };
        (!notes.by_page.is_empty()).then_some(notes)
    }

    pub fn is_empty(&self) -> bool {
        self.by_page.is_empty()
    }

    /// How many pages carry a note. The count pulpit reports on open.
    pub fn len(&self) -> usize {
        self.by_page.len()
    }

    pub fn markdown(&self) -> bool {
        self.markdown
    }

    /// The note on a physical page, zero-based.
    pub fn for_page(&self, page: usize) -> Option<&str> {
        self.by_page.get(&page).map(String::as_str)
    }

    /// The note for a logical slide under the mapping in force.
    ///
    /// The slide's *audience* page is the one asked about: a note belongs to
    /// the slide the room is looking at, never to the page the notes half was
    /// cropped from.
    pub fn for_slide(
        &self,
        slide: usize,
        mapping: &NotesMapping,
        pdf_pages: usize,
    ) -> Option<&str> {
        let source = mapping.audience_source(slide, pdf_pages)?;
        self.for_page(source.pdf_page)
    }

    /// Every logical slide's note under one mapping, indexed by slide.
    ///
    /// Resolved once when the document opens or the mapping changes, so that
    /// drawing a frame is a lookup rather than a walk.
    pub fn by_slide(&self, mapping: &NotesMapping, pdf_pages: usize) -> Vec<Option<String>> {
        (0..mapping.slide_count(pdf_pages))
            .map(|slide| {
                self.for_slide(slide, mapping, pdf_pages)
                    .map(str::to_string)
            })
            .collect()
    }
}

/// The JSON dialect, as written by pdfpc, BeamerPresenter and `polylux2pdfpc`.
#[derive(Deserialize)]
struct JsonPayload {
    #[serde(rename = "pdfpcFormat")]
    format: Option<u32>,
    #[serde(default)]
    pages: Vec<JsonPage>,
}

#[derive(Deserialize)]
struct JsonPage {
    /// One-based physical page. The format's own numbering.
    idx: Option<i64>,
    #[serde(default)]
    note: Option<String>,
}

fn parse_json(payload: &str) -> Option<TextNotes> {
    let parsed: JsonPayload = serde_json::from_str(payload).ok()?;
    // A future format may mean fields this parser would misread. Refusing is
    // the honest answer: a wrong note under the wrong slide is worse than no
    // notes, because the presenter trusts it.
    if parsed.format.is_some_and(|format| format > 2) {
        return None;
    }
    let mut by_page = BTreeMap::new();
    for page in parsed.pages {
        let Some(idx) = page.idx else { continue };
        // One-based in the file, zero-based here. A zero or negative index is
        // not a page, so it is dropped rather than wrapped onto the last one.
        let Ok(idx) = usize::try_from(idx) else {
            continue;
        };
        let Some(zero_based) = idx.checked_sub(1) else {
            continue;
        };
        let Some(note) = page.note else { continue };
        if note.trim().is_empty() {
            continue;
        }
        by_page.insert(zero_based, note);
    }
    Some(TextNotes {
        by_page,
        markdown: true,
    })
}

/// The sectioned dialect: a `[notes]` section of `### N` delimited blocks.
fn parse_sectioned(payload: &str) -> Option<TextNotes> {
    let mut by_page = BTreeMap::new();
    let mut markdown = false;
    let mut in_notes = false;
    let mut page: Option<usize> = None;
    let mut body = String::new();

    // Closing over `by_page` and `body` would borrow both for the whole loop,
    // so the flush is written out at each of its three sites instead.
    macro_rules! flush {
        () => {
            if let Some(page) = page.take() {
                if !body.trim().is_empty() {
                    by_page.insert(page, body.trim_end().to_string());
                }
            }
            body.clear();
        };
    }

    for line in payload.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[').filter(|_| trimmed.contains(']')) {
            flush!();
            // `[notes]` may carry a format after the bracket:
            // `[notes] type=markdown`. The name decides the section; the
            // remainder is read for the format and otherwise ignored.
            let (name, tail) = rest.split_once(']').unwrap_or((rest, ""));
            in_notes = name.trim() == "notes";
            if in_notes && tail.contains("markdown") {
                markdown = true;
            }
            continue;
        }
        if !in_notes {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("###") {
            flush!();
            // The marker counts user slides from one, as the JSON `idx` does.
            page = rest
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|idx| idx.checked_sub(1));
            continue;
        }
        if page.is_some() {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush!();

    Some(TextNotes { by_page, markdown })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::{PairedRule, Region};

    #[test]
    fn the_json_dialect_is_read_and_renumbered_from_zero() {
        let notes = TextNotes::parse(
            r#"{"pdfpcFormat":2,"pages":[
                {"idx":1,"note":"Open by naming the question."},
                {"idx":3,"note":"Give the point estimate first.","forcedOverlay":true}
            ]}"#,
        )
        .expect("a pdfpc payload");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes.for_page(0), Some("Open by naming the question."));
        assert_eq!(notes.for_page(2), Some("Give the point estimate first."));
        assert_eq!(notes.for_page(1), None, "page two carries no note");
        assert!(notes.markdown(), "the JSON dialect states Markdown bodies");
    }

    #[test]
    fn unknown_fields_do_not_stop_the_notes_being_read() {
        let notes = TextNotes::parse(
            r#"{"pdfpcFormat":2,"duration":20,"endSlide":4,
                "pages":[{"idx":1,"note":"Hello","label":"1","hidden":false}]}"#,
        )
        .expect("a pdfpc payload");
        assert_eq!(notes.for_page(0), Some("Hello"));
    }

    #[test]
    fn a_future_format_is_refused_rather_than_guessed_at() {
        assert_eq!(
            TextNotes::parse(r#"{"pdfpcFormat":9,"pages":[{"idx":1,"note":"Hello"}]}"#),
            None,
            "a wrong note under the wrong slide is worse than none"
        );
    }

    #[test]
    fn the_sectioned_dialect_is_read() {
        let notes = TextNotes::parse(
            "[file]\ntalk.pdf\n[duration]\n20\n[notes]\n### 1\nFirst note\nsecond line\n### 2\n\
             Second note\n",
        )
        .expect("a pdfpc payload");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes.for_page(0), Some("First note\nsecond line"));
        assert_eq!(notes.for_page(1), Some("Second note"));
        assert!(
            !notes.markdown(),
            "the older dialect is plain unless it says"
        );
    }

    #[test]
    fn the_sectioned_dialect_can_declare_markdown() {
        let notes = TextNotes::parse("[notes] type=markdown\n### 1\n*emphasis*\n")
            .expect("a pdfpc payload");
        assert!(notes.markdown());
    }

    #[test]
    fn text_outside_the_notes_section_is_not_a_note() {
        let notes = TextNotes::parse("[file]\ntalk.pdf\n[duration]\n20\n");
        assert_eq!(notes, None, "no notes is not a notes payload");
    }

    #[test]
    fn payloads_that_are_not_pdfpc_are_refused() {
        assert_eq!(TextNotes::parse("{not json at all"), None);
        assert_eq!(TextNotes::parse(""), None);
        assert_eq!(TextNotes::parse("just some prose"), None);
    }

    #[test]
    fn an_oversized_payload_is_refused_before_it_is_parsed() {
        let huge = format!(
            r#"{{"pdfpcFormat":2,"pages":[{{"idx":1,"note":"{}"}}]}}"#,
            "x".repeat(MAX_PAYLOAD_BYTES)
        );
        assert_eq!(TextNotes::parse(&huge), None);
    }

    #[test]
    fn a_page_index_of_zero_is_dropped_rather_than_wrapped() {
        assert_eq!(
            TextNotes::parse(r#"{"pdfpcFormat":2,"pages":[{"idx":0,"note":"Hello"}]}"#),
            None,
            "the format counts from one, so there is no page zero"
        );
    }

    #[test]
    fn empty_notes_are_not_notes() {
        assert_eq!(
            TextNotes::parse(r#"{"pdfpcFormat":2,"pages":[{"idx":1,"note":"   "}]}"#),
            None
        );
    }

    #[test]
    fn a_slide_reads_the_note_on_its_audience_page() {
        let notes = TextNotes::parse(
            r#"{"pdfpcFormat":2,"pages":[{"idx":1,"note":"one"},{"idx":3,"note":"three"}]}"#,
        )
        .expect("a pdfpc payload");

        // Slides and pages coincide, so the note lands on its own slide.
        let plain = NotesMapping::SlidesOnly;
        assert_eq!(notes.for_slide(0, &plain, 4), Some("one"));
        assert_eq!(notes.for_slide(2, &plain, 4), Some("three"));

        // Alternating: logical slide 1 is physical page 2, which is page index
        // 2 zero-based, so it is the note written for page three.
        let alternating = NotesMapping::PairedPages(PairedRule::Alternating { notes_first: false });
        assert_eq!(notes.for_slide(0, &alternating, 4), Some("one"));
        assert_eq!(notes.for_slide(1, &alternating, 4), Some("three"));
    }

    #[test]
    fn a_split_deck_reads_notes_from_the_audience_page() {
        let notes =
            TextNotes::parse(r#"{"pdfpcFormat":2,"pages":[{"idx":2,"note":"two"}]}"#).unwrap();
        let split = NotesMapping::SplitPage {
            slide: Region::left_half(),
            notes: Region::right_half(),
        };
        assert_eq!(
            notes.for_slide(1, &split, 3),
            Some("two"),
            "the halves share one physical page, so the note is that page's"
        );
    }

    #[test]
    fn resolving_every_slide_gives_one_entry_per_slide() {
        let notes =
            TextNotes::parse(r#"{"pdfpcFormat":2,"pages":[{"idx":2,"note":"two"}]}"#).unwrap();
        let resolved = notes.by_slide(&NotesMapping::SlidesOnly, 3);
        assert_eq!(
            resolved,
            vec![None, Some("two".to_string()), None],
            "one slot per slide, so drawing is a lookup"
        );
    }
}
