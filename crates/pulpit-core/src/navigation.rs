//! The shared document-navigation model: page labels, the outline tree and
//! the section a page belongs to.
//!
//! Everything a presenter navigates *by* other than the page number lives
//! here — the bookmark tree a producer wrote, the labels beamer emits for
//! incremental reveals, and the "you are in section 3" answer derived from
//! them. The model is pure: extracting it from a PDF is the renderer's job,
//! and displaying it is the application's.
//!
//! The bounds in this module exist because an outline is attacker-controlled
//! data: a malformed document can name a bookmark whose child is itself, and
//! a nesting depth no reader could show. [`build_outline`] walks such a tree
//! without recursing forever, and every string it keeps is truncated before
//! it is stored.

use std::collections::HashSet;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use crate::document::LinkTarget;
use crate::overlay::PageLabels;

/// A bookmark title longer than this is not a section name; it is a paragraph
/// that happened to be pasted into the outline. The rest is dropped.
pub const MAX_OUTLINE_TITLE_CHARS: usize = 200;

/// Nesting deeper than this is never shown and never useful, and a cyclic
/// document can claim any depth at all.
pub const MAX_OUTLINE_DEPTH: usize = 12;

/// More outline entries than this cannot be navigated by a human under time
/// pressure, and the whole tree travels in one IPC message.
pub const MAX_OUTLINE_ENTRIES: usize = 4096;

/// One node of the document outline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutlineEntry {
    /// The bookmark title, trimmed and truncated to
    /// [`MAX_OUTLINE_TITLE_CHARS`].
    pub title: String,
    pub target: LinkTarget,
    /// Distance from the top level; the top level is zero.
    pub depth: usize,
    pub children: Vec<OutlineEntry>,
}

impl OutlineEntry {
    /// The physical page this entry jumps to, when it jumps inside the
    /// document at all. A bookmark pointing at a URI orders nothing.
    pub fn page(&self) -> Option<usize> {
        match &self.target {
            LinkTarget::Page { page, .. } => Some(*page),
            LinkTarget::Uri(_) => None,
        }
    }
}

/// The outline (bookmark) tree of one document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Outline {
    pub entries: Vec<OutlineEntry>,
}

impl Outline {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total number of entries at every depth.
    pub fn len(&self) -> usize {
        self.flattened().len()
    }

    /// Every entry in reading order: an entry, then its subtree, then its
    /// next sibling. This is the order a presenter scans a table of contents,
    /// and the order a jump list must be built in.
    pub fn flattened(&self) -> Vec<&OutlineEntry> {
        let mut out = Vec::new();
        fn walk<'a>(entries: &'a [OutlineEntry], out: &mut Vec<&'a OutlineEntry>) {
            for entry in entries {
                out.push(entry);
                walk(&entry.children, out);
            }
        }
        walk(&self.entries, &mut out);
        out
    }

    /// Every entry at exactly one depth, in reading order. Depth zero is the
    /// list of sections.
    pub fn entries_at_depth(&self, depth: usize) -> Vec<&OutlineEntry> {
        self.flattened()
            .into_iter()
            .filter(|entry| entry.depth == depth)
            .collect()
    }
}

/// A tree the outline can be built from, abstracted so the walk itself — the
/// part with the cycle and depth guards — is testable without a PDF.
pub trait OutlineSource {
    /// Whatever identifies a node to the underlying library. Bookmarks are
    /// raw pointers in PDFium's case, which is exactly why identity has to be
    /// tracked to detect a cycle.
    type Node: Copy + Eq + Hash;

    /// The first child of `node`, or the first top-level entry for `None`.
    fn first_child(&self, node: Option<Self::Node>) -> Option<Self::Node>;

    fn next_sibling(&self, node: Self::Node) -> Option<Self::Node>;

    /// The node's title, already decoded. `None` drops the node's own entry
    /// but not its children.
    fn title(&self, node: Self::Node) -> Option<String>;

