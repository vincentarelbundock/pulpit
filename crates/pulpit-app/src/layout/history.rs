//! Undo and redo for the editor session.
//!
//! Every editing action is covered — splits, deletes, moves, resizes, widget
//! placement and removal, property edits and renames — because the history
//! stores whole layout snapshots rather than trying to invert each operation.
//! A layout is a few kilobytes; correctness is worth more than the bytes.
//!
//! The history is unbounded within a session, is *not* cleared by saving, and
//! disappears when the editor closes.

/// A snapshot stack with a redo branch.
#[derive(Debug, Clone)]
pub struct History<T> {
    past: Vec<T>,
    present: T,
    future: Vec<T>,
    /// The state as it was last saved, for "unsaved changes" and revert.
    saved: Option<T>,
}

impl<T: Clone + PartialEq> History<T> {
    pub fn new(initial: T) -> Self {
        Self {
            past: Vec::new(),
            present: initial.clone(),
            future: Vec::new(),
            saved: Some(initial),
        }
    }

    /// Start a history whose initial state has never been saved.
    pub fn unsaved(initial: T) -> Self {
        Self {
            past: Vec::new(),
            present: initial,
            future: Vec::new(),
            saved: None,
        }
    }

    pub fn current(&self) -> &T {
        &self.present
    }

    pub fn current_mut(&mut self) -> &mut T {
        &mut self.present
    }

    /// Record an edit. A no-op edit is not recorded, so undo never appears to
    /// do nothing.
    pub fn commit(&mut self, next: T) {
        if next == self.present {
            return;
        }
        self.past.push(std::mem::replace(&mut self.present, next));
        self.future.clear();
    }

    /// Edit in place and record the result if it changed anything.
    pub fn edit<R>(&mut self, action: impl FnOnce(&mut T) -> R) -> R {
        let before = self.present.clone();
        let result = action(&mut self.present);
        if self.present != before {
            self.past.push(before);
            self.future.clear();
        }
        result
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.past.pop() else {
            return false;
        };
        self.future
            .push(std::mem::replace(&mut self.present, previous));
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.future.pop() else {
            return false;
        };
        self.past.push(std::mem::replace(&mut self.present, next));
        true
    }

    /// Has this ever been saved? A layout with no saved version has nothing
    /// to revert *to*, which is a different situation from having no changes.
    pub fn has_saved_version(&self) -> bool {
        self.saved.is_some()
    }

    /// Mark the current state as saved. Deliberately does **not** clear the
    /// history: undoing past a save is allowed.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.present.clone());
    }

    pub fn has_unsaved_changes(&self) -> bool {
        match &self.saved {
            Some(saved) => saved != &self.present,
            None => true,
        }
    }

    pub fn saved_state(&self) -> Option<&T> {
        self.saved.as_ref()
    }

    /// Revert to the last saved version, as one undoable step.
    pub fn revert_to_saved(&mut self) -> bool {
        let Some(saved) = self.saved.clone() else {
            return false;
        };
        if saved == self.present {
            return false;
        }
        self.commit(saved);
        true
    }

    pub fn depth(&self) -> usize {
        self.past.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_walk_the_stack() {
        let mut history = History::new(1);
        history.commit(2);
        history.commit(3);
        assert_eq!(*history.current(), 3);

        assert!(history.undo());
        assert_eq!(*history.current(), 2);
        assert!(history.undo());
        assert_eq!(*history.current(), 1);
        assert!(!history.undo(), "nothing left to undo");

        assert!(history.redo());
        assert_eq!(*history.current(), 2);
        assert!(history.redo());
        assert_eq!(*history.current(), 3);
        assert!(!history.redo());
    }

    #[test]
    fn a_new_edit_discards_the_redo_branch() {
        let mut history = History::new(1);
        history.commit(2);
        history.undo();
        assert!(history.can_redo());
        history.commit(99);
        assert!(!history.can_redo());
        assert_eq!(*history.current(), 99);
    }

    #[test]
    fn no_op_edits_are_not_recorded() {
        let mut history = History::new(1);
        history.commit(1);
        assert!(!history.can_undo(), "undo would appear to do nothing");

        history.edit(|value| *value = 1);
        assert!(!history.can_undo());

        history.edit(|value| *value = 2);
        assert!(history.can_undo());
    }

    #[test]
    fn saving_does_not_clear_the_history() {
        let mut history = History::new(1);
        history.commit(2);
        history.mark_saved();
        assert!(!history.has_unsaved_changes());
        assert!(history.can_undo(), "undo past a save is allowed");

        history.undo();
        assert!(
            history.has_unsaved_changes(),
            "now it differs from the saved copy"
        );
    }

    #[test]
    fn reverting_to_saved_is_itself_undoable() {
        let mut history = History::new(1);
        history.commit(2);
        history.mark_saved();
        history.commit(3);
        assert!(history.has_unsaved_changes());

        assert!(history.revert_to_saved());
        assert_eq!(*history.current(), 2);
        assert!(!history.has_unsaved_changes());

        history.undo();
        assert_eq!(*history.current(), 3, "the discarded work is recoverable");
    }

    #[test]
    fn an_unsaved_history_starts_dirty() {
        let mut history = History::unsaved("draft");
        assert!(history.has_unsaved_changes());
        history.mark_saved();
        assert!(!history.has_unsaved_changes());
    }

    #[test]
    fn the_history_is_unbounded_within_a_session() {
        let mut history = History::new(0);
        for value in 1..5_000 {
            history.commit(value);
        }
        assert_eq!(history.depth(), 4_999);
        for _ in 0..4_999 {
            assert!(history.undo());
        }
        assert_eq!(*history.current(), 0);
    }
}
