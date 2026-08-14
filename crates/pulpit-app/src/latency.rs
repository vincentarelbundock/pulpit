//! What a page turn actually spends its time on.
//!
//! Every performance question asked of this application so far has been
//! answered by reading the code and arguing, and the answers have been wrong
//! about as often as right: a poll nobody counted, a pin that had quietly
//! stopped protecting anything, a stand-in one window never got. The
//! specification's rule is that static findings are hypotheses until
//! measurement attaches numbers to them, and this module is what attaches
//! them.
//!
//! Two kinds of number, because they answer different questions.
//!
//! A **turn** is the user-visible one: the key press, and then the moment
//! each surface finally showed the page it asked for. That is the number a
//! presenter would recognise, and the only one that says whether a change
//! helped. Turns are recorded whole or not at all — a turn interrupted by the
//! next key press is abandoned rather than averaged in, because a presenter
//! holding the arrow key down is not waiting for any of the pages in between.
//!
//! A **stage** is the diagnostic one: how long a named piece of synchronous
//! work took, and at worst. These exist to locate a turn's time, so they
//! count the work that happens on the event loop — where a millisecond is a
//! millisecond the interface is not drawing — and deliberately not the work
//! that happens in a worker process, which is already visible as render
//! latency.
//!
//! Cheap enough to leave on: two clock reads and some arithmetic per event,
//! bounded storage, and no allocation in the steady state.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How many finished turns to keep. Enough that a presenter can step through
/// a handful of slides and then look, not so many that the report is a wall.
const REMEMBERED_TURNS: usize = 16;

/// A surface that has to answer a page turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The projector: the picture the room sees.
    Audience,
    /// The presenter's current-slide panel.
    Presenter,
}

/// What a surface showed, and how good it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The coarse frame, correct page but soft.
    StandIn,
    /// The frame that surface actually wants.
    Exact,
}

/// One page turn in progress.
#[derive(Debug, Clone)]
struct Open {
    slide: usize,
    started: Instant,
    audience_stand_in: Option<Duration>,
    audience_exact: Option<Duration>,
    presenter_stand_in: Option<Duration>,
    presenter_exact: Option<Duration>,
}

/// A turn both surfaces have answered exactly.
#[derive(Debug, Clone, Copy)]
pub struct Turn {
    pub slide: usize,
    pub audience_stand_in: Option<Duration>,
    pub audience_exact: Duration,
    pub presenter_stand_in: Option<Duration>,
    pub presenter_exact: Duration,
}

impl Turn {
    /// The turn as the presenter experienced it: the last surface to answer.
    pub fn settled(&self) -> Duration {
        self.audience_exact.max(self.presenter_exact)
    }

    /// The first correct picture on either surface, exact or not. This is
    /// what "it felt instant" means, as against "it finished".
    pub fn first_picture(&self) -> Duration {
        [
            self.audience_stand_in,
            self.presenter_stand_in,
            Some(self.audience_exact),
            Some(self.presenter_exact),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or_default()
    }
}

/// Synchronous work done on the event loop, by name.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stage {
    pub calls: u64,
    pub total: Duration,
    pub worst: Duration,
}

impl Stage {
    fn record(&mut self, elapsed: Duration) {
        self.calls += 1;
        self.total += elapsed;
        self.worst = self.worst.max(elapsed);
    }

    pub fn mean(&self) -> Duration {
        self.total
            .checked_div(self.calls.max(1) as u32)
            .unwrap_or_default()
    }
}

/// The named stages of a turn's synchronous work.
///
/// One struct rather than a map: the set is fixed, and a fixed set costs no
/// hashing, no allocation and no chance of a typo inventing a stage.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stages {
    /// Planning renders: cache lookups, job submission, and the cancellation
    /// broadcast, which writes to every worker.
    pub plan_renders: Stage,
    /// Following the committed page with overlay sessions.
    pub service_media: Stage,
    /// Draining the renderer, which includes copying large frames out of the
    /// shared region on this thread.
    pub drain_renderer: Stage,
}

/// Frames copied out of the shared memory region, which happens on the event
/// loop. Separate from [`Stages::drain_renderer`] so the copy can be told
/// apart from the rest of the drain.
#[derive(Debug, Clone, Copy, Default)]
pub struct Copies {
    pub frames: u64,
    pub bytes: u64,
}