    /// Where the node jumps. `None` drops the node's own entry.
    fn target(&self, node: Self::Node) -> Option<LinkTarget>;
}

/// Walk an [`OutlineSource`] into an [`Outline`].
///
/// The walk is iterative in its sibling chain and bounded in every other
/// direction: a node already seen is never entered twice (a malformed
/// document can make a bookmark its own descendant, or two siblings point at
/// each other), depth stops at [`MAX_OUTLINE_DEPTH`], and the total entry
/// count stops at [`MAX_OUTLINE_ENTRIES`]. Whatever was collected before a
/// bound was hit is kept: a partial table of contents is more useful to a
/// presenter than none.
pub fn build_outline<S: OutlineSource>(source: &S) -> Outline {
    let mut visited = HashSet::new();
    let mut budget = MAX_OUTLINE_ENTRIES;
    Outline {
        entries: collect_level(source, None, 0, &mut visited, &mut budget),
    }
}

fn collect_level<S: OutlineSource>(
    source: &S,
    parent: Option<S::Node>,
    depth: usize,
    visited: &mut HashSet<S::Node>,
    budget: &mut usize,
) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    if depth >= MAX_OUTLINE_DEPTH {
        return entries;
    }
    let mut node = source.first_child(parent);
    while let Some(current) = node {
        if *budget == 0 || !visited.insert(current) {
            break;
        }
        *budget -= 1;
        let children = collect_level(source, Some(current), depth + 1, visited, budget);
        match (source.title(current), source.target(current)) {
            (Some(title), Some(target)) => entries.push(OutlineEntry {
                title: truncate_title(&title),
                target,
                depth,
                children,
            }),
            // A bookmark without a usable title or destination is a container
            // for its children, not a place to jump to; the children are
            // lifted so the branch is not lost.
            _ => entries.extend(children.into_iter().map(|child| lift(child, depth))),
        }
        node = source.next_sibling(current);
    }
    entries
}

/// Move an entry and its subtree one level towards the root, after its parent
/// turned out not to be navigable.
fn lift(entry: OutlineEntry, depth: usize) -> OutlineEntry {
    OutlineEntry {
        title: entry.title,
        target: entry.target,
        depth,
        children: entry
            .children
            .into_iter()
            .map(|child| lift(child, depth + 1))
            .collect(),
    }
}

fn truncate_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.chars().count() <= MAX_OUTLINE_TITLE_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(MAX_OUTLINE_TITLE_CHARS).collect()
}

/// Everything one document offers for navigating by something other than the
/// page number.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocumentNavigation {
    pub labels: PageLabels,
    pub outline: Outline,
}

impl DocumentNavigation {
    pub fn new(labels: PageLabels, outline: Outline) -> Self {
        Self { labels, outline }
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.outline.is_empty()
    }

    /// The producer's label for a page, when there is one.
    pub fn label_for_page(&self, page: usize) -> Option<&str> {
        self.labels.label(page)
    }

    /// Top-level outline entries: the sections of the talk.
    pub fn sections(&self) -> Vec<&OutlineEntry> {
        self.outline.entries_at_depth(0)
    }

