//! The application's half of an image document.
//!
//! `SPEC-images.md` §42.1 and §43. **The application owns the page table.**
//! It lists the directory itself, holds the ordered file names, and uses them
//! for identity across a reload and for the digest comparison that makes a
//! disagreement with the worker's own listing detectable. None of that
//! requires decoding anything.
//!
//! `pulpit-core` never learns what a file name is (§43.4): the translation
//! from names to indices lives here, next to the table it belongs to, and
//! `replace_document` keeps its index semantics unchanged.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use pulpit_render::images::{list_directory, ListError, PageTable, ResolvedSource};

/// Where the presentation should sit after a document is promoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Positions {
    pub committed: usize,
    pub preview: usize,
}

/// One open image document, from the application's side.
#[derive(Debug)]
pub struct ImageDocumentState {
    directory: PathBuf,
    /// The table the *active* document was listed with. `None` until the
    /// first promote.
    active: Option<PageTable>,
    /// The table listed for the candidate currently being opened.
    candidate: Option<PageTable>,
    /// The file the presenter picked when they opened a file rather than a
    /// folder (§40.2). Consumed by the first promote, which is where it
    /// becomes the initial committed page.
    picked: Option<OsString>,
    /// Has §40.3's statement been made yet? The presenter must be told the
    /// resolved directory and its page count *before any navigation happens*
    /// — silently sweeping up four hundred siblings is the failure mode the
    /// rule exists to prevent.
    announced: bool,
    /// Pages whose render failure has already been reported, so a broken
    /// file is named once rather than once per frame that wanted it.
    reported: std::collections::HashSet<usize>,
}