/// The recorder. One per application.
#[derive(Debug, Default)]
pub struct Latency {
    open: Option<Open>,
    turns: VecDeque<Turn>,
    /// Turns abandoned because the next one began first.
    abandoned: u64,
    stages: Stages,
    copies: Copies,
    /// Render latency: submitted to frame in hand.
    ///
    /// There is deliberately no upload stage beside it. Which pictures a
    /// window has on the GPU is that window's own widget state, reachable
    /// from no `&mut self` the application holds, so `residency` reports a
    /// slow upload to the log where it happens rather than posting a number
    /// here that would have to be smuggled across the view boundary.
    render: Stage,
}

impl Latency {
    /// A page turn has begun. Any turn still open is abandoned: the presenter
    /// has moved on, and timing a page they no longer want tells us nothing.
    pub fn begin_turn(&mut self, slide: usize, now: Instant) {
        if self.open.is_some() {
            self.abandoned += 1;
        }
        self.open = Some(Open {
            slide,
            started: now,
            audience_stand_in: None,
            audience_exact: None,
            presenter_stand_in: None,
            presenter_exact: None,
        });
    }

    /// A surface has shown something for `slide`.
    ///
    /// Ignored unless it is the page the open turn is about, so a neighbour's
    /// prefetch landing cannot be mistaken for an answer. Only the first of
    /// each kind counts: a surface answers a turn once.
    pub fn answered(&mut self, surface: Surface, answer: Answer, slide: usize, now: Instant) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.slide != slide {
            return;
        }
        let elapsed = now.saturating_duration_since(open.started);
        let slot = match (surface, answer) {
            (Surface::Audience, Answer::StandIn) => &mut open.audience_stand_in,
            (Surface::Audience, Answer::Exact) => &mut open.audience_exact,
            (Surface::Presenter, Answer::StandIn) => &mut open.presenter_stand_in,
            (Surface::Presenter, Answer::Exact) => &mut open.presenter_exact,
        };
        if slot.is_none() {
            *slot = Some(elapsed);
        }
        self.close_if_complete();
    }

    /// A turn is finished when both surfaces have their exact frame. Recorded
    /// then rather than on the next turn, so a presenter who stops on a slide
    /// still sees the turn that got them there.
    fn close_if_complete(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let (Some(audience_exact), Some(presenter_exact)) =
            (open.audience_exact, open.presenter_exact)
        else {
            return;
        };
        let turn = Turn {
            slide: open.slide,
            audience_stand_in: open.audience_stand_in,
            audience_exact,
            presenter_stand_in: open.presenter_stand_in,
            presenter_exact,
        };
        self.open = None;
        if self.turns.len() == REMEMBERED_TURNS {
            self.turns.pop_front();
        }
        self.turns.push_back(turn);
    }

    /// Record how long a named piece of synchronous work took.
    ///
    /// Takes the elapsed time rather than a closure to run: the work is
    /// almost always a method on the application, and a closure holding
    /// `&mut App` cannot coexist with the `&mut self` this needs.
    pub fn record_stage(&mut self, which: fn(&mut Stages) -> &mut Stage, elapsed: Duration) {
        which(&mut self.stages).record(elapsed);
    }

    /// Note a frame copied out of the shared region.
    pub fn note_copy(&mut self, bytes: u64) {
        self.copies.frames += 1;
        self.copies.bytes += bytes;
    }

    /// Note how long a render took, from submission to the frame in hand.
    pub fn note_render(&mut self, elapsed: Duration) {
        self.render.record(elapsed);
    }

    pub fn turns(&self) -> &VecDeque<Turn> {
        &self.turns
    }

    pub fn abandoned(&self) -> u64 {
        self.abandoned
    }

    pub fn stages(&self) -> &Stages {
        &self.stages
    }

    pub fn copies(&self) -> &Copies {
        &self.copies
    }

    pub fn render(&self) -> &Stage {
        &self.render
    }

    /// The typical and worst settled turn, over what has been remembered.
    pub fn settled_summary(&self) -> Option<(Duration, Duration)> {
        if self.turns.is_empty() {
            return None;
        }
        let mut total = Duration::ZERO;
        let mut worst = Duration::ZERO;
        for turn in &self.turns {
            total += turn.settled();
            worst = worst.max(turn.settled());
        }
        Some((total / self.turns.len() as u32, worst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_turn_is_the_last_surface_to_answer() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.begin_turn(4, base);
        latency.answered(Surface::Audience, Answer::Exact, 4, at(base, 30));
        latency.answered(Surface::Presenter, Answer::Exact, 4, at(base, 80));

        let turn = latency.turns().back().expect("a finished turn");
        assert_eq!(turn.settled(), Duration::from_millis(80));
        assert_eq!(turn.first_picture(), Duration::from_millis(30));
    }

    #[test]
    fn a_stand_in_is_the_first_picture_without_ending_the_turn() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.begin_turn(4, base);
        latency.answered(Surface::Presenter, Answer::StandIn, 4, at(base, 12));
        assert!(latency.turns().is_empty(), "soft is not settled");

        latency.answered(Surface::Audience, Answer::Exact, 4, at(base, 40));
        latency.answered(Surface::Presenter, Answer::Exact, 4, at(base, 45));
        let turn = latency.turns().back().expect("a finished turn");
        assert_eq!(turn.first_picture(), Duration::from_millis(12));
        assert_eq!(turn.settled(), Duration::from_millis(45));
    }

    /// A neighbour's prefetch landing is not an answer to this turn. Counting
    /// it would report a turn as settled before its own page ever appeared.
    #[test]
    fn another_page_never_answers_this_turn() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.begin_turn(4, base);
        latency.answered(Surface::Audience, Answer::Exact, 5, at(base, 10));
        latency.answered(Surface::Presenter, Answer::Exact, 5, at(base, 10));
        assert!(latency.turns().is_empty(), "page 5 did not answer for 4");
    }

    /// Holding the arrow key down must not fill the record with turns nobody
    /// waited for — and must not report the last one as having taken the
    /// whole sweep.
    #[test]
    fn an_interrupted_turn_is_abandoned_not_averaged() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.begin_turn(4, base);
        latency.begin_turn(5, at(base, 20));
        latency.begin_turn(6, at(base, 40));
        assert_eq!(latency.abandoned(), 2);
        assert!(latency.turns().is_empty());

        latency.answered(Surface::Audience, Answer::Exact, 6, at(base, 60));
        latency.answered(Surface::Presenter, Answer::Exact, 6, at(base, 70));
        let turn = latency.turns().back().expect("the turn that landed");
        assert_eq!(turn.slide, 6);
        // Timed from the key that asked for page 6, not from the first key.
        assert_eq!(turn.settled(), Duration::from_millis(30));
    }

    #[test]
    fn a_surface_answers_once() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.begin_turn(1, base);
        latency.answered(Surface::Audience, Answer::Exact, 1, at(base, 10));
        latency.answered(Surface::Audience, Answer::Exact, 1, at(base, 90));
        latency.answered(Surface::Presenter, Answer::Exact, 1, at(base, 20));
        let turn = latency.turns().back().expect("a turn");
        assert_eq!(turn.audience_exact, Duration::from_millis(10));
    }

    #[test]
    fn only_the_last_turns_are_kept() {
        let base = Instant::now();
        let mut latency = Latency::default();
        for slide in 0..REMEMBERED_TURNS + 5 {
            latency.begin_turn(slide, base);
            latency.answered(Surface::Audience, Answer::Exact, slide, at(base, 5));
            latency.answered(Surface::Presenter, Answer::Exact, slide, at(base, 5));
        }
        assert_eq!(latency.turns().len(), REMEMBERED_TURNS);
        assert_eq!(latency.turns().back().unwrap().slide, REMEMBERED_TURNS + 4);
    }

    #[test]
    fn a_stage_reports_its_worst_not_only_its_mean() {
        let mut stage = Stage::default();
        stage.record(Duration::from_millis(1));
        stage.record(Duration::from_millis(9));
        assert_eq!(stage.mean(), Duration::from_millis(5));
        assert_eq!(stage.worst, Duration::from_millis(9));
    }

    #[test]
    fn nothing_is_summarised_before_anything_is_measured() {
        assert!(Latency::default().settled_summary().is_none());
    }
}
