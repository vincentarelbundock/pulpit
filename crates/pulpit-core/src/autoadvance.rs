//! Unattended page turning: the kiosk case.
//!
//! A poster loop, a lobby screen, a self-running deck. The document steps
//! forward on a wall clock rather than on a keypress, and stops when someone
//! asks it to.
//!
//! Two things this deliberately does not know about:
//!
//! - **Which mode is up.** Autoadvance counts in whatever unit the caller
//!   counts in — reader pages or deck slides — because [`step`] is arithmetic
//!   over an index and a count. The application turns the answer into a
//!   [`Place`](crate::Place); nothing here knows there are two viewers, and
//!   nothing here knows a PDF from a comic archive or a scanned book.
//! - **The time.** Like [`Timer`](crate::Timer), it never reads the clock:
//!   `now` is passed in. That is what makes a lid closed for an hour, a page
//!   that took half a second to draw, and a dwell interrupted mid-flight into
//!   ordinary unit tests.

use std::time::{Duration, Instant};

/// The shortest dwell a reader can ask for.
///
/// Below about a second an unattended loop stops being readable and starts
/// being a strobe, and the settled tick could not honour it anyway.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The default dwell: long enough to read a slide's title, short enough that a
/// lobby loop gets round the deck.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// Where a running autoadvance stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not running. The only state a document opens in.
    Stopped,
    /// Running, with the current page's dwell ending at this instant.
    Running { due_at: Instant },
    /// Running, but held: someone took the controls, and the loop is not
    /// going to fight them for it. Only an explicit start resumes.
    Suspended,
}

/// The unattended page-turning clock.
///
/// Held beside [`PresentationState`](crate::PresentationState) rather than
/// inside it: that state machine is the presenter's deck, and autoadvance
/// turns reader pages too.
#[derive(Debug, Clone, PartialEq)]
pub struct Autoadvance {
    interval: Duration,
    state: State,
    /// Wrap to the first page at the end rather than stopping there.
    pub wrap: bool,
}

impl Default for Autoadvance {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
            state: State::Stopped,
            wrap: false,
        }
    }
}