impl ImageDocumentState {
    pub fn new(resolved: ResolvedSource) -> ImageDocumentState {
        ImageDocumentState {
            directory: resolved.directory,
            active: None,
            candidate: None,
            picked: resolved.picked,
            announced: false,
            reported: std::collections::HashSet::new(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// True when the document is larger than what the presenter picked, and
    /// they have not been told so yet.
    pub fn owes_an_announcement(&self) -> bool {
        self.picked.is_some() && !self.announced
    }

    /// The one sentence §40.3 requires, and the record that it was said.
    pub fn announcement(&mut self) -> Option<String> {
        if !self.owes_an_announcement() {
            return None;
        }
        self.announced = true;
        let pages = self
            .candidate
            .as_ref()
            .or(self.active.as_ref())
            .map(PageTable::len)?;
        Some(format!(
            "Showing {} — {pages} image{}",
            self.directory.display(),
            if pages == 1 { "" } else { "s" }
        ))
    }

    /// List the directory for a candidate about to be opened.
    ///
    /// Called as the open is issued, so the application's listing and the
    /// worker's are as close together in time as they can be. They can still
    /// disagree, and [`Self::agrees_with`] is what notices (§42.3).
    pub fn list_candidate(&mut self) -> Result<usize, ListError> {
        let table = list_directory(&self.directory)?;
        let count = table.len();
        self.candidate = Some(table);
        Ok(count)
    }

    /// Does the worker's digest match the application's own listing?
    ///
    /// A worker that reports no digest at all is answering about something
    /// that is not a directory — an old binary, or a routing mistake — and
    /// that is a disagreement too.
    pub fn agrees_with(&self, worker: Option<u64>) -> bool {
        match (self.candidate.as_ref(), worker) {
            (Some(table), Some(digest)) => table.digest() == digest,
            _ => false,
        }
    }

    /// The candidate's page count, as the application listed it.
    pub fn candidate_pages(&self) -> Option<usize> {
        self.candidate.as_ref().map(PageTable::len)
    }

    /// Give up on the candidate without promoting it.
    pub fn discard_candidate(&mut self) {
        self.candidate = None;
    }

    /// Promote the candidate, translating `positions` from names to indices
    /// in the new table (§43.3).
    ///
    /// Returns `None` when there is no candidate to promote, which is a late
    /// reply for something already abandoned.
    pub fn promote(&mut self, positions: Positions) -> Option<Positions> {
        let candidate = self.candidate.take()?;
        // A file that was broken may have been fixed; a fresh table earns a
        // fresh chance to say so.
        self.reported.clear();
        let translated = match self.active.as_ref() {
            // A reload: page identity is the file name, not the index (§43.3).
            Some(previous) => Positions {
                committed: candidate.reindex_from(previous, positions.committed),
                preview: candidate.reindex_from(previous, positions.preview),
            },
            // The first open: the presenter picked a file, and that file is
            // where the document starts (§40.2).
            None => {
                let start = self
                    .picked
                    .as_deref()
                    .and_then(|name| candidate.index_of(name))
                    .unwrap_or(0);
                Positions {
                    committed: start,
                    preview: start,
                }
            }
        };
        self.active = Some(candidate);
        Some(translated)
    }

    /// The promoted table, for anything that needs to name a page's file —
    /// a render failure in particular (§49.3).
    pub fn active(&self) -> Option<&PageTable> {
        self.active.as_ref()
    }

    /// The file behind one page of the active document.
    pub fn page_name(&self, page: usize) -> Option<String> {
        self.active
            .as_ref()?
            .name(page)
            .map(|name| name.to_string_lossy().into_owned())
    }

    /// The name of a page whose render just failed, the *first* time it
    /// fails (§49.3).
    ///
    /// One broken file is asked for by the audience frame, the presenter
    /// frame and its thumbnail, and a reload asks again; saying so once is
    /// telling the presenter something, and saying it six times is noise
    /// covering the slide they are trying to give.
    pub fn note_render_failure(&mut self, page: usize) -> Option<String> {
        if !self.reported.insert(page) {
            return None;
        }
        self.page_name(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_render::images::{ImageEntry, PageTable};

    fn table(names: &[&str]) -> PageTable {
        PageTable::from_entries(
            "/pictures/talk",
            names
                .iter()
                .map(|name| ImageEntry {
                    name: OsString::from(name),
                    len: 1,
                    modified: None,
                })
                .collect(),
        )
    }

    fn state(picked: Option<&str>) -> ImageDocumentState {
        ImageDocumentState::new(ResolvedSource {
            directory: PathBuf::from("/pictures/talk"),
            picked: picked.map(OsString::from),
        })
    }

    fn at(index: usize) -> Positions {
        Positions {
            committed: index,
            preview: index,
        }
    }

    #[test]
    fn opening_a_file_starts_the_document_on_that_file() {
        let mut state = state(Some("c.png"));
        state.candidate = Some(table(&["a.png", "b.png", "c.png"]));
        assert_eq!(state.promote(at(0)), Some(at(2)));
    }

    #[test]
    fn opening_a_folder_starts_at_its_first_page() {
        let mut state = state(None);
        state.candidate = Some(table(&["a.png", "b.png"]));
        assert_eq!(state.promote(at(0)), Some(at(0)));
    }

    /// §43.2, as the regression it is: adding a file earlier in sort order
    /// shifts every later index, so an index-preserving reload would change
    /// the audience frame to unrelated content with no navigation.
    #[test]
    fn a_reload_follows_the_file_name_and_not_the_index() {
        let mut state = state(None);
        state.candidate = Some(table(&["b.png", "c.png"]));
        state.promote(at(0));

        state.candidate = Some(table(&["a.png", "b.png", "c.png"]));
        assert_eq!(
            state.promote(Positions {
                committed: 0,
                preview: 1
            }),
            Some(Positions {
                committed: 1,
                preview: 2
            }),
            "b.png and c.png both moved down one; the audience keeps its picture"
        );
    }

    #[test]
    fn deleting_the_page_on_screen_advances_to_the_next_one() {
        let mut state = state(None);
        state.candidate = Some(table(&["a.png", "b.png", "c.png"]));
        state.promote(at(0));
        state.candidate = Some(table(&["a.png", "c.png"]));
        assert_eq!(state.promote(at(1)), Some(at(1)), "which is now c.png");
    }

    #[test]
    fn a_disagreement_with_the_worker_is_detectable() {
        let mut state = state(None);
        state.candidate = Some(table(&["a.png"]));
        let ours = state.candidate.as_ref().unwrap().digest();
        assert!(state.agrees_with(Some(ours)));
        assert!(!state.agrees_with(Some(ours ^ 1)));
        assert!(
            !state.agrees_with(None),
            "a worker that reports no digest is not answering about this folder"
        );
    }

    #[test]
    fn the_resolution_is_stated_once_and_names_the_directory_and_the_count() {
        let mut state = state(Some("shot.png"));
        state.candidate = Some(table(&["a.png", "b.png", "shot.png"]));
        let said = state.announcement().expect("§40.3");
        assert!(said.contains("/pictures/talk"), "{said}");
        assert!(said.contains('3'), "{said}");
        assert!(
            state.announcement().is_none(),
            "said once, before any navigation — not on every reload"
        );
    }

    #[test]
    fn opening_a_folder_owes_no_announcement() {
        let mut state = state(None);
        state.candidate = Some(table(&["a.png"]));
        assert!(!state.owes_an_announcement());
        assert!(state.announcement().is_none());
    }
}
