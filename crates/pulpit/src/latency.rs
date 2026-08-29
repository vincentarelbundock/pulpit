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
    /// Complete application message handling, including the state-to-view
    /// snapshot synchronisation that follows dispatch.
    pub update: Stage,
    /// Planning renders: cache lookups, job submission, and the cancellation
    /// broadcast, which writes to every worker.
    pub plan_renders: Stage,
    /// Following the committed page with overlay sessions.
    pub service_media: Stage,
    /// Draining the renderer, which includes copying large frames out of the
    /// shared region on this thread.
    pub drain_renderer: Stage,
    /// Applying ordered answers from the document worker.
    pub drain_reader: Stage,
    /// Coalescing and copying complete frames from media workers.
    pub drain_media: Stage,
}

/// Frames copied out of the shared memory region, which happens on the event
/// loop. Separate from [`Stages::drain_renderer`] so the copy can be told
/// apart from the rest of the drain.
#[derive(Debug, Clone, Copy, Default)]
pub struct Copies {
    pub frames: u64,
    pub bytes: u64,
}

/// Where `residency` reports the uploads it blocks on.
///
/// Shared by handle because the two ends are on opposite sides of the view
/// boundary: a window's residency is widget state reached through `&App`,
/// and the recorder is `&mut` only inside `update`. A `Cell` between them is
/// the whole mechanism — one thread, no locking, and the widget never sees
/// the recorder.
///
/// This was left out at first on the grounds that the application cannot see
/// what a window has uploaded. That was true and beside the point: it does
/// not need to know *which* pictures are resident, only how long it was
/// stopped putting them there. Leaving it out meant the one part of a page
/// turn that blocks the event loop outside `update` was the one part never
/// counted — while the report said, on the strength of the parts that were,
/// that the event loop was innocent.
#[derive(Debug, Clone, Default)]
pub struct UploadMeter(std::rc::Rc<std::cell::Cell<Stage>>);

impl UploadMeter {
    /// Note a blocking upload. Takes `&self`: the caller is a widget holding
    /// nothing mutable.
    pub fn record(&self, elapsed: Duration) {
        let mut stage = self.0.get();
        stage.record(elapsed);
        self.0.set(stage);
    }

    pub fn get(&self) -> Stage {
        self.0.get()
    }
}

/// The recorder. One per application.
#[derive(Debug, Default)]
pub struct Latency {
    /// Whether there is no audience surface to wait for.
    ///
    /// A turn is finished when every surface that exists has answered, and
    /// on a laptop with no projector attached only one does. Requiring the
    /// audience unconditionally — which is what this once did — left every
    /// turn of a single-screen session open forever, so the report said
    /// "nothing measured yet" no matter how many pages were turned.
    ///
    /// Phrased as an absence so that `Default` — which every test and the
    /// application both start from — still means "wait for both".
    audience_absent: bool,
    open: Option<Open>,
    turns: VecDeque<Turn>,
    /// Turns abandoned because the next one began first.
    abandoned: u64,
    stages: Stages,
    copies: Copies,
    /// Render latency for a frame a window is waiting for: submitted to
    /// frame in hand, so it includes the wait in the queue.
    ///
    /// There is deliberately no upload stage beside it. Which pictures a
    /// window has on the GPU is that window's own widget state, reachable
    /// from no `&mut self` the application holds, so `residency` reports a
    /// slow upload to the log where it happens rather than posting a number
    /// here that would have to be smuggled across the view boundary.
    render: Stage,
    /// Of the live renders, the ones for the page a window is showing right
    /// now — the only ones a presenter is ever actually waiting on.
    ///
    /// The rest are prefetch: the neighbours, and the panels two pages either
    /// side. They are "live" in the sense that they are not deck warming, and
    /// they are not live in the sense that matters, because nothing on screen
    /// is missing while they run. Reported together, as they were, a hundred
    /// speculative renders queued behind each other set the typical figure
    /// and the handful anyone waited for disappeared into it.
    on_screen: Stage,
    /// Renders for a page one step away, wanted soon, waited for by nobody.
    prefetch: Stage,
    /// The wait in a worker's own inbox, split the same way.
    ///
    /// The two totals above cannot settle the question they raise. A visible
    /// page that finishes later than a speculative one has either waited
    /// behind less urgent work — which would be a priority inversion, and a
    /// bug — or simply been a more expensive picture, which is not. The
    /// inbox wait separates them: it is the part of a job's life spent held
    /// by a worker that was rendering something else, and it is the only part
    /// an ordering mistake could lengthen.
    on_screen_inbox: Stage,
    prefetch_inbox: Stage,
    /// The rasteriser's own time for each of the two tiers a page is drawn
    /// at, so the cost of the coarse-then-refined arrangement can be weighed
    /// against what it buys.
    ///
    /// Reported together — which is how it was — the two are one number that
    /// answers neither question anyone asks of them: whether a preview is
    /// cheap enough to be worth rendering, and whether a full page is slow
    /// enough to need one. A fifth of the pixels is a fifth of the work only
    /// if the rasteriser is pixel-bound, and a page of text is not obviously
    /// that.
    coarse_rendered: Stage,
    refined_rendered: Stage,
    /// The part of `render` a worker was holding the job. What is left is the
    /// wait in this process's queue.
    render_worked: Stage,
    /// The same split for warming.
    warming_worked: Stage,
    /// The rasteriser's own time, as the worker measured it. `worked` minus
    /// this is the wait in the worker's inbox — a queue the supervisor
    /// cannot see, and the last place a page turn's time could be hiding.
    render_rendered: Stage,
    warming_rendered: Stage,
    /// Render latency for deck warming, kept apart from the above.
    ///
    /// A deck of seven hundred pages warms seven hundred thumbnails, each
    /// queued behind all the others, so most of them wait seconds by design
    /// and nobody is waiting on any of them. Averaged together with the live
    /// frames — which is how this was first reported — the handful of numbers
    /// that describe a page turn vanished into hundreds that describe idle
    /// background work, and the report said renders take half a second when
    /// no page turn had waited anything like that.
    warming: Stage,
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

