//! What the search pane says, decided without drawing anything.
//!
//! The summary line is the whole reason this file exists: "no matches", "3 of
//! 17", "17 so far" and "this document cannot be searched" are four different
//! statements about a document, and which one is true is a decision that
//! should be testable without a window.

use pulpit_core::search::{Hit, SearchProblem, SearchState};

/// The line under the search box.
pub fn summary(state: &SearchState) -> String {
    if let Some(problem) = state.problem() {
        // A problem that stopped an otherwise useful scan still reports what
        // was found: "more than 2048 matches" is only alarming if the reader
        // cannot see the ones it did find.
        return match problem {
            SearchProblem::TooManyHits if !state.hits().is_empty() => {
                format!("{} matches; narrow the search", state.hits().len())
            }
            other => other.to_string(),
        };
    }
    if state.query().is_empty() {
        return String::new();
    }
    match (state.hits().len(), state.position()) {
        (0, _) if state.scanning() => "Searching…".to_string(),
        (0, _) => "No matches".to_string(),
        (total, Some(at)) if state.scanning() => format!("{at} of {total} so far"),
        (total, Some(at)) => format!("{at} of {total}"),
        (total, None) if state.scanning() => format!("{total} so far"),
        (total, None) => format!("{total} matches"),
    }
}

/// The left-hand label of one row: the page it is on, and where it came from.
///
/// Pages are counted from one everywhere a person reads them, which
/// [`pulpit_core::page::PageIndex`]'s own `Display` already does.
pub fn row_label(hit: &Hit) -> String {
    match hit.source {
        pulpit_core::search::HitSource::PageText => format!("p. {}", hit.page),
        source => format!("p. {} · {}", hit.page, source.label()),
    }
}

/// The row's text split around the match, so the middle can be drawn
/// emphasised without the view doing character arithmetic.
pub fn row_parts(hit: &Hit) -> (String, String, String) {
    let chars: Vec<char> = hit.context.chars().collect();
    let start = hit.highlight.offset.min(chars.len());
    let end = (hit.highlight.offset + hit.highlight.len).min(chars.len());
    (
        chars[..start].iter().collect(),
        chars[start..end].iter().collect(),
        chars[end..].iter().collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::page::PageIndex;
    use pulpit_core::search::{HitChunk, HitSource, Query, TextMatch};

    fn hit(page: usize, ordinal: usize) -> Hit {
        Hit {
            page: PageIndex(page),
            source: HitSource::PageText,
            ordinal,
            quads: Vec::new(),
            context: "a needle here".into(),
            highlight: TextMatch { offset: 2, len: 6 },
        }
    }

    fn searched(pages: usize, hits: Vec<Hit>, done: bool) -> SearchState {
        let mut state = SearchState::new();
        state.open(pages);
        let generation = state.set_query(Query::new("needle", false, false));
        state.accept(
            generation,
            HitChunk {
                from_page: 0,
                to_page: if done { pages } else { 1 },
                hits,
                truncated: false,
            },
        );
        state
    }

    #[test]
    fn nothing_typed_says_nothing() {
        assert_eq!(summary(&SearchState::new()), "");
    }

    #[test]
    fn a_finished_scan_with_nothing_in_it_says_so() {
        assert_eq!(summary(&searched(1, Vec::new(), true)), "No matches");
    }

    #[test]
    fn a_scan_still_running_never_claims_there_are_none() {
        assert_eq!(summary(&searched(100, Vec::new(), false)), "Searching…");
        assert_eq!(
            summary(&searched(100, vec![hit(0, 0), hit(1, 0)], false)),
            "1 of 2 so far"
        );
    }

    #[test]
    fn a_finished_scan_counts_the_position() {
        assert_eq!(
            summary(&searched(2, vec![hit(0, 0), hit(1, 0)], true)),
            "1 of 2"
        );
    }

    #[test]
    fn a_row_is_split_around_its_match() {
        let (before, matched, after) = row_parts(&hit(0, 0));
        assert_eq!(before, "a ");
        assert_eq!(matched, "needle");
        assert_eq!(after, " here");
    }

    #[test]
    fn a_row_says_which_page_and_which_source() {
        assert_eq!(row_label(&hit(4, 0)), "p. 5");
        let mut from_notes = hit(4, 0);
        from_notes.source = HitSource::Notes;
        assert_eq!(row_label(&from_notes), "p. 5 · notes");
    }
}
