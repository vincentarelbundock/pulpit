//! Splitting a page's text into the units that get spoken.
//!
//! The sentence is the quantum of this whole feature. It is what gets
//! synthesised, what gets prefetched while the previous one plays, what a
//! pause stops after and what a resume starts from, and what a highlight
//! follows. Getting the boundaries wrong is therefore audible twice: once as
//! a pause in the wrong place, and once as a control that does not respond
//! where the reader expected it to.
//!
//! There is no perfect rule. `Fig. 4 shows` and `ends here. Next` differ only
//! by knowledge this crate does not have, so the rules below are tuned to
//! prefer *not* splitting when the evidence is weak: a sentence that runs on
//! is a slightly long breath, while a sentence split at `Dr.` is an audible
//! stumble in the middle of a name.

use std::ops::Range;

/// One speakable unit, addressed by byte range into the text it came from.
///
/// A range rather than a `String` so a caller can highlight the span it is
/// currently speaking without a second search for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    /// Byte range into the source text, always on character boundaries.
    pub range: Range<usize>,
}

impl Sentence {
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range.clone()]
    }
}

/// Abbreviations after which a period is usually not a sentence end.
///
/// Deliberately short. Each entry buys one avoided mis-split and costs one
/// missed real boundary when a sentence genuinely ends in that word, so only
/// the ones that are overwhelmingly abbreviations earn a place.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "st", "jr", "sr", "vs", "etc", "e.g", "i.e", "cf", "al",
    "fig", "figs", "eq", "eqs", "no", "vol", "pp", "ch", "sec", "approx", "est", "inc", "ltd",
    "co", "univ", "dept", "ed", "eds", "trans", "repr", "ibid", "op", "cit",
];

/// Terminators that end a sentence in Latin-script writing, plus the
/// full-width forms used in CJK text, where there is no following space to
/// key on.
fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '。' | '！' | '？' | '…')
}

/// Closing punctuation that may follow a terminator and still belong to the
/// sentence that is ending: `(as shown).` and `"stop!"` both end after the
/// bracket, not before it.
fn is_trailing(c: char) -> bool {
    matches!(
        c,
        ')' | ']' | '}' | '"' | '\'' | '»' | '”' | '’' | '」' | '』' | '）'
    )
}

/// Split `text` into sentences.
///
/// Every byte of `text` that is not inter-sentence whitespace lands in exactly
/// one sentence, so a caller can reconstruct the page from the ranges. Empty
/// and whitespace-only input yields no sentences rather than one empty one.
pub fn sentences(text: &str) -> Vec<Sentence> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = None::<usize>;
    let mut index = 0usize;

    while index < text.len() {
        let c = match text[index..].chars().next() {
            Some(c) => c,
            None => break,
        };
        let width = c.len_utf8();

        if start.is_none() && !c.is_whitespace() {
            start = Some(index);
        }

        if start.is_some() && is_terminator(c) {
            let mut end = index + width;
            // Absorb a run of terminators (`?!`, `...`) and any closing
            // punctuation, so the break lands after them.
            while end < text.len() {
                let next = match text[end..].chars().next() {
                    Some(next) => next,
                    None => break,
                };
                if is_terminator(next) || is_trailing(next) {
                    end += next.len_utf8();
                } else {
                    break;
                }
            }

            if ends_sentence(text, index, end) {
                let begin = start.take().unwrap();
                out.push(Sentence { range: begin..end });
            }
            index = end;
            continue;
        }

        // A blank line is a boundary even without punctuation: headings,
        // captions and list items frequently have no terminator at all, and
        // running them into the next paragraph is the most common way this
        // sounds broken on a real page.
        if start.is_some() && c == '\n' && is_blank_line_break(bytes, index) {
            let begin = start.take().unwrap();
            let end = trim_end(text, begin, index);
            if end > begin {
                out.push(Sentence { range: begin..end });
            }
        }

        index += width;
    }

    if let Some(begin) = start {
        let end = trim_end(text, begin, text.len());
        if end > begin {
            out.push(Sentence { range: begin..end });
        }
    }
    out
}

