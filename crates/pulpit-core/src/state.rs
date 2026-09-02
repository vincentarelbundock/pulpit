//! The authoritative presentation state.
//!
//! Both windows render projections of this value. Navigation is expressed as
//! [`Command`]s and reduced into a new state so input arriving from either
//! window cannot produce divergent page state.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::document::{DocumentInfo, PageSize};
use crate::generation::RenderGeneration;
use crate::notes::{NotesMapping, PageSource, Region};
use crate::timer::Timer;

/// A logical (0-based) slide index. Logical, not a borrowed PDF page object,
/// so it survives a document reload.
pub type SlideIndex = usize;

/// Audience blanking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Blank {
    #[default]
    Off,
    /// Black is the default: the audience view letterboxes in black too.
    Black,
    /// White is only ever produced by explicit user request.
    White,
}

impl Blank {
    pub fn is_blanked(self) -> bool {
        !matches!(self, Blank::Off)
    }
}

/// Everything an input source can ask the presentation to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Move the audience (and preview) forward one slide.
    Next,
    /// Move the audience (and preview) back one slide.
    Previous,
    First,
    Last,
    /// Commit a specific slide directly to the audience.
    GoTo(SlideIndex),
    /// Move only the presenter preview; the audience does not follow.
    PreviewNext,
    PreviewPrevious,
    PreviewGoTo(SlideIndex),
    /// Commit whatever the presenter is previewing. The audience never sees
    /// the intermediate pages traversed while choosing it.
    CommitPreview,
    /// Abandon the preview and snap it back to the committed slide.
    CancelPreview,
    SetBlank(Blank),
    ToggleBlack,
    ToggleWhite,
    StartTimer,
    PauseTimer,
    ToggleTimer,
    ResetTimer,
    SetNotesMapping(NotesMapping),
    /// Zoom the audience view to a region of the committed page (a `/FitR`
    /// link destination), or clear the zoom with `None`. Any navigation
    /// clears it too.
    SetZoom(Option<Region>),
    /// Promote a validated replacement document (see `DocumentManager`).
    ReplaceDocument(DocumentInfo),
    /// A DPI/scale or target-resolution change: pixels are stale, state is not.
    InvalidateRenders,
}

/// What a [`Command`] actually changed. The UI uses this to decide what to
/// re-render and what to leave alone; a no-op command changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Changed {
    pub committed: bool,
    pub preview: bool,
    pub blank: bool,
    pub timer: bool,
    pub document: bool,
    pub mapping: bool,
    pub generation: bool,
}

impl Changed {
    pub fn any(&self) -> bool {
        self.committed
            || self.preview
            || self.blank
            || self.timer
            || self.document
            || self.mapping
            || self.generation
    }

