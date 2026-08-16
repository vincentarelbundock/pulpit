//! Where the presenter has been: the back/forward stacks behind the two
//! navigation buttons.
//!
//! The rule this module encodes is that *stepping is not travelling*. Pressing
//! the right arrow two hundred times through a book is one continuous reading
//! motion, and a back button that unwound it one page at a time would be a
//! slower way of pressing the left arrow. Only a **jump** — picking a slide out
//! of the overview, following a link, committing a typed page number, landing
//! on a search hit — records anything, and what it records is the place the
//! jump left behind. Back therefore answers the question a presenter actually
//! asks under time pressure: *take me back to where I was before I went
//! looking*.
//!
//! The stacks hold [`Place`], which spans both modes, so a jump made in the
//! overview of a slide deck and a jump made in the document reader share one
//! history and survive switching between them.
//!
//! Nothing here reads a clock, touches a document, or knows what a window is:
//! it is two stacks and the rules for moving entries between them.

use crate::state::SlideIndex;

/// Entries beyond this are dropped from the oldest end. A presenter who has
/// made 256 jumps without once pressing back is not going to press it 257
/// times now, and the bound is what keeps a long session's history from
/// growing without limit.
pub const MAX_HISTORY_ENTRIES: usize = 256;

/// A position the presenter can return to.
///
/// The two modes count in different units — a deck moves by slide, the reader
/// moves by page, and the mapping between them is the application's job — so a
/// place remembers which kind it is rather than pretending there is one index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A committed slide in presentation mode.
    Slide(SlideIndex),
    /// A page in the document reader.
    Page(usize),
}

/// The back and forward stacks.
///
/// Ordinary browser semantics: a jump pushes onto back and discards forward,
/// going back moves the current place onto forward, going forward moves it
/// back again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavHistory {
    back: Vec<Place>,
    forward: Vec<Place>,
}

impl NavHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a jump from `from` to `to`, returning whether anything was
    /// recorded.
    ///
    /// A jump that does not move — clicking the overview cell the presenter is
    /// already on — is not history, and recording it would make back a no-op
    /// that has to be pressed twice.
    pub fn record_jump(&mut self, from: Place, to: Place) -> bool {
        if from == to {
            return false;
        }
        self.back.push(from);
        // Everything ahead is a future that this jump has just replaced.
        self.forward.clear();
        if self.back.len() > MAX_HISTORY_ENTRIES {
            self.back.remove(0);
        }
        true
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Step back, given where the presenter is now. `None` means there is no
    /// history to unwind, and the caller falls back to moving sequentially —
    /// the button is never dead.
    pub fn go_back(&mut self, current: Place) -> Option<Place> {
        let destination = self.back.pop()?;
        self.forward.push(current);
        Some(destination)
    }

    /// Step forward again after going back. `None` means the presenter is at
    /// the head of their history, and the caller moves sequentially instead.
    pub fn go_forward(&mut self, current: Place) -> Option<Place> {
        let destination = self.forward.pop()?;
        self.back.push(current);
        Some(destination)
    }

    /// Forget everything. Opening a different document makes every remembered
    /// place a reference to a page that no longer exists.
    pub fn clear(&mut self) {
        self.back.clear();
        self.forward.clear();
    }

    pub fn back_depth(&self) -> usize {
        self.back.len()
    }

    pub fn forward_depth(&self) -> usize {
        self.forward.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slide(index: usize) -> Place {
        Place::Slide(index)
    }

    #[test]
    fn a_fresh_history_has_nowhere_to_go() {
        let history = NavHistory::new();
        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());
    }

    #[test]
    fn going_back_returns_the_place_a_jump_left_behind() {
        let mut history = NavHistory::new();
        assert!(history.record_jump(slide(3), slide(40)));
        assert!(history.can_go_back());
        assert_eq!(history.go_back(slide(40)), Some(slide(3)));
        assert!(!history.can_go_back());
        assert!(history.can_go_forward());
    }

    #[test]
    fn forward_undoes_a_back() {
        let mut history = NavHistory::new();
        history.record_jump(slide(3), slide(40));
        history.go_back(slide(40));
        assert_eq!(history.go_forward(slide(3)), Some(slide(40)));
        assert!(history.can_go_back());
        assert!(!history.can_go_forward());
    }

    #[test]
    fn back_returns_where_the_presenter_actually_was_not_where_the_jump_landed() {
        // The presenter jumps to 40, then reads forward by hand to 47. Back
        // takes them to 3, and forward returns them to 47 — the place they
        // were, not the slide the jump originally landed on.
        let mut history = NavHistory::new();
        history.record_jump(slide(3), slide(40));
        assert_eq!(history.go_back(slide(47)), Some(slide(3)));
        assert_eq!(history.go_forward(slide(3)), Some(slide(47)));
    }

    #[test]
    fn stepping_records_nothing() {
        // There is simply no call to make: sequential movement never reaches
        // this type, so a long run of stepping leaves the stacks untouched.
        let history = NavHistory::new();
        assert_eq!(history.back_depth(), 0);
        assert!(!history.can_go_back());
    }

    #[test]
    fn a_jump_that_does_not_move_is_not_recorded() {
        let mut history = NavHistory::new();
        assert!(!history.record_jump(slide(7), slide(7)));
        assert!(!history.can_go_back());
    }

    #[test]
    fn a_new_jump_discards_the_forward_stack() {
        let mut history = NavHistory::new();
        history.record_jump(slide(3), slide(40));
        history.go_back(slide(40));
        assert!(history.can_go_forward());
        history.record_jump(slide(3), slide(90));
        assert!(!history.can_go_forward());
        assert_eq!(history.go_back(slide(90)), Some(slide(3)));
    }

    #[test]
    fn history_spans_both_modes() {
        let mut history = NavHistory::new();
        history.record_jump(Place::Page(12), Place::Slide(4));
        assert_eq!(history.go_back(Place::Slide(4)), Some(Place::Page(12)));
    }

    #[test]
    fn the_stack_is_bounded_and_drops_the_oldest() {
        let mut history = NavHistory::new();
        for index in 0..(MAX_HISTORY_ENTRIES + 50) {
            history.record_jump(slide(index), slide(index + 1));
        }
        assert_eq!(history.back_depth(), MAX_HISTORY_ENTRIES);
        // The oldest surviving entry is the 50th jump's origin, not the first.
        let mut current = slide(MAX_HISTORY_ENTRIES + 50);
        let mut last = current;
        while let Some(destination) = history.go_back(current) {
            last = destination;
            current = destination;
        }
        assert_eq!(last, slide(50));
    }

    #[test]
    fn clearing_forgets_both_directions() {
        let mut history = NavHistory::new();
        history.record_jump(slide(3), slide(40));
        history.go_back(slide(40));
        history.clear();
        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());
    }
}