    /// The section a page is in: the title of the latest outline entry that
    /// starts at or before it.
    ///
    /// Later wins on a tie, and a deeper entry wins over its own parent,
    /// because a subsection starting on the same page as its section is the
    /// more specific truth about where the presenter is. A page before the
    /// first bookmark belongs to no section rather than to the first one.
    pub fn section_for_page(&self, page: usize) -> Option<&str> {
        self.outline
            .flattened()
            .into_iter()
            .filter_map(|entry| entry.page().map(|start| (start, entry)))
            .filter(|(start, _)| *start <= page)
            .max_by_key(|(start, entry)| (*start, entry.depth))
            .map(|(_, entry)| entry.title.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A tree described as `(node, title, page, children)`, so the walk can be
    /// driven — including into cycles — without a PDF.
    #[derive(Default)]
    struct FakeTree {
        children: HashMap<Option<u32>, Vec<u32>>,
        titles: HashMap<u32, Option<String>>,
        pages: HashMap<u32, Option<usize>>,
    }

    impl FakeTree {
        fn node(
            &mut self,
            parent: Option<u32>,
            node: u32,
            title: Option<&str>,
            page: Option<usize>,
        ) {
            self.children.entry(parent).or_default().push(node);
            self.titles.insert(node, title.map(str::to_string));
            self.pages.insert(node, page);
        }
    }

    impl OutlineSource for FakeTree {
        type Node = u32;

        fn first_child(&self, node: Option<u32>) -> Option<u32> {
            self.children
                .get(&node)
                .and_then(|kids| kids.first())
                .copied()
        }

        fn next_sibling(&self, node: u32) -> Option<u32> {
            self.children.values().find_map(|kids| {
                let position = kids.iter().position(|kid| *kid == node)?;
                kids.get(position + 1).copied()
            })
        }

        fn title(&self, node: u32) -> Option<String> {
            self.titles.get(&node).cloned().flatten()
        }

        fn target(&self, node: u32) -> Option<LinkTarget> {
            let page = (*self.pages.get(&node)?)?;
            Some(LinkTarget::Page { page, zoom: None })
        }
    }

    fn talk() -> Outline {
        let mut tree = FakeTree::default();
        tree.node(None, 1, Some("Introduction"), Some(0));
        tree.node(None, 2, Some("Method"), Some(4));
        tree.node(Some(2), 3, Some("Setup"), Some(4));
        tree.node(Some(2), 4, Some("Measurements"), Some(7));
        tree.node(None, 5, Some("Conclusion"), Some(11));
        build_outline(&tree)
    }

    #[test]
    fn an_outline_flattens_into_reading_order() {
        let outline = talk();
        let titles: Vec<&str> = outline
            .flattened()
            .iter()
            .map(|entry| entry.title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec![
                "Introduction",
                "Method",
                "Setup",
                "Measurements",
                "Conclusion"
            ]
        );
    }

    #[test]
    fn depth_is_recorded_and_queryable() {
        let outline = talk();
        let top: Vec<&str> = outline
            .entries_at_depth(0)
            .iter()
            .map(|entry| entry.title.as_str())
            .collect();
        assert_eq!(top, vec!["Introduction", "Method", "Conclusion"]);
        let nested: Vec<&str> = outline
            .entries_at_depth(1)
            .iter()
            .map(|entry| entry.title.as_str())
            .collect();
        assert_eq!(nested, vec!["Setup", "Measurements"]);
        assert_eq!(outline.len(), 5);
    }

    #[test]
    fn a_cyclic_outline_terminates_instead_of_recursing_forever() {
        let mut tree = FakeTree::default();
        tree.node(None, 1, Some("Loop"), Some(0));
        // The node is its own child *and* its own sibling: both directions of
        // the walk are made cyclic at once.
        tree.children.insert(Some(1), vec![1]);
        let outline = build_outline(&tree);
        assert_eq!(outline.len(), 1);
        assert_eq!(outline.entries[0].title, "Loop");
        assert!(outline.entries[0].children.is_empty());
    }

    #[test]
    fn nesting_stops_at_the_depth_bound() {
        let mut tree = FakeTree::default();
        let mut parent = None;
        for node in 1..(MAX_OUTLINE_DEPTH as u32 + 6) {
            tree.node(parent, node, Some(&format!("level {node}")), Some(0));
            parent = Some(node);
        }
        let outline = build_outline(&tree);
        assert_eq!(outline.len(), MAX_OUTLINE_DEPTH);
        assert!(outline
            .flattened()
            .iter()
            .all(|entry| entry.depth < MAX_OUTLINE_DEPTH));
    }

    #[test]
    fn the_entry_count_is_bounded() {
        let mut tree = FakeTree::default();
        for node in 1..(MAX_OUTLINE_ENTRIES as u32 + 100) {
            tree.node(None, node, Some("section"), Some(0));
        }
        assert_eq!(build_outline(&tree).len(), MAX_OUTLINE_ENTRIES);
    }

    #[test]
    fn titles_are_trimmed_and_truncated() {
        let mut tree = FakeTree::default();
        tree.node(None, 1, Some("  Introduction \n"), Some(0));
        tree.node(
            None,
            2,
            Some(&"x".repeat(MAX_OUTLINE_TITLE_CHARS + 50)),
            Some(1),
        );
        let outline = build_outline(&tree);
        assert_eq!(outline.entries[0].title, "Introduction");
        assert_eq!(
            outline.entries[1].title.chars().count(),
            MAX_OUTLINE_TITLE_CHARS
        );
    }

    #[test]
    fn a_bookmark_without_a_destination_lifts_its_children() {
        let mut tree = FakeTree::default();
        tree.node(None, 1, Some("Container"), None);
        tree.node(Some(1), 2, Some("Real section"), Some(3));
        let outline = build_outline(&tree);
        assert_eq!(outline.len(), 1);
        assert_eq!(outline.entries[0].title, "Real section");
        assert_eq!(
            outline.entries[0].depth, 0,
            "a lifted child takes its parent's place"
        );
    }

    // -- sections -----------------------------------------------------------

    fn navigation() -> DocumentNavigation {
        DocumentNavigation::new(PageLabels::default(), talk())
    }

    #[test]
    fn a_page_belongs_to_the_latest_section_that_starts_at_or_before_it() {
        let navigation = navigation();
        assert_eq!(navigation.section_for_page(0), Some("Introduction"));
        assert_eq!(navigation.section_for_page(3), Some("Introduction"));
        assert_eq!(
            navigation.section_for_page(4),
            Some("Setup"),
            "the subsection starting on the same page is the more specific answer"
        );
        assert_eq!(navigation.section_for_page(6), Some("Setup"));
        assert_eq!(navigation.section_for_page(7), Some("Measurements"));
        assert_eq!(navigation.section_for_page(10), Some("Measurements"));
        assert_eq!(navigation.section_for_page(11), Some("Conclusion"));
        assert_eq!(navigation.section_for_page(999), Some("Conclusion"));
    }

    #[test]
    fn a_page_before_the_first_bookmark_is_in_no_section() {
        let mut tree = FakeTree::default();
        tree.node(None, 1, Some("Method"), Some(5));
        let navigation = DocumentNavigation::new(PageLabels::default(), build_outline(&tree));
        assert_eq!(navigation.section_for_page(4), None);
        assert_eq!(navigation.section_for_page(5), Some("Method"));
    }

    #[test]
    fn a_document_without_an_outline_answers_nothing_rather_than_guessing() {
        let navigation = DocumentNavigation::default();
        assert!(navigation.is_empty());
        assert_eq!(navigation.section_for_page(0), None);
        assert!(navigation.sections().is_empty());
    }

    #[test]
    fn bookmarks_pointing_outside_the_document_do_not_order_sections() {
        let mut tree = FakeTree::default();
        tree.node(None, 1, Some("Method"), Some(2));
        tree.titles.insert(2, Some("Homepage".into()));
        tree.children.entry(None).or_default().push(2);
        // Node 2 has no page: `target` answers `None`, so it is not a section.
        let navigation = DocumentNavigation::new(PageLabels::default(), build_outline(&tree));
        assert_eq!(navigation.section_for_page(9), Some("Method"));
    }

    #[test]
    fn page_labels_travel_with_the_navigation_model() {
        let labels = PageLabels {
            labels: [(0, "i".to_string()), (1, "1".to_string())]
                .into_iter()
                .collect(),
        };
        let navigation = DocumentNavigation::new(labels, Outline::default());
        assert_eq!(navigation.label_for_page(0), Some("i"));
        assert_eq!(navigation.label_for_page(9), None);
        assert!(!navigation.is_empty());
    }
}