    fn merge(self, other: Changed) -> Changed {
        Changed {
            committed: self.committed || other.committed,
            preview: self.preview || other.preview,
            blank: self.blank || other.blank,
            timer: self.timer || other.timer,
            document: self.document || other.document,
            mapping: self.mapping || other.mapping,
            generation: self.generation || other.generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresentationState {
    document: Option<DocumentInfo>,
    mapping: NotesMapping,
    committed: SlideIndex,
    preview: SlideIndex,
    blank: Blank,
    timer: Timer,
    generation: RenderGeneration,
    /// Region of the committed page the audience sees, set by a `/FitR`
    /// link. Ephemeral: any navigation clears it.
    zoom: Option<Region>,
}

impl Default for PresentationState {
    fn default() -> Self {
        Self {
            document: None,
            mapping: NotesMapping::default(),
            committed: 0,
            preview: 0,
            blank: Blank::Off,
            timer: Timer::default(),
            generation: RenderGeneration::ZERO,
            zoom: None,
        }
    }
}

impl PresentationState {
    pub fn new(document: DocumentInfo, mapping: NotesMapping) -> Self {
        Self {
            document: Some(document),
            mapping,
            generation: RenderGeneration(1),
            ..Self::default()
        }
    }

    pub fn document(&self) -> Option<&DocumentInfo> {
        self.document.as_ref()
    }

    pub fn mapping(&self) -> &NotesMapping {
        &self.mapping
    }

    pub fn generation(&self) -> RenderGeneration {
        self.generation
    }

    pub fn blank(&self) -> Blank {
        self.blank
    }

    pub fn timer(&self) -> &Timer {
        &self.timer
    }

    pub fn timer_mut(&mut self) -> &mut Timer {
        &mut self.timer
    }

    pub fn committed(&self) -> SlideIndex {
        self.committed
    }

    pub fn preview(&self) -> SlideIndex {
        self.preview
    }

    /// True when the presenter is looking at a slide the audience is not.
    pub fn preview_is_detached(&self) -> bool {
        self.preview != self.committed
    }

    pub fn slide_count(&self) -> usize {
        self.document
            .as_ref()
            .map(|d| self.mapping.slide_count(d.pdf_pages))
            .unwrap_or(0)
    }

    pub fn pdf_pages(&self) -> usize {
        self.document.as_ref().map(|d| d.pdf_pages).unwrap_or(0)
    }

    pub fn first_page_size(&self) -> Option<PageSize> {
        self.document.as_ref().and_then(|d| d.first_page_size())
    }

    /// Image shown on the audience display right now (`None` while blanked or
    /// with no document).
    pub fn audience_source(&self) -> Option<PageSource> {
        if self.blank.is_blanked() {
            return None;
        }
        let mut source = self
            .mapping
            .audience_source(self.committed, self.pdf_pages())?;
        if let Some(zoom) = &self.zoom {
            // The zoom rectangle is in page coordinates, the same space as
            // the mapping crop; a zoom outside the crop is ignored.
            if let Some(region) = source.region.intersect(zoom) {
                source.region = region;
            }
        }
        Some(source)
    }

    /// The `/FitR` zoom in force on the committed page, if any.
    pub fn zoom(&self) -> Option<Region> {
        self.zoom
    }

    pub fn preview_source(&self) -> Option<PageSource> {
        self.mapping.audience_source(self.preview, self.pdf_pages())
    }

    pub fn notes_source(&self) -> Option<PageSource> {
        self.mapping.notes_source(self.preview, self.pdf_pages())
    }

    /// Pages worth having in cache, most important first. This is exactly the
    /// renderer priority order from the specification.
    pub fn priority_slides(&self) -> Vec<SlideIndex> {
        let count = self.slide_count();
        let mut wanted = Vec::new();
        let push = |slide: i64, out: &mut Vec<SlideIndex>| {
            if slide >= 0 && (slide as usize) < count {
                let slide = slide as usize;
                if !out.contains(&slide) {
                    out.push(slide);
                }
            }
        };
        push(self.committed as i64, &mut wanted);
        push(self.preview as i64, &mut wanted);
        push(self.preview as i64 + 1, &mut wanted);
        push(self.committed as i64 + 1, &mut wanted);
        push(self.preview as i64 - 1, &mut wanted);
        push(self.committed as i64 - 1, &mut wanted);
        push(self.preview as i64 + 2, &mut wanted);
        // Two behind as well as two ahead. Walking backward, the previous
        // panel shows `committed - 2` the moment the key lands; without this
        // entry that page was never warm and every backward step popped in a
        // late-arriving frame, where forward steps were seamless.
        push(self.preview as i64 - 2, &mut wanted);
        push(self.committed as i64 - 2, &mut wanted);
        wanted
    }

    fn clamp(&self, slide: i64) -> SlideIndex {
        let count = self.slide_count();
        if count == 0 {
            return 0;
        }
        slide.clamp(0, count as i64 - 1) as usize
    }

    fn commit(&mut self, slide: i64) -> Changed {
        let target = self.clamp(slide);
        let mut changed = Changed::default();
        if self.committed != target {
            self.committed = target;
            changed.committed = true;
        }
        if self.preview != target {
            self.preview = target;
            changed.preview = true;
        }
        changed
    }

    fn set_preview(&mut self, slide: i64) -> Changed {
        let target = self.clamp(slide);
        let mut changed = Changed::default();
        if self.preview != target {
            self.preview = target;
            changed.preview = true;
        }
        changed
    }

    fn set_blank(&mut self, blank: Blank) -> Changed {
        let mut changed = Changed::default();
        if self.blank != blank {
            self.blank = blank;
            changed.blank = true;
        }
        changed
    }

    /// Replace the zoom. Pixels are page content either way, so this is an
    /// invalidation exactly like `InvalidateRenders`: old frames stay up
    /// until the re-cropped ones land.
    fn set_zoom(&mut self, zoom: Option<Region>) -> Changed {
        let mut changed = Changed::default();
        if self.zoom != zoom {
            self.zoom = zoom;
            self.generation.advance();
            changed.generation = true;
        }
        changed
    }

    /// Reduce a command into new state. `now` is only consulted by timer
    /// commands, so display events and reloads never disturb the clock.
    pub fn apply(&mut self, command: Command, now: Instant) -> Changed {
        let mut changed = Changed::default();
        // A zoom lives only until the audience moves: any command that can
        // change what the audience sees drops it first.
        let cleared = if matches!(
            command,
            Command::Next
                | Command::Previous
                | Command::First
                | Command::Last
                | Command::GoTo(_)
                | Command::CommitPreview
                | Command::SetNotesMapping(_)
                | Command::ReplaceDocument(_)
        ) {
            self.set_zoom(None)
        } else {
            Changed::default()
        };
        match command {
            Command::Next => changed = self.commit(self.committed as i64 + 1),
            Command::Previous => changed = self.commit(self.committed as i64 - 1),
            Command::First => changed = self.commit(0),
            Command::Last => changed = self.commit(self.slide_count() as i64 - 1),
            Command::GoTo(slide) => changed = self.commit(slide as i64),
            Command::PreviewNext => changed = self.set_preview(self.preview as i64 + 1),
            Command::PreviewPrevious => changed = self.set_preview(self.preview as i64 - 1),
            Command::PreviewGoTo(slide) => changed = self.set_preview(slide as i64),
            Command::CommitPreview => changed = self.commit(self.preview as i64),
            Command::CancelPreview => changed = self.set_preview(self.committed as i64),
            Command::SetBlank(blank) => changed = self.set_blank(blank),
            Command::ToggleBlack => {
                let target = if self.blank == Blank::Black {
                    Blank::Off
                } else {
                    Blank::Black
                };
                changed = self.set_blank(target);
            }
            Command::ToggleWhite => {
                let target = if self.blank == Blank::White {
                    Blank::Off
                } else {
                    Blank::White
                };
                changed = self.set_blank(target);
            }
            Command::StartTimer => {
                if !self.timer.is_running() {
                    self.timer.start(now);
                    changed.timer = true;
                }
            }
            Command::PauseTimer => {
                if self.timer.is_running() {
                    self.timer.pause(now);
                    changed.timer = true;
                }
            }
            Command::ToggleTimer => {
                self.timer.toggle(now);
                changed.timer = true;
            }
            Command::ResetTimer => {
                self.timer.reset(now);
                changed.timer = true;
            }
            Command::SetNotesMapping(mapping) => {
                if self.mapping != mapping {
                    self.mapping = mapping;
                    changed.mapping = true;
                    changed.generation = true;
                    self.generation.advance();
                    // Slide count may have changed under us.
                    let clamped = self.clamp(self.committed as i64);
                    changed = changed.merge(self.commit(clamped as i64));
                }
            }
            Command::SetZoom(zoom) => changed = self.set_zoom(zoom),
            Command::ReplaceDocument(info) => changed = self.replace_document(info),
            Command::InvalidateRenders => {
                self.generation.advance();
                changed.generation = true;
            }
        }
        cleared.merge(changed)
    }

    /// Promote a new document while preserving page, timer, blanking and
    /// mapping. The page is clamped when the replacement is shorter.
    fn replace_document(&mut self, info: DocumentInfo) -> Changed {
        let mut changed = Changed {
            document: true,
            generation: true,
            ..Changed::default()
        };
        self.document = Some(info);
        self.generation.advance();

        let count = self.slide_count();
        if count == 0 {
            if self.committed != 0 || self.preview != 0 {
                self.committed = 0;
                self.preview = 0;
                changed.committed = true;
                changed.preview = true;
            }
            return changed;
        }
        let max = count - 1;
        if self.committed > max {
            self.committed = max;
            changed.committed = true;
        }
        if self.preview > max {
            self.preview = max;
            changed.preview = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentInfo;
    use crate::notes::{PairedRule, Region};
    use std::time::Duration;

    fn state(pages: usize) -> PresentationState {
        PresentationState::new(
            DocumentInfo::new(crate::DocumentId(1), "/tmp/deck.pdf", pages),
            NotesMapping::SlidesOnly,
        )
    }

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn navigation_is_bounded() {
        let mut s = state(3);
        s.apply(Command::Previous, now());
        assert_eq!(s.committed(), 0);
        s.apply(Command::Last, now());
        assert_eq!(s.committed(), 2);
        s.apply(Command::Next, now());
        assert_eq!(s.committed(), 2, "next past the end is a no-op");
        assert!(!s.apply(Command::Next, now()).any());
        s.apply(Command::GoTo(99), now());
        assert_eq!(s.committed(), 2);
    }

    #[test]
    fn empty_document_navigation_does_not_panic() {
        let mut s = state(0);
        s.apply(Command::Next, now());
        s.apply(Command::Last, now());
        assert_eq!(s.committed(), 0);
        assert_eq!(s.audience_source(), None);
    }

    #[test]
    fn preview_never_moves_the_audience() {
        let mut s = state(10);
        s.apply(Command::PreviewGoTo(7), now());
        assert_eq!(s.committed(), 0, "audience stays put");
        assert_eq!(s.preview(), 7);
        assert!(s.preview_is_detached());
        assert_eq!(s.audience_source(), Some(PageSource::full(0)));

        s.apply(Command::CommitPreview, now());
        assert_eq!(s.committed(), 7);
        assert!(!s.preview_is_detached());
    }

    #[test]
    fn jump_to_page_shows_no_intermediate_page_to_the_audience() {
        let mut s = state(50);
        // Scrubbing the preview across many slides...
        for slide in 1..=42 {
            s.apply(Command::PreviewGoTo(slide), now());
            assert_eq!(s.committed(), 0, "audience must not follow the scrub");
        }
        s.apply(Command::CommitPreview, now());
        assert_eq!(s.committed(), 42);
    }

    #[test]
    fn zoom_crops_the_audience_source_and_navigation_clears_it() {
        let mut s = state(10);
        s.apply(Command::GoTo(3), now());
        let zoom = Region::new(0.25, 0.25, 0.5, 0.5);

        let changed = s.apply(Command::SetZoom(Some(zoom)), now());
        assert!(changed.generation, "a zoom invalidates rendered pixels");
        assert_eq!(s.zoom(), Some(zoom));
        assert_eq!(
            s.audience_source().map(|source| source.region),
            Some(zoom),
            "the audience sees only the zoomed region"
        );

        let generation = s.generation();
        let changed = s.apply(Command::Next, now());
        assert_eq!(s.zoom(), None, "navigation releases the zoom");
        assert!(changed.generation);
        assert!(s.generation() > generation);
        assert_eq!(s.audience_source(), Some(PageSource::full(4)));
    }

    #[test]
    fn setting_the_same_zoom_twice_changes_nothing() {
        let mut s = state(10);
        let zoom = Region::new(0.1, 0.1, 0.4, 0.4);
        s.apply(Command::SetZoom(Some(zoom)), now());
        let generation = s.generation();
        let changed = s.apply(Command::SetZoom(Some(zoom)), now());
        assert!(!changed.any());
        assert_eq!(s.generation(), generation);
    }

    #[test]
    fn preview_moves_do_not_release_the_zoom() {
        let mut s = state(10);
        let zoom = Region::new(0.0, 0.0, 0.5, 0.5);
        s.apply(Command::SetZoom(Some(zoom)), now());
        s.apply(Command::PreviewNext, now());
        s.apply(Command::SetBlank(Blank::Black), now());
        assert_eq!(s.zoom(), Some(zoom), "only audience navigation clears it");
    }

    #[test]
    fn cancel_preview_snaps_back() {
        let mut s = state(10);
        s.apply(Command::GoTo(4), now());
        s.apply(Command::PreviewGoTo(9), now());
        s.apply(Command::CancelPreview, now());
        assert_eq!(s.preview(), 4);
    }

    #[test]
    fn committed_navigation_drags_the_preview_along() {
        let mut s = state(10);
        s.apply(Command::PreviewGoTo(6), now());
        s.apply(Command::Next, now());
        assert_eq!(s.committed(), 1);
        assert_eq!(
            s.preview(),
            1,
            "committed navigation re-attaches the preview"
        );
    }

    #[test]
    fn blanking_hides_the_audience_source_only() {
        let mut s = state(5);
        s.apply(Command::GoTo(2), now());
        s.apply(Command::ToggleBlack, now());
        assert_eq!(s.blank(), Blank::Black);
        assert_eq!(s.audience_source(), None);
        assert_eq!(
            s.preview_source(),
            Some(PageSource::full(2)),
            "presenter keeps working"
        );
        s.apply(Command::Next, now());
        assert_eq!(s.committed(), 3, "navigation still works while blanked");
        s.apply(Command::ToggleBlack, now());
        assert_eq!(s.blank(), Blank::Off);
        assert_eq!(s.audience_source(), Some(PageSource::full(3)));
    }

    #[test]
    fn white_and_black_blanking_are_distinct_modes() {
        let mut s = state(5);
        s.apply(Command::ToggleWhite, now());
        assert_eq!(s.blank(), Blank::White);
        s.apply(Command::ToggleBlack, now());
        assert_eq!(
            s.blank(),
            Blank::Black,
            "black replaces white rather than toggling off"
        );
        s.apply(Command::ToggleWhite, now());
        assert_eq!(s.blank(), Blank::White);
    }

    #[test]
    fn document_replacement_preserves_state_and_clamps_the_page() {
        let start = now();
        let mut s = state(20);
        s.apply(Command::GoTo(15), start);
        s.apply(Command::ToggleBlack, start);
        s.apply(Command::StartTimer, start);
        let generation_before = s.generation();

        let later = start + Duration::from_secs(90);
        let changed = s.apply(
            Command::ReplaceDocument(DocumentInfo::new(crate::DocumentId(2), "/tmp/deck.pdf", 10)),
            later,
        );

        assert!(changed.document && changed.generation && changed.committed);
        assert_eq!(s.committed(), 9, "clamped to the shorter replacement");
        assert_eq!(s.blank(), Blank::Black, "blanking survives the reload");
        assert!(s.timer().is_running(), "timer survives the reload");
        assert_eq!(s.timer().elapsed(later), Duration::from_secs(90));
        assert!(s.generation() > generation_before);
    }

    #[test]
    fn document_replacement_with_more_pages_keeps_the_page() {
        let mut s = state(10);
        s.apply(Command::GoTo(5), now());
        s.apply(
            Command::ReplaceDocument(DocumentInfo::new(crate::DocumentId(2), "/x.pdf", 40)),
            now(),
        );
        assert_eq!(s.committed(), 5);
        assert_eq!(s.slide_count(), 40);
    }

    #[test]
    fn changing_the_mapping_advances_the_generation_and_clamps() {
        let mut s = state(10);
        s.apply(Command::GoTo(9), now());
        let changed = s.apply(
            Command::SetNotesMapping(NotesMapping::PairedPages(PairedRule::Alternating {
                notes_first: false,
            })),
            now(),
        );
        assert!(changed.mapping && changed.generation);
        assert_eq!(s.slide_count(), 5);
        assert_eq!(s.committed(), 4, "clamped into the new slide space");
    }

    #[test]
    fn split_mapping_projects_both_windows_from_one_page() {
        let mut s = state(4);
        s.apply(
            Command::SetNotesMapping(NotesMapping::SplitPage {
                slide: Region::left_half(),
                notes: Region::right_half(),
            }),
            now(),
        );
        s.apply(Command::GoTo(1), now());
        assert_eq!(
            s.audience_source(),
            Some(PageSource {
                pdf_page: 1,
                region: Region::left_half()
            })
        );
        assert_eq!(
            s.notes_source(),
            Some(PageSource {
                pdf_page: 1,
                region: Region::right_half()
            })
        );
    }

    #[test]
    fn priority_order_puts_the_audience_page_first() {
        let mut s = state(20);
        s.apply(Command::GoTo(5), now());
        s.apply(Command::PreviewGoTo(12), now());
        let priority = s.priority_slides();
        assert_eq!(priority[0], 5, "committed audience page first");
        assert_eq!(priority[1], 12, "then the presenter page");
        assert_eq!(priority[2], 13, "then the next page");
        assert!(priority.len() <= 9);
        assert!(
            priority.contains(&3) && priority.contains(&10),
            "two behind each cursor, so backward steps are as warm as forward ones"
        );
    }

    #[test]
    fn priority_order_is_clamped_at_the_deck_boundaries() {
        let mut s = state(2);
        s.apply(Command::Last, now());
        let priority = s.priority_slides();
        assert_eq!(priority, vec![1, 0]);
    }

    #[test]
    fn invalidating_renders_keeps_every_logical_value() {
        let start = now();
        let mut s = state(10);
        s.apply(Command::GoTo(4), start);
        s.apply(Command::StartTimer, start);
        s.apply(Command::ToggleWhite, start);
        let before = (s.committed(), s.preview(), s.blank());
        s.apply(Command::InvalidateRenders, start + Duration::from_secs(5));
        assert_eq!((s.committed(), s.preview(), s.blank()), before);
        assert_eq!(
            s.timer().elapsed(start + Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }
}