/// Whether the terminator run ending at `end` really ends a sentence.
fn ends_sentence(text: &str, terminator: usize, end: usize) -> bool {
    let c = text[terminator..].chars().next().unwrap_or('.');

    // `!`, `?` and the CJK forms are unambiguous; only the period is
    // overloaded, so only the period is interrogated further.
    if c != '.' {
        return true;
    }

    let before = &text[..terminator];
    let word = before
        .rsplit(|ch: char| ch.is_whitespace())
        .next()
        .unwrap_or("")
        .trim_end_matches(['(', '"']);
    let bare = word.trim_start_matches(|ch: char| !ch.is_alphanumeric());

    // A single letter before the period is an initial (`J. R. R. Tolkien`) or
    // a list label (`a. first`), neither of which ends a sentence.
    if bare.chars().count() == 1 && bare.chars().all(|ch| ch.is_alphabetic()) {
        return false;
    }
    if ABBREVIATIONS.contains(&bare.to_lowercase().as_str()) {
        return false;
    }
    // A decimal point: digits on both sides, no space.
    if bare.chars().last().is_some_and(|ch| ch.is_ascii_digit()) {
        if let Some(next) = text[end..].chars().next() {
            if next.is_ascii_digit() {
                return false;
            }
        }
    }

    // What follows has to look like a new sentence. End of text counts;
    // otherwise there must be whitespace, and then something that is not a
    // lowercase letter — `foo. bar` inside a filename or a citation stays one
    // unit, while `foo. Bar` breaks.
    let rest = &text[end..];
    let mut chars = rest.chars();
    match chars.next() {
        None => true,
        Some(next) if next.is_whitespace() => match rest.trim_start().chars().next() {
            None => true,
            Some(following) => !following.is_lowercase(),
        },
        // No space after the period at all: an ellipsis already consumed, a
        // URL, or a version number.
        Some(_) => false,
    }
}

/// Whether the newline at `index` is part of a blank-line (paragraph) break.
fn is_blank_line_break(bytes: &[u8], index: usize) -> bool {
    let mut seen_newline = false;
    let mut cursor = index + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\n' => {
                seen_newline = true;
                break;
            }
            b' ' | b'\t' | b'\r' => cursor += 1,
            _ => break,
        }
    }
    seen_newline
}

fn trim_end(text: &str, begin: usize, end: usize) -> usize {
    let slice = &text[begin..end];
    begin + slice.trim_end().len()
}