impl Autoadvance {
    pub fn new(interval: Duration, wrap: bool) -> Self {
        Self {
            interval: interval.max(MIN_INTERVAL),
            state: State::Stopped,
            wrap,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Running or held. Both are "the reader turned this on", which is what
    /// the indicator and the screensaver inhibitor care about.
    pub fn is_on(&self) -> bool {
        !matches!(self.state, State::Stopped)
    }

    pub fn is_suspended(&self) -> bool {
        matches!(self.state, State::Suspended)
    }

    /// Change the dwell.
    ///
    /// A running loop re-dwells from `now` rather than keeping a deadline set
    /// under the old interval: the setting was just changed by someone
    /// watching, and the change should be visible on the page they are looking
    /// at rather than one page later.
    pub fn set_interval(&mut self, interval: Duration, now: Instant) {
        self.interval = interval.max(MIN_INTERVAL);
        if let State::Running { .. } = self.state {
            self.state = State::Running {
                due_at: now + self.interval,
            };
        }
    }

    /// Start, or resume from suspension. The page in front of the reader gets
    /// a full dwell before it turns.
    pub fn start(&mut self, now: Instant) {
        self.state = State::Running {
            due_at: now + self.interval,
        };
    }

    pub fn stop(&mut self) {
        self.state = State::Stopped;
    }

    pub fn toggle(&mut self, now: Instant) {
        if self.is_on() {
            self.stop();
        } else {
            self.start(now);
        }
    }

    /// Someone took the controls.
    ///
    /// The loop is held rather than stopped, and stays held until it is
    /// started again: a loop that silently resumed while a reader was still
    /// reading would be the fighting-for-control this exists to avoid.
    /// Suspending an already-suspended or stopped loop changes nothing.
    pub fn suspend(&mut self) {
        if let State::Running { .. } = self.state {
            self.state = State::Suspended;
        }
    }

    /// The dwell has ended and the caller should turn the page.
    pub fn due(&self, now: Instant) -> bool {
        match self.state {
            State::Running { due_at } => now >= due_at,
            State::Stopped | State::Suspended => false,
        }
    }

    /// A page is on the screen: dwell from here.
    ///
    /// Called when the page has actually landed rather than when the turn was
    /// asked for, so a page that took half a second to draw still gets its
    /// full time in front of the room. Ignored while stopped or held, so a
    /// page arriving for any other reason cannot start a loop nobody asked
    /// for.
    pub fn page_landed(&mut self, now: Instant) {
        if let State::Running { .. } = self.state {
            self.state = State::Running {
                due_at: now + self.interval,
            };
        }
    }

    /// The machine came back from suspend, or the clock jumped.
    ///
    /// The dwell restarts rather than settling a backlog: an hour with the lid
    /// shut owes seven hundred page turns, and firing them is both wrong and a
    /// way to spend a minute inside one event-loop turn.
    pub fn clock_jumped(&mut self, now: Instant) {
        self.page_landed(now);
    }
}

/// The next index, or `None` when the loop should stop here.
///
/// The whole of "stop at the end" versus "wrap to the beginning", as
/// arithmetic rather than as a branch in the application. An empty document
/// has nowhere to go, and a document of one page has nowhere to go either —
/// wrapping in place would be a page turn that turns nothing.
pub fn step(at: usize, count: usize, wrap: bool) -> Option<usize> {
    if count <= 1 {
        return None;
    }
    let next = at.saturating_add(1);
    if next < count {
        Some(next)
    } else if wrap {
        Some(0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_stopped_loop_is_never_due() {
        let start = t0();
        let show = Autoadvance::default();
        assert!(!show.due(start + Duration::from_secs(3600)));
    }

    #[test]
    fn the_page_in_hand_gets_a_full_dwell() {
        let start = t0();
        let mut show = Autoadvance::new(Duration::from_secs(5), false);
        show.start(start);
        assert!(!show.due(start + Duration::from_secs(4)));
        assert!(show.due(start + Duration::from_secs(5)));
    }

    #[test]
    fn the_dwell_runs_from_the_page_landing_not_the_turn() {
        let start = t0();
        let mut show = Autoadvance::new(Duration::from_secs(5), false);
        show.start(start);
        // The turn came due, and the page took half a second to draw.
        let landed = start + Duration::from_secs(5) + Duration::from_millis(500);
        show.page_landed(landed);
        assert!(!show.due(landed + Duration::from_secs(4)));
        assert!(show.due(landed + Duration::from_secs(5)));
    }

    #[test]
    fn suspending_holds_until_started_again() {
        let start = t0();
        let mut show = Autoadvance::new(Duration::from_secs(5), false);
        show.start(start);
        show.suspend();
        assert!(show.is_on(), "held is still on: the indicator must say so");
        assert!(!show.due(start + Duration::from_secs(3600)));

        let resumed = start + Duration::from_secs(3600);
        show.start(resumed);
        assert!(!show.due(resumed + Duration::from_secs(4)));
        assert!(show.due(resumed + Duration::from_secs(5)));
    }

    #[test]
    fn a_stopped_loop_cannot_be_suspended_into_running() {
        let mut show = Autoadvance::default();
        show.suspend();
        assert_eq!(show.state(), State::Stopped);
    }

    #[test]
    fn a_page_landing_never_starts_a_stopped_loop() {
        let start = t0();
        let mut show = Autoadvance::default();
        show.page_landed(start);
        assert_eq!(show.state(), State::Stopped);
        assert!(!show.due(start + Duration::from_secs(3600)));
    }

    #[test]
    fn an_hour_with_the_lid_shut_owes_one_turn_at_most() {
        let start = t0();
        let mut show = Autoadvance::new(Duration::from_secs(5), false);
        show.start(start);
        let woke = start + Duration::from_secs(3600);
        assert!(show.due(woke), "the deadline passed while suspended");
        show.clock_jumped(woke);
        assert!(!show.due(woke), "and it is not still owed after the reset");
        assert!(show.due(woke + Duration::from_secs(5)));
    }

    #[test]
    fn changing_the_dwell_re_dwells_the_page_in_hand() {
        let start = t0();
        let mut show = Autoadvance::new(Duration::from_secs(60), false);
        show.start(start);
        let changed = start + Duration::from_secs(10);
        show.set_interval(Duration::from_secs(5), changed);
        assert!(show.due(changed + Duration::from_secs(5)));
    }

    #[test]
    fn the_dwell_has_a_floor() {
        let show = Autoadvance::new(Duration::from_millis(10), false);
        assert_eq!(show.interval(), MIN_INTERVAL);
    }

    #[test]
    fn stopping_at_the_end_and_wrapping_are_the_same_arithmetic() {
        assert_eq!(step(0, 10, false), Some(1));
        assert_eq!(step(8, 10, false), Some(9));
        assert_eq!(step(9, 10, false), None);
        assert_eq!(step(9, 10, true), Some(0));
    }

    #[test]
    fn a_document_with_nowhere_to_go_stays_put() {
        assert_eq!(step(0, 0, true), None);
        assert_eq!(step(0, 1, true), None, "wrapping in place turns nothing");
        assert_eq!(step(0, 1, false), None);
    }
}
