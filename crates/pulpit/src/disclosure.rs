//! A pane that opens and closes: the fact and the animation, as one value.
//!
//! The application has two — the outline rail and the search pane — and they
//! used to be four fields: a boolean somewhere and a clocked animation beside
//! it, paired only by the discipline of always writing both. They came apart
//! exactly where that discipline was hardest to keep. Opening a document reset
//! the reader session, and the session was where the rail's boolean lived, so
//! the session came back believing the rail was open while it was drawn
//! closed; the reading position is recorded from that boolean, so every
//! document was remembered with a rail its reader had never opened, and
//! reopening the file dutifully restored one. Opening a document set the
//! search pane's boolean without touching its animation, for the same reason
//! and with the same shape of consequence.
//!
//! So neither half is reachable on its own. There is one polarity — `open`,
//! the word the settings record already uses — and one direction of reveal,
//! rather than a rail that stored `collapsed` and interpolated backwards to
//! compensate.
//!
//! The animation is here rather than in the pure crates because it is clocked:
//! `pulpit-core` and the reader session are told the time, never read it.

use std::time::Instant;

use crate::platform::Motion;

#[derive(Clone)]
pub struct Disclosure {
    open: bool,
    animation: iced::Animation<bool>,
}

impl Disclosure {
    /// Put away, and drawn that way from the first frame.
    ///
    /// The only constructor: a pane that starts out is a pane somebody has to
    /// have asked for, and nothing has asked yet.
    pub fn closed() -> Self {
        Self {
            open: false,
            animation: iced::Animation::new(false).quick(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// How much of the pane is out: 0.0 shut, 1.0 fully open.
    pub fn reveal(&self, now: Instant) -> f32 {
        self.animation.interpolate(0.0_f32, 1.0_f32, now)
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.animation.is_animating(now)
    }

    /// Somebody asked: it slides, unless this session keeps its motion down.
    pub fn set(&mut self, open: bool, motion: Motion, now: Instant) {
        self.open = open;
        self.animation = if motion.is_reduced() {
            iced::Animation::new(open).quick()
        } else {
            self.animation.clone().go(open, now)
        };
    }

    pub fn toggle(&mut self, motion: Motion, now: Instant) {
        self.set(!self.open, motion, now);
    }

    /// Nobody asked: a remembered position being put back, or a document open
    /// deciding what this file starts with. The pane is simply there, or not,
    /// on arrival — the same immediate form reduced motion uses — rather than
    /// sliding in front of a reader who did not ask for a pane to move.
    pub fn jump(&mut self, open: bool) {
        self.open = open;
        self.animation = iced::Animation::new(open).quick();
    }
}

impl Default for Disclosure {
    fn default() -> Self {
        Self::closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_pane_starts_away_and_drawn_away() {
        let pane = Disclosure::closed();
        assert!(!pane.is_open());
        assert_eq!(pane.reveal(now()), 0.0);
        assert!(!pane.is_animating(now()));
    }

    #[test]
    fn reduced_motion_arrives_at_the_answer_without_the_transition() {
        let mut pane = Disclosure::closed();
        pane.set(true, Motion::Reduced, now());
        assert!(pane.is_open());
        assert_eq!(pane.reveal(now()), 1.0, "there is nothing to animate");
        assert!(!pane.is_animating(now()));
    }

    #[test]
    fn a_restored_pane_is_there_on_arrival_rather_than_sliding_in() {
        let mut pane = Disclosure::closed();
        pane.jump(true);
        assert!(pane.is_open());
        assert_eq!(pane.reveal(now()), 1.0);
        assert!(!pane.is_animating(now()));
    }

    /// The invariant the four fields could not hold: whatever moved the pane,
    /// what it says and what it draws agree at the end of the move.
    #[test]
    fn what_the_pane_says_and_what_it_draws_never_disagree() {
        let mut pane = Disclosure::closed();
        for (open, motion) in [
            (true, Motion::Full),
            (false, Motion::Full),
            (true, Motion::Reduced),
            (false, Motion::Reduced),
        ] {
            pane.set(open, motion, now());
            // Full motion is mid-transition here, so settle it the way the
            // clock would: the target is what the reveal is heading for.
            pane.jump(pane.is_open());
            assert_eq!(pane.is_open(), open);
            assert_eq!(pane.reveal(now()), if open { 1.0 } else { 0.0 });
        }
    }

    #[test]
    fn toggling_alternates() {
        let mut pane = Disclosure::closed();
        pane.toggle(Motion::Reduced, now());
        assert!(pane.is_open());
        pane.toggle(Motion::Reduced, now());
        assert!(!pane.is_open());
    }
}