    /// Whether a projector is attached, and so whether a turn has to wait for
    /// it. Set from the application whenever the audience window comes or
    /// goes; a turn already open is judged by the answer in force when its
    /// last surface reports.
    pub fn expect_audience(&mut self, expected: bool) {
        self.audience_absent = !expected;
        // A projector unplugged mid-turn leaves a turn waiting for a surface
        // that no longer exists. It can finish now.
        if !expected {
            self.close_if_complete();
        }
    }

    /// A turn is finished when every surface that exists has its exact frame.
    /// Recorded then rather than on the next turn, so a presenter who stops on
    /// a slide still sees the turn that got them there.
    fn close_if_complete(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(presenter_exact) = open.presenter_exact else {
            return;
        };
        // With no projector the audience columns are reported as zero rather
        // than omitted: a report whose shape changed with the hardware would
        // be one nobody could compare across sessions.
        let audience_exact = match (!self.audience_absent, open.audience_exact) {
            (true, Some(exact)) => exact,
            (true, None) => return,
            (false, _) => Duration::ZERO,
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

    /// Note how long a render took, from submission to the frame in hand,
    /// and how much of that a worker was holding it.
    ///
    /// `warming` separates work nobody is waiting for from work a window is.
    /// The difference between the two figures is the wait in this process's
    /// own queue, which is the only part anything here can do something
    /// about.
    pub fn note_render(
        &mut self,
        elapsed: Duration,
        worked: Duration,
        rendered: Duration,
        warming: bool,
        on_screen: bool,
        refined: bool,
    ) {
        if warming {
            self.warming.record(elapsed);
            self.warming_worked.record(worked);
            self.warming_rendered.record(rendered);
            return;
        }
        // Warming is excluded, and the first attempt at this got that wrong.
        // A thumbnail looks like coarse work and is not: it is submitted at
        // `Refined` quality and a fraction of the width, so counting it here
        // put six hundred one-millisecond thumbnails in the same bucket as
        // the full pages and reported a full page as costing a millisecond.
        // The tiers being compared are the two a *reader's* page is drawn at,
        // and warming is neither of them.
        if refined {
            self.refined_rendered.record(rendered);
        } else {
            self.coarse_rendered.record(rendered);
        }
        self.render.record(elapsed);
        self.render_worked.record(worked);
        self.render_rendered.record(rendered);
        let inbox = worked.saturating_sub(rendered);
        if on_screen {
            self.on_screen.record(elapsed);
            self.on_screen_inbox.record(inbox);
        } else {
            self.prefetch.record(elapsed);
            self.prefetch_inbox.record(inbox);
        }
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

    pub fn warming(&self) -> &Stage {
        &self.warming
    }

    pub fn render_worked(&self) -> &Stage {
        &self.render_worked
    }

    pub fn render_rendered(&self) -> &Stage {
        &self.render_rendered
    }

    pub fn warming_rendered(&self) -> &Stage {
        &self.warming_rendered
    }

    pub fn on_screen(&self) -> &Stage {
        &self.on_screen
    }

    pub fn prefetch(&self) -> &Stage {
        &self.prefetch
    }

    pub fn on_screen_inbox(&self) -> &Stage {
        &self.on_screen_inbox
    }

    pub fn prefetch_inbox(&self) -> &Stage {
        &self.prefetch_inbox
    }

    pub fn coarse_rendered(&self) -> &Stage {
        &self.coarse_rendered
    }

    pub fn refined_rendered(&self) -> &Stage {
        &self.refined_rendered
    }

    pub fn warming_worked(&self) -> &Stage {
        &self.warming_worked
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

    /// A laptop with no projector has one surface, and a turn that waits for
    /// a second one never finishes. Every reading session was measured this
    /// way — which is to say not at all — until the turn stopped requiring an
    /// audience that was not there.
    #[test]
    fn one_surface_settles_a_turn_when_there_is_no_projector() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.expect_audience(false);
        latency.begin_turn(3, base);
        latency.answered(Surface::Presenter, Answer::Exact, 3, at(base, 25));
        let turn = latency.turns().back().expect("the turn settled alone");
        assert_eq!(turn.slide, 3);
        assert_eq!(turn.settled(), Duration::from_millis(25));
        assert_eq!(turn.audience_exact, Duration::ZERO);
    }

    /// And with a projector attached it still takes both, which is the case
    /// the single-surface rule must not have loosened.
    #[test]
    fn a_projector_is_still_waited_for_when_there_is_one() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.expect_audience(true);
        latency.begin_turn(3, base);
        latency.answered(Surface::Presenter, Answer::Exact, 3, at(base, 25));
        assert!(latency.turns().is_empty(), "the projector has not answered");
        latency.answered(Surface::Audience, Answer::Exact, 3, at(base, 40));
        let turn = latency.turns().back().expect("both surfaces answered");
        assert_eq!(turn.settled(), Duration::from_millis(40));
    }

    /// Unplugging mid-turn must release the turn that was waiting on the
    /// screen that went away, rather than stranding it until the next one
    /// abandons it.
    #[test]
    fn unplugging_mid_turn_releases_the_waiting_turn() {
        let base = Instant::now();
        let mut latency = Latency::default();
        latency.expect_audience(true);
        latency.begin_turn(7, base);
        latency.answered(Surface::Presenter, Answer::Exact, 7, at(base, 15));
        assert!(latency.turns().is_empty());
        latency.expect_audience(false);
        let turn = latency.turns().back().expect("released by the unplug");
        assert_eq!(turn.slide, 7);
        assert_eq!(latency.abandoned(), 0);
    }

    /// The inbox wait is what an ordering mistake would lengthen, so it is
    /// kept apart from the rasterising a bigger picture costs. A visible page
    /// that took longer only because it was larger must not look like one
    /// that was made to wait.
    #[test]
    fn the_inbox_wait_is_split_from_the_rasterising() {
        let mut latency = Latency::default();
        // On screen: slow to draw, never held up.
        latency.note_render(
            Duration::from_millis(40),
            Duration::from_millis(38),
            Duration::from_millis(38),
            false,
            true,
            true,
        );
        // Speculative: quick to draw, sat in an inbox.
        latency.note_render(
            Duration::from_millis(20),
            Duration::from_millis(18),
            Duration::from_millis(3),
            false,
            false,
            false,
        );
        assert_eq!(latency.on_screen().mean(), Duration::from_millis(40));
        assert_eq!(latency.on_screen_inbox().mean(), Duration::ZERO);
        assert_eq!(latency.prefetch().mean(), Duration::from_millis(20));
        assert_eq!(
            latency.prefetch_inbox().mean(),
            Duration::from_millis(15),
            "the wait, not the drawing"
        );
        // And the two tiers are told apart by what they cost to draw, which
        // is the question "is the preview tier worth its complication?" in
        // its measurable form.
        assert_eq!(latency.refined_rendered().mean(), Duration::from_millis(38));
        assert_eq!(latency.coarse_rendered().mean(), Duration::from_millis(3));
    }

    /// A thumbnail is submitted at `Refined` quality and a fraction of the
    /// width, so counting warming towards a tier puts hundreds of tiny
    /// pictures beside the full pages and reports a full page as costing
    /// what a thumbnail costs. The tiers are the two a reader's page is drawn
    /// at; warming is neither.
    #[test]
    fn warming_belongs_to_neither_tier() {
        let mut latency = Latency::default();
        latency.note_render(
            Duration::from_millis(50),
            Duration::from_millis(30),
            Duration::from_millis(1),
            true,
            false,
            true,
        );
        assert_eq!(
            latency.refined_rendered().calls,
            0,
            "a thumbnail is not a page"
        );
        assert_eq!(latency.coarse_rendered().calls, 0);
        assert_eq!(latency.warming().calls, 1, "counted, as warming");
        assert_eq!(latency.on_screen().calls, 0);
        assert_eq!(latency.prefetch().calls, 0);
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

    /// Both windows write to one meter through their own clones, and the
    /// application reads the total. If a clone kept its own counter, the
    /// audience window's uploads — the large ones — would be reported by
    /// nobody.
    #[test]
    fn every_window_reports_to_the_same_meter() {
        let meter = UploadMeter::default();
        let presenter = meter.clone();
        let audience = meter.clone();
        presenter.record(Duration::from_millis(3));
        audience.record(Duration::from_millis(21));

        let stage = meter.get();
        assert_eq!(stage.calls, 2);
        assert_eq!(stage.worst, Duration::from_millis(21));
        assert_eq!(stage.mean(), Duration::from_millis(12));
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