/// Split a sentence that is too long for one synthesis call.
///
/// Some engines cap their input — Kokoro's graph takes a bounded number of
/// tokens per forward pass — and a legal sentence can exceed it. Splitting is
/// then not optional, so the only question is where it hurts least: at a
/// clause boundary if there is one, at a word boundary otherwise, and never
/// mid-word.
///
/// `limit` is in bytes, which is a proxy for tokens rather than a measure of
/// them; callers should pass a limit with enough headroom that the proxy
/// being loose does not matter.
pub fn split_long(text: &str, limit: usize) -> Vec<Range<usize>> {
    if limit == 0 || text.len() <= limit {
        // Built rather than `vec![a..b]`, which reads as a range *of* vectors
        // to the lint and to about half of readers.
        return std::iter::once(0..text.len()).collect();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while text.len() - start > limit {
        let window_end = floor_boundary(text, start + limit);
        let window = &text[start..window_end];
        // Prefer a clause break, then any whitespace. `rfind` over the window
        // gives the latest such point, which keeps chunks as full as
        // possible and so keeps the number of audible seams down.
        let cut = window
            .rfind([';', ':', ',', '—', '、'])
            .map(|at| at + window[at..].chars().next().map_or(1, char::len_utf8))
            .or_else(|| window.rfind(char::is_whitespace))
            .map(|at| start + at)
            .unwrap_or(window_end);
        let cut = if cut <= start { window_end } else { cut };
        out.push(start..cut);
        start = cut + text[cut..].len() - text[cut..].trim_start().len();
    }
    if start < text.len() {
        out.push(start..text.len());
    }
    out
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spoken(text: &str) -> Vec<&str> {
        sentences(text)
            .into_iter()
            .map(|sentence| sentence.text(text))
            .collect()
    }

    #[test]
    fn plain_prose_splits_on_terminators() {
        assert_eq!(
            spoken("One thing. Two things! Three? Yes."),
            ["One thing.", "Two things!", "Three?", "Yes."]
        );
    }

    #[test]
    fn abbreviations_do_not_end_a_sentence() {
        assert_eq!(
            spoken("See Fig. 4 for the layout. It is described by Dr. Smith."),
            [
                "See Fig. 4 for the layout.",
                "It is described by Dr. Smith."
            ]
        );
    }

    #[test]
    fn initials_and_decimals_stay_together() {
        assert_eq!(
            spoken("J. R. R. Tolkien wrote it."),
            ["J. R. R. Tolkien wrote it."]
        );
        assert_eq!(
            spoken("The value is 3.14 exactly. Really."),
            ["The value is 3.14 exactly.", "Really."]
        );
    }

    #[test]
    fn a_lowercase_continuation_is_not_a_new_sentence() {
        // The shape a version number or a citation takes in extracted text.
        assert_eq!(
            spoken("pulpit v0.0.9 ships it."),
            ["pulpit v0.0.9 ships it."]
        );
    }

    #[test]
    fn closing_punctuation_belongs_to_the_sentence_it_closes() {
        assert_eq!(
            spoken("He said \"stop!\" Then he left."),
            ["He said \"stop!\"", "Then he left."]
        );
        assert_eq!(
            spoken("It works (mostly). Next."),
            ["It works (mostly).", "Next."]
        );
    }

    #[test]
    fn runs_of_terminators_are_one_boundary() {
        assert_eq!(
            spoken("Really?! Yes... Fine."),
            ["Really?!", "Yes...", "Fine."]
        );
    }

    #[test]
    fn a_blank_line_ends_a_sentence_that_has_no_terminator() {
        // Headings and captions, which is most of what a slide holds.
        assert_eq!(
            spoken("Chapter One\n\nThe reconciliation function is pure."),
            ["Chapter One", "The reconciliation function is pure."]
        );
        // A single newline is a line wrap, not a boundary.
        assert_eq!(
            spoken("a sentence broken\nacross two lines."),
            ["a sentence broken\nacross two lines."]
        );
    }

    #[test]
    fn cjk_terminators_need_no_following_space() {
        assert_eq!(
            spoken("これは日本語です。表示を確認してください。"),
            ["これは日本語です。", "表示を確認してください。"]
        );
    }

    #[test]
    fn nothing_speakable_yields_no_sentences() {
        assert!(sentences("").is_empty());
        assert!(sentences("   \n\n  \t ").is_empty());
    }

    #[test]
    fn ranges_are_on_character_boundaries_and_in_order() {
        let text = "Héllo wörld. Zweiter Satz! Ünd der dritte?";
        let found = sentences(text);
        let mut last_end = 0;
        for sentence in &found {
            assert!(text.is_char_boundary(sentence.range.start));
            assert!(text.is_char_boundary(sentence.range.end));
            assert!(sentence.range.start >= last_end);
            assert!(sentence.range.end > sentence.range.start);
            last_end = sentence.range.end;
        }
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn long_sentences_split_at_clauses_then_words_never_mid_word() {
        let text = "one two three, four five six, seven eight nine ten eleven";
        let parts = split_long(text, 20);
        assert!(parts.len() > 1);
        for part in &parts {
            assert!(part.end - part.start <= 22, "chunk within limit-ish");
            let chunk = &text[part.clone()];
            assert_eq!(chunk.trim(), chunk, "no leading or trailing space");
        }
        // Rejoining recovers the words in order.
        let joined: Vec<&str> = parts.iter().map(|p| &text[p.clone()]).collect();
        assert_eq!(
            joined.join(" ").split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_short_sentence_is_never_split() {
        assert_eq!(split_long("short", 100), vec![0..5]);
        assert_eq!(split_long("short", 0), vec![0..5]);
    }

    #[test]
    fn splitting_a_word_longer_than_the_limit_still_terminates() {
        let text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let parts = split_long(text, 10);
        assert!(parts.len() >= 3);
        assert_eq!(parts.last().unwrap().end, text.len());
    }
}
