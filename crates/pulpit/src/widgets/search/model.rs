//! What the search pane says, decided without drawing anything.
//!
//! The summary line is the whole reason this file exists: "no matches", "3 of
//! 17", "17 so far" and "this document cannot be searched" are four different
//! statements about a document, and which one is true is a decision that
//! should be testable without a window.

use pulpit_core::search::{Hit, SearchProblem, SearchState};
use std::ops::Range;

/// One result card per page, whatever number of occurrences the page carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageGroup {
    pub page: pulpit_core::page::PageIndex,
    pub hit_range: Range<usize>,
}

/// Fixed-height cards make the result stream virtualizable without measuring
/// hundreds of off-screen snippets on every view pass.
pub const RESULT_ROW_HEIGHT: f32 = 82.0;
const RESULT_OVERSCAN_ROWS: usize = 2;

/// Collapse consecutive, document-ordered hits into one card per page.
pub fn page_groups(hits: &[Hit]) -> Vec<PageGroup> {
    let mut groups: Vec<PageGroup> = Vec::new();
    for (index, hit) in hits.iter().enumerate() {
        match groups.last_mut() {
            Some(group) if group.page == hit.page => group.hit_range.end = index + 1,
            _ => groups.push(PageGroup {
                page: hit.page,
                hit_range: index..index + 1,
            }),
        }
    }
    groups
}

/// Which result cards to build for one viewport.
pub fn visible_group_range(count: usize, scroll: f32, viewport: f32) -> Range<usize> {
    if count == 0 || viewport <= 0.0 {
        return 0..0;
    }
    let first_visible = (scroll.max(0.0) / RESULT_ROW_HEIGHT).floor() as usize;
    let first = first_visible.saturating_sub(RESULT_OVERSCAN_ROWS);
    let visible = (viewport / RESULT_ROW_HEIGHT).ceil() as usize;
    let last = (first_visible + visible + RESULT_OVERSCAN_ROWS + 1).min(count);
    first.min(last)..last
}

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

/// The excerpt split around its match for one flowing rich-text row.
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
    fn a_row_says_which_page_and_which_source() {
        assert_eq!(row_label(&hit(4, 0)), "p. 5");
        let mut from_notes = hit(4, 0);
        from_notes.source = HitSource::Notes;
        assert_eq!(row_label(&from_notes), "p. 5 · notes");
    }

    #[test]
    fn a_row_is_split_around_its_match() {
        let (before, matched, after) = row_parts(&hit(0, 0));
        assert_eq!(
            (before.as_str(), matched.as_str(), after.as_str()),
            ("a ", "needle", " here")
        );
    }

    #[test]
    fn hits_are_grouped_once_per_page_for_the_workspace() {
        let hits = vec![hit(0, 0), hit(0, 1), hit(4, 0), hit(4, 1), hit(4, 2)];
        let groups = page_groups(&hits);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].page, PageIndex(0));
        assert_eq!(groups[0].hit_range, 0..2);
        assert_eq!(groups[1].page, PageIndex(4));
        assert_eq!(groups[1].hit_range, 2..5);
    }

    #[test]
    fn a_thousand_page_result_stream_builds_only_the_visible_window() {
        let visible = visible_group_range(1_000, 200.0 * RESULT_ROW_HEIGHT, 600.0);

        assert!(visible.start > 0);
        assert!(visible.end < 1_000);
        let viewport_rows = (600.0 / RESULT_ROW_HEIGHT).ceil() as usize;
        assert!(
            visible.len() <= viewport_rows + RESULT_OVERSCAN_ROWS * 2 + 1,
            "only visible rows plus overscan are built"
        );
    }
}
