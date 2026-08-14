#![allow(dead_code)] // configuration vocabulary, kept for when it is offered again
//! Timing: the elapsed timer, the wall clock, and the pair of them.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimerOptions {
    /// Minutes remaining at which the display turns to the warning colour.
    pub warning_minutes: u32,
}

impl Default for TimerOptions {
    fn default() -> Self {
        Self { warning_minutes: 5 }
    }
}

/// Beyond two hours a "warning" is not a warning.
pub const MAX_WARNING_MINUTES: u32 = 120;

impl TimerOptions {
    pub fn sanitise(&mut self) {
        self.warning_minutes = self.warning_minutes.min(MAX_WARNING_MINUTES);
    }

    /// What the timer reads, and how it should feel.
    ///
    /// Pure arithmetic, kept out of the drawing code so the boundaries —
    /// exactly on target, one second over, counting down past zero — can be
    /// asserted directly.
    ///
    /// `count_down` is passed in rather than configured here: which way the
    /// timer runs belongs to the talk, alongside its length and its cues, not
    /// to a layout that will be reused next month.
    pub fn reading(
        &self,
        elapsed: Duration,
        target: Option<Duration>,
        count_down: bool,
    ) -> TimerReading {
        let elapsed = elapsed.as_secs() as i64;
        let target_seconds = target.map(|target| target.as_secs() as i64);

        let (value, overtime) = match (count_down, target_seconds) {
            (true, Some(target)) => {
                let remaining = target - elapsed;
                (remaining.abs(), remaining < 0)
            }
            (_, Some(target)) => (elapsed, elapsed > target),
            (_, None) => (elapsed, false),
        };

        let warning = !overtime
            && target_seconds.is_some_and(|target| {
                let remaining = target - elapsed;
                remaining >= 0 && remaining <= self.warning_minutes as i64 * 60
            });

        TimerReading {
            seconds: value,
            overtime,
            warning,
            label: match (count_down, overtime) {
                (_, true) => "OVERTIME",
                (true, false) => "REMAINING",
                (false, false) => "ELAPSED",
            },
        }
    }
}

/// What the timer says right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerReading {
    pub seconds: i64,
    pub overtime: bool,
    pub warning: bool,
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClockOptions {
    pub twenty_four_hour: bool,
    pub show_seconds: bool,
    /// Whether the clock offers the alarm affordance and names the next one.
    ///
    /// A layout preference, not the alarms themselves: the times belong to
    /// the talk and live in [`AlarmControls`], the way ink lives outside the
    /// layout that configures the annotation palette.
    pub show_alarms: bool,
}

impl Default for ClockOptions {
    fn default() -> Self {
        Self {
            twenty_four_hour: true,
            show_seconds: false,
            show_alarms: true,
        }
    }
}

impl ClockOptions {
    /// The wall clock as it should read, from seconds since midnight.
    pub fn format(&self, seconds_of_day: u32) -> String {
        let seconds = seconds_of_day % 86_400;
        let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
        let (hours, suffix) = if self.twenty_four_hour {
            (hours, "")
        } else {
            let suffix = if hours >= 12 { " PM" } else { " AM" };
            let twelve = match hours % 12 {
                0 => 12,
                other => other,
            };
            (twelve, suffix)
        };
        if self.show_seconds {
            format!("{hours:02}:{minutes:02}:{secs:02}{suffix}")
        } else {
            format!("{hours:02}:{minutes:02}{suffix}")
        }
    }

    /// A time of day for the alarm list, always to the minute.
    pub fn format_alarm(&self, seconds_of_day: u32) -> String {
        ClockOptions {
            show_seconds: false,
            ..*self
        }
        .format(seconds_of_day)
    }
}

/// A wall-clock cue.
///
/// Absolute rather than relative because that is what survives starting late:
/// a talk handed the room twelve minutes behind schedule still hands off at
/// half past. Relative entry (`+10m`) is an input convenience that resolves
/// to one of these immediately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    /// Seconds since local midnight, the same basis as
    /// [`crate::widgets::context::TimingData::seconds_of_day`].
    pub at: u32,
    /// What the cue is for. Short: it is read at a glance, mid-sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Alarm {
    pub fn new(at: u32, label: Option<String>) -> Self {
        Self {
            at: at % 86_400,
            label,
        }
    }
}

/// More cues than this is a schedule, not a set of reminders, and more than
/// a presenter can hold in mind while talking.
pub const MAX_ALARMS: usize = 8;

/// A cue that fired longer ago than this is not announced on resume: an alarm
/// delivered urgently and late is worse information than no alarm at all.
pub const STALE_AFTER_SECONDS: u32 = 90;

/// How long a snoozed cue waits before asking again, when nobody has said.
pub const DEFAULT_SNOOZE_MINUTES: u32 = 5;
/// The same, in seconds, for the tests and for anything still assuming it.
pub const SNOOZE_SECONDS: u32 = DEFAULT_SNOOZE_MINUTES * 60;
/// The widest a snooze may be set. Longer than this is not a snooze, it is a
/// different alarm, and the popup has a field for those.
pub const MAX_SNOOZE_MINUTES: u32 = 60;

/// One full cycle of the alert flash.
///
/// Slow on purpose. A fast strobe over a slide is both harder to read past
/// and a genuine hazard for photosensitive people; what is wanted is
/// something the eye catches at the edge of vision, not something that
/// demands the room's attention.
pub const FLASH_PERIOD: Duration = Duration::from_millis(4_000);
/// How much of each cycle the tint is visible at all.
pub const FLASH_VISIBLE: Duration = Duration::from_millis(1_200);
/// The tint at its strongest. Low enough to read a slide through.
pub const FLASH_PEAK: f32 = 0.22;
/// The steady tint used instead of a pulse when motion is to be kept down.
pub const FLASH_STEADY: f32 = 0.10;

/// How strong the full-screen tint is, `since` the cue started ringing.
///
/// Pure so the shape of the pulse can be asserted rather than watched: it
/// rises and falls smoothly, rests for most of each cycle, and repeats until
/// the cue is answered.
pub fn flash_alpha(since: Duration, reduce_motion: bool) -> f32 {
    // Someone who has asked the desktop for less motion gets a steady wash
    // rather than a pulse: still unmissable, never moving.
    if reduce_motion {
        return FLASH_STEADY;
    }
    let phase = Duration::from_nanos((since.as_nanos() % FLASH_PERIOD.as_nanos()) as u64);
    if phase >= FLASH_VISIBLE {
        return 0.0;
    }
    // A raised cosine, so the tint arrives and leaves without an edge.
    let progress = phase.as_secs_f32() / FLASH_VISIBLE.as_secs_f32();
    let eased = (1.0 - (progress * std::f32::consts::TAU).cos()) / 2.0;
    eased * FLASH_PEAK
}

/// A time being typed as two fields with a colon between them.
///
/// Hours and minutes on the clock's alarms, minutes and seconds on the timer's
/// length: the same gesture for both, because they are the same gesture. Two
/// fields rather than one run of four digits because the colon is then real
/// punctuation between two boxes instead of a character that appears under the
/// typing and cannot be backspaced over, and because each half says plainly how
/// much it holds.
///
/// Held as typed. A half-finished "1" is not yet a time, and rewriting it into
/// one under the presenter's fingers is how a field fights back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeEntry {
    pub left: String,
    pub right: String,
}

impl TimeEntry {
    /// Keep what a person typed and drop what they cannot have meant: digits
    /// only, two at most, because each half of a time is two digits.
    fn digits(typed: &str) -> String {
        typed.chars().filter(char::is_ascii_digit).take(2).collect()
    }

    /// Take a keystroke in the left field. `true` means that field is now full
    /// and the typing belongs in the right one — which is what lets a presenter
    /// type "1420" straight through without reaching for Tab.
    #[must_use]
    pub fn type_left(&mut self, typed: &str) -> bool {
        let typed: String = typed.chars().filter(char::is_ascii_digit).collect();
        if typed.len() > 2 {
            // Typed past the end of a full field: the overflow is the start of
            // the next one rather than a keystroke thrown away.
            self.left = typed[..2].to_owned();
            self.right = Self::digits(&typed[2..]);
            return true;
        }
        let full = typed.len() == 2;
        self.left = typed;
        full
    }

    pub fn type_right(&mut self, typed: &str) {
        self.right = Self::digits(typed);
    }

    /// The two halves as numbers, or `None` for a pair that is not a time at
    /// all. An empty half is zero — "9" in the hours is nine o'clock — but two
    /// empty halves are nothing typed rather than midnight.
    pub fn values(&self) -> Option<(u32, u32)> {
        if self.left.is_empty() && self.right.is_empty() {
            return None;
        }
        let read = |half: &str| -> Option<u32> {
            if half.is_empty() {
                Some(0)
            } else {
                half.parse().ok()
            }
        };
        Some((read(&self.left)?, read(&self.right)?))
    }

    /// Put a time in the fields, as the digits that would type it.
    pub fn set(&mut self, left: u32, right: u32) {
        self.left = format!("{left:02}");
        self.right = format!("{right:02}");
    }

    pub fn clear(&mut self) {
        self.left.clear();
        self.right.clear();
    }
}

/// The longest talk the menu will dial. Past this a "timer" is a clock.
pub const MAX_TARGET_MINUTES: u32 = 480;

/// The same bound, in the seconds the target is actually kept in.
pub const MAX_TARGET_SECONDS: u32 = MAX_TARGET_MINUTES * 60;

/// How the timer is running, and whether the menu that says so is open.
///
/// Beside [`AlarmControls`] and for the same reason: counting down to
/// twenty-five minutes is a fact about today's talk, not about the layout it
/// is being given in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerControls {
    /// Count down towards the target, rather than up from zero.
    pub count_down: bool,
    /// How long the talk is meant to be, in seconds. `None` is an open-ended
    /// talk, which can only be counted up.
    ///
    /// Seconds rather than minutes because a lightning talk is five minutes
    /// *thirty*, and a target that could only be dialled in whole minutes made
    /// the presenter round their own talk to fit the control.
    pub target_seconds: Option<u32>,
    /// The length being typed into the menu, minutes then seconds.
    pub entry: TimeEntry,
    /// Whether the menu that sets those two is showing.
    pub open: bool,
    /// Running past the target has been acknowledged, so the line under the
    /// timer stops offering to do anything about it. Reset by anything that
    /// changes what "the end" means.
    pub overtime_dismissed: bool,
    /// How long "snooze" pushes the target out by, in minutes. The same number
    /// the alarms use; both popups set it.
    pub snooze_minutes: u32,
    /// When the talk went past its target, which is what the alert pulse is
    /// timed from. `None` while there is still time left, or once the overrun
    /// has been acknowledged.
    pub overtime_since: Option<std::time::Instant>,
}

impl Default for TimerControls {
    fn default() -> Self {
        Self {
            count_down: false,
            target_seconds: None,
            entry: TimeEntry::default(),
            open: false,
            overtime_dismissed: false,
            snooze_minutes: DEFAULT_SNOOZE_MINUTES,
            overtime_since: None,
        }
    }
}

impl TimerControls {
    pub fn new(target_seconds: Option<u32>, count_down: bool) -> Self {
        let mut controls = Self {
            count_down,
            target_seconds,
            ..Self::default()
        };
        controls.sanitise();
        controls.sync_entry();
        controls
    }

    /// Push the end of the talk out by a snooze, and take the acknowledgement
    /// with it: the timer is no longer over, so there is nothing to answer.
    ///
    /// Open-ended talks cannot be snoozed — there is no target to move — and
    /// the line that offers it is only drawn when there is one.
    pub fn snooze(&mut self) {
        if let Some(seconds) = self.target_seconds {
            self.target_seconds = Some(seconds + self.snooze_minutes * 60);
            self.overtime_dismissed = false;
            self.overtime_since = None;
            self.sanitise();
            self.sync_entry();
        }
    }

    /// Stop offering to do anything about the overrun, and stop the pulse. The
    /// reading stays red: the presenter asked to be left alone about it, not to
    /// be lied to.
    pub fn dismiss_overtime(&mut self) {
        self.overtime_dismissed = true;
        self.overtime_since = None;
    }

    /// Notice the moment the talk runs past its target, and let go of it when
    /// it no longer has.
    ///
    /// Kept as a moment rather than recomputed per frame so the pulse is timed
    /// from when the talk actually went over: a window redrawn late must not
    /// restart the flash, and one redrawn often must not speed it up.
    pub fn note_overtime(&mut self, elapsed: Duration, target: Option<Duration>, now: Instant) {
        let over = target.is_some_and(|target| elapsed > target);
        if !over {
            // Back under — the presenter snoozed, or the timer was reset — so
            // the next overrun is a fresh piece of news, announced again.
            self.overtime_since = None;
            self.overtime_dismissed = false;
        } else if self.overtime_since.is_none() && !self.overtime_dismissed {
            self.overtime_since = Some(now);
        }
    }

    /// How strong the alert tint is now, or `None` when the talk is inside its
    /// time or the overrun has been answered. The clock's cue and the timer's
    /// overrun share both the shape of the pulse and the reason for it.
    pub fn flash(&self, now: Instant, reduce_motion: bool) -> Option<f32> {
        let since = self.overtime_since?;
        Some(flash_alpha(
            now.saturating_duration_since(since),
            reduce_motion,
        ))
    }

    /// Ask for a different snooze length, in whole minutes.
    pub fn nudge_snooze(&mut self, delta: i32) {
        let asked = self.snooze_minutes as i32 + delta;
        self.snooze_minutes = asked.clamp(1, MAX_SNOOZE_MINUTES as i32) as u32;
    }

    /// A target within bounds, and no countdown without one to count to.
    pub fn sanitise(&mut self) {
        self.target_seconds = self
            .target_seconds
            .map(|seconds| seconds.clamp(1, MAX_TARGET_SECONDS));
        if self.target_seconds.is_none() {
            self.count_down = false;
        }
        self.snooze_minutes = self.snooze_minutes.clamp(1, MAX_SNOOZE_MINUTES);
    }

    pub fn target(&self) -> Option<Duration> {
        self.target_seconds
            .map(|seconds| Duration::from_secs(seconds as u64))
    }

    /// Move the target by whole minutes; below a second is no target at all,
    /// which is how the menu offers "open-ended" without a separate control.
    ///
    /// The nudge keeps whatever seconds were typed — 20:30 stepped up by a
    /// minute is 21:30, not a silent rounding of the half-minute the presenter
    /// deliberately entered.
    pub fn nudge_target(&mut self, delta_minutes: i32) {
        let current = self.target_seconds.unwrap_or(0) as i32;
        let dialled = current + delta_minutes * 60;
        self.target_seconds = (dialled >= 1).then_some(dialled as u32);
        self.sanitise();
        self.sync_entry();
    }

    /// Take a length, in seconds, from a preset or the typed field.
    pub fn set_target(&mut self, seconds: Option<u32>) {
        self.target_seconds = seconds;
        self.sanitise();
        self.sync_entry();
    }

    /// Ask for a direction. Counting down needs something to count to, so a
    /// talk with no length gets a sensible one rather than a refusal.
    pub fn set_count_down(&mut self, count_down: bool) {
        if count_down && self.target_seconds.is_none() {
            self.target_seconds = Some(DEFAULT_TARGET_MINUTES * 60);
            self.sync_entry();
        }
        self.count_down = count_down;
        self.sanitise();
    }

    /// Take what is typed as a length, in seconds.
    ///
    /// `None` is a field that is not a length — empty, past sixty seconds, zero,
    /// or longer than a timer will run — which is the menu's cue to refuse it
    /// rather than round it into something that was not asked for.
    pub fn entered(&self) -> Option<u32> {
        let (minutes, seconds) = self.entry.values()?;
        if seconds > 59 {
            return None;
        }
        let total = minutes * 60 + seconds;
        (1..=MAX_TARGET_SECONDS).contains(&total).then_some(total)
    }

    /// Put the target in the field, as the digits that would type it.
    ///
    /// Called by everything that sets the length another way, so the field
    /// always shows what the timer is actually counting to: a preset pressed
    /// while stale digits sit in the box would otherwise read as a
    /// disagreement between the two halves of the same control.
    pub fn sync_entry(&mut self) {
        match self.target_seconds {
            // Past the two digits the minutes field holds there is nothing
            // honest to show, so it is left empty rather than made to lie.
            Some(seconds) if seconds < 100 * 60 => self.entry.set(seconds / 60, seconds % 60),
            _ => self.entry.clear(),
        }
    }

    /// What the line under the timer reads.
    pub fn caption(&self) -> String {
        match (self.count_down, self.target_seconds) {
            (true, Some(seconds)) => format!("counting down from {}", format_length(seconds)),
            (false, Some(seconds)) => format!("counting up to {}", format_length(seconds)),
            (_, None) => "counting up".to_string(),
        }
    }
}

/// A length said the shortest way that is still exact: whole minutes stay
/// whole, and a target with seconds in it shows them rather than rounding to a
/// number the presenter did not ask for.
pub fn format_length(seconds: u32) -> String {
    match (seconds / 60, seconds % 60) {
        (0, seconds) => format!("{seconds}s"),
        (minutes, 0) => format!("{minutes}m"),
        (minutes, seconds) => format!("{minutes}m {seconds:02}s"),
    }
}

/// The length a countdown assumes when one is asked for and none is set.
pub const DEFAULT_TARGET_MINUTES: u32 = 20;

/// The alarms, and the state of the popup that edits them.
///
/// Lives on the application beside `AnnotationControls`, for the same reason:
/// it is what the presenter is doing right now, not how the pane is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct AlarmControls {
    pub alarms: Vec<Alarm>,
    /// Whether the editing popup is showing.
    pub open: bool,
    /// The time being typed into the popup, hours then minutes.
    pub entry: TimeEntry,
    /// Whether a typed hour of twelve or less means the afternoon. Meaningless
    /// — and, in the popup, greyed — once the hour says which half it is in.
    pub afternoon: bool,
    /// The most recent alarm to have gone off and not yet been answered.
    pub ringing: Option<Alarm>,
    /// When it started ringing, which is what the flash is timed from.
    pub ringing_since: Option<std::time::Instant>,
    /// How long a snooze lasts, in minutes. Set in the popup, kept with the
    /// talk's other timing settings, and shared with the timer.
    pub snooze_minutes: u32,
    /// A cue put off for a few minutes. Deliberately not in `alarms`: it is
    /// this cue asking again, not a new one, and it must neither be saved nor
    /// appear in the list as something the presenter set.
    pub snoozed: Option<Alarm>,
}

impl Default for AlarmControls {
    fn default() -> Self {
        Self {
            alarms: Vec::new(),
            open: false,
            entry: TimeEntry::default(),
            afternoon: false,
            snooze_minutes: DEFAULT_SNOOZE_MINUTES,
            ringing: None,
            ringing_since: None,
            snoozed: None,
        }
    }
}

impl AlarmControls {
    pub fn new(alarms: Vec<Alarm>) -> Self {
        let mut controls = Self {
            alarms,
            ..Self::default()
        };
        controls.sanitise();
        controls
    }

    /// Sorted, deduplicated by time, and within the cap.
    pub fn sanitise(&mut self) {
        for alarm in &mut self.alarms {
            alarm.at %= 86_400;
        }
        self.alarms.sort_by_key(|alarm| alarm.at);
        self.alarms.dedup_by_key(|alarm| alarm.at);
        self.alarms.truncate(MAX_ALARMS);
        self.snooze_minutes = self.snooze_minutes.clamp(1, MAX_SNOOZE_MINUTES);
    }

    /// Ask for a different snooze length, in whole minutes.
    pub fn nudge_snooze(&mut self, delta: i32) {
        let asked = self.snooze_minutes as i32 + delta;
        self.snooze_minutes = asked.clamp(1, MAX_SNOOZE_MINUTES as i32) as u32;
    }

    /// Take what is typed as a time.
    ///
    /// `None` is for an hour or minute that is not a time at all, which is the
    /// popup's cue to refuse the addition rather than to silently round it.
    pub fn entered(&self) -> Option<u32> {
        let (hour, minute) = self.entry.values()?;
        if hour > 23 || minute > 59 {
            return None;
        }
        // Only an hour that could be either half of the day listens to the
        // toggle; 13:00 and later already said which one they are.
        let hour = if self.afternoon && hour < 12 {
            hour + 12
        } else {
            hour
        };
        Some((hour * 3600 + minute * 60) % 86_400)
    }

    /// Whether the typed hour is ambiguous, and so whether the AM/PM toggle
    /// has anything to decide.
    pub fn hour_is_ambiguous(&self) -> bool {
        // Nothing typed yet is midnight, which is as ambiguous as it gets.
        self.entry.left.is_empty() || self.entry.left.parse::<u32>().is_ok_and(|hour| hour < 12)
    }

    /// Put a time of day in the fields, as the digits that would type it.
    pub fn set_entry_to(&mut self, seconds_of_day: u32) {
        let seconds = seconds_of_day % 86_400;
        self.entry.set(seconds / 3600, (seconds % 3600) / 60);
        self.afternoon = false;
    }

    pub fn add(&mut self, alarm: Alarm) {
        if self.alarms.len() < MAX_ALARMS {
            self.alarms.push(alarm);
        }
        self.sanitise();
    }

    /// Take a cue off the list.
    ///
    /// A cue removed while it is going off stops going off: a marker that
    /// outlived its alarm would name a time no longer in the list, and the
    /// popup would offer no way to be rid of it.
    pub fn remove(&mut self, at: u32) {
        self.alarms.retain(|alarm| alarm.at != at);
        if self.ringing.as_ref().is_some_and(|alarm| alarm.at == at) {
            self.dismiss();
        }
    }

    pub fn is_full(&self) -> bool {
        self.alarms.len() >= MAX_ALARMS
    }

    /// Advance to `now`, ringing whatever fell in `(previous, now]`.
    ///
    /// `at` is the monotonic instant the flash is timed from; wall-clock
    /// seconds cannot do that job, because they have no sub-second part and
    /// jump when the timezone does.
    pub fn strike(&mut self, previous: u32, now: u32, at: std::time::Instant) {
        // The most recent cue in the window is the one that rings: two at
        // once is a schedule the presenter has already lost track of.
        let mut struck = crossed(previous, now, &self.alarms)
            .last()
            .map(|alarm| (*alarm).clone());

        // A snoozed cue asking again wins over one that merely came due in
        // the same window: it is the one already waiting on an answer.
        if let Some(snoozed) = self.snoozed.clone() {
            if !crossed(previous, now, std::slice::from_ref(&snoozed)).is_empty() {
                self.snoozed = None;
                struck = Some(snoozed);
            }
        }

        if let Some(alarm) = struck {
            self.ringing = Some(alarm);
            self.ringing_since = Some(at);
        }
    }

    /// Put the ringing cue off for as long as the presenter asked snoozes to
    /// last.
    ///
    /// Snoozing repeatedly is allowed: a presenter who needs another five
    /// minutes twice is not doing anything the clock should argue with.
    pub fn snooze(&mut self, now: u32) {
        if let Some(alarm) = self.ringing.take() {
            self.ringing_since = None;
            self.snoozed = Some(Alarm::new(now + self.snooze_minutes * 60, alarm.label));
        }
    }

    /// Answer the ringing cue for good.
    pub fn dismiss(&mut self) {
        self.ringing = None;
        self.ringing_since = None;
    }

    /// How strong the alert tint is now, or `None` when nothing is ringing.
    pub fn flash(&self, now: std::time::Instant, reduce_motion: bool) -> Option<f32> {
        let since = self.ringing_since?;
        Some(flash_alpha(
            now.saturating_duration_since(since),
            reduce_motion,
        ))
    }

    /// The next cue due at or after `now`, wrapping past midnight so that a
    /// late-evening talk still sees an alarm set for just after twelve.
    pub fn next(&self, now: u32) -> Option<&Alarm> {
        self.alarms
            .iter()
            .find(|alarm| alarm.at >= now)
            .or_else(|| self.alarms.first())
    }
}

/// Which cues fell in `(previous, now]`, oldest first.
///
/// A crossing rather than an equality test: ticks are not guaranteed to land
/// on any particular second, and `now == alarm.at` silently drops a cue the
/// one time it matters. Pure, and taking both ends explicitly, so midnight
/// and resume are assertable without a clock.
pub fn crossed(previous: u32, now: u32, alarms: &[Alarm]) -> Vec<&Alarm> {
    let (previous, now) = (previous % 86_400, now % 86_400);
    let elapsed = if now >= previous {
        now - previous
    } else {
        // The window straddles midnight.
        86_400 - previous + now
    };

    // A gap this large is a suspended machine, not a slow frame. Announcing
    // everything that passed while the lid was shut would be a burst of cues
    // for moments that have gone.
    if elapsed > STALE_AFTER_SECONDS {
        return Vec::new();
    }

    let mut struck: Vec<&Alarm> = alarms
        .iter()
        .filter(|alarm| {
            if now >= previous {
                alarm.at > previous && alarm.at <= now
            } else {
                alarm.at > previous || alarm.at <= now
            }
        })
        .collect();
    // Oldest first within the window, so the caller's last write wins and the
    // most recent cue is the one left ringing.
    struck.sort_by_key(|alarm| {
        let since = if alarm.at <= now {
            now - alarm.at
        } else {
            86_400 - alarm.at + now
        };
        std::cmp::Reverse(since)
    });
    struck
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minutes(count: u64) -> Duration {
        Duration::from_secs(count * 60)
    }

    #[test]
    fn asking_to_count_down_with_no_length_set_picks_one() {
        let mut controls = TimerControls::default();
        controls.set_count_down(true);
        assert!(controls.count_down);
        assert_eq!(controls.target_seconds, Some(DEFAULT_TARGET_MINUTES * 60));
    }

    #[test]
    fn dialling_the_length_away_leaves_a_timer_counting_up() {
        let mut controls = TimerControls::new(Some(2 * 60), true);
        controls.nudge_target(-5);
        assert_eq!(controls.target_seconds, None, "under a second is no target");
        assert!(
            !controls.count_down,
            "a countdown with nothing to count to is not a countdown"
        );
        assert_eq!(controls.caption(), "counting up");
    }

    #[test]
    fn the_length_stays_within_bounds() {
        let mut controls = TimerControls::new(Some(30 * 60), false);
        controls.nudge_target(10_000);
        assert_eq!(controls.target_seconds, Some(MAX_TARGET_SECONDS));
    }

    #[test]
    fn counting_up_without_a_target_is_never_overtime() {
        let options = TimerOptions::default();
        let reading = options.reading(minutes(90), None, false);
        assert_eq!(reading.seconds, 5_400);
        assert!(!reading.overtime && !reading.warning);
        assert_eq!(reading.label, "ELAPSED");
    }

    #[test]
    fn the_warning_starts_exactly_at_the_configured_minutes() {
        let options = TimerOptions { warning_minutes: 5 };
        let target = Some(minutes(20));
        assert!(
            !options.reading(minutes(14), target, false).warning,
            "six left"
        );
        assert!(
            options.reading(minutes(15), target, false).warning,
            "five left"
        );
        assert!(
            options.reading(minutes(20), target, false).warning,
            "on target"
        );
    }

    #[test]
    fn one_second_past_the_target_is_overtime() {
        let options = TimerOptions::default();
        let target = Some(minutes(20));
        let on_target = options.reading(minutes(20), target, false);
        assert!(!on_target.overtime, "exactly on target is not yet over");

        let over = options.reading(Duration::from_secs(20 * 60 + 1), target, false);
        assert!(over.overtime);
        assert!(!over.warning, "overtime replaces the warning");
        assert_eq!(over.label, "OVERTIME");
    }

    #[test]
    fn counting_down_shows_what_is_left_and_then_how_far_past() {
        let options = TimerOptions::default();
        let target = Some(minutes(20));
        let left = options.reading(minutes(5), target, true);
        assert_eq!(left.seconds, 15 * 60);
        assert_eq!(left.label, "REMAINING");

        let past = options.reading(minutes(25), target, true);
        assert_eq!(past.seconds, 5 * 60, "five minutes over, not minus five");
        assert!(past.overtime);
    }

    #[test]
    fn the_clock_formats_in_both_conventions() {
        let twenty_four = ClockOptions {
            twenty_four_hour: true,
            show_seconds: false,
            ..ClockOptions::default()
        };
        assert_eq!(twenty_four.format(13 * 3600 + 5 * 60), "13:05");

        let twelve = ClockOptions {
            twenty_four_hour: false,
            show_seconds: true,
            ..ClockOptions::default()
        };
        assert_eq!(twelve.format(13 * 3600 + 5 * 60 + 9), "01:05:09 PM");
        assert_eq!(
            twelve.format(0),
            "12:00:00 AM",
            "midnight is twelve, not zero"
        );
        assert_eq!(twelve.format(12 * 3600), "12:00:00 PM");
    }

    fn at(hours: u32, minutes: u32) -> u32 {
        hours * 3600 + minutes * 60
    }

    fn alarms(times: &[u32]) -> Vec<Alarm> {
        times.iter().map(|t| Alarm::new(*t, None)).collect()
    }

    #[test]
    fn a_cue_exactly_on_the_tick_still_fires() {
        let list = alarms(&[at(14, 20)]);
        let struck = crossed(at(14, 20) - 1, at(14, 20), &list);
        assert_eq!(struck.len(), 1, "the closing end of the window is included");

        // And it does not fire twice: the next window opens past it.
        assert!(crossed(at(14, 20), at(14, 20) + 1, &list).is_empty());
    }

    #[test]
    fn a_cue_between_two_ticks_is_not_dropped() {
        // Ticks do not land on whole seconds, and a slow frame can skip
        // several. Equality testing would lose this cue entirely.
        let list = alarms(&[at(14, 20)]);
        let struck = crossed(at(14, 20) - 4, at(14, 20) + 3, &list);
        assert_eq!(struck.len(), 1);
    }

    #[test]
    fn a_window_across_midnight_still_strikes() {
        let list = alarms(&[10, 86_395]);
        let struck = crossed(86_390, 20, &list);
        assert_eq!(struck.len(), 2, "both sides of midnight are in the window");
        assert_eq!(struck[0].at, 86_395, "oldest first");
        assert_eq!(struck[1].at, 10);
    }

    #[test]
    fn nothing_is_announced_after_a_suspend() {
        // The lid was shut over lunch. These cues have gone; saying so now,
        // urgently, would be worse than silence.
        let list = alarms(&[at(12, 30), at(13, 00)]);
        assert!(crossed(at(12, 00), at(14, 00), &list).is_empty());
    }

    #[test]
    fn the_next_cue_wraps_past_midnight() {
        let controls = AlarmControls::new(alarms(&[at(9, 00), at(23, 50)]));
        assert_eq!(controls.next(at(12, 00)).unwrap().at, at(23, 50));
        assert_eq!(
            controls.next(at(23, 55)).unwrap().at,
            at(9, 00),
            "a late talk sees tomorrow morning's cue, not nothing"
        );
    }

    #[test]
    fn typing_crosses_from_the_hours_into_the_minutes_by_itself() {
        let mut entry = TimeEntry::default();
        assert!(!entry.type_left("1"), "half an hour typed stays put");
        assert!(entry.type_left("14"), "a full hour hands the typing on");
        entry.type_right("20");
        assert_eq!((entry.left.as_str(), entry.right.as_str()), ("14", "20"));

        // Typed straight through without pausing at the colon: the overflow
        // starts the minutes rather than being dropped on the floor.
        let mut entry = TimeEntry::default();
        assert!(entry.type_left("1420"));
        assert_eq!((entry.left.as_str(), entry.right.as_str()), ("14", "20"));
    }

    #[test]
    fn each_half_holds_two_digits_and_only_digits() {
        let mut entry = TimeEntry::default();
        entry.type_right("2059");
        assert_eq!(entry.right, "20");
        entry.type_right("ab");
        assert_eq!(entry.right, "");
    }

    #[test]
    fn a_half_typed_time_is_read_as_the_hour_on_its_own() {
        let mut controls = AlarmControls::default();
        controls.entry.left = "9".into();
        assert_eq!(
            controls.entered(),
            Some(at(9, 0)),
            "an empty minutes field is o'clock, not a refusal"
        );
    }

    #[test]
    fn an_hour_or_minute_that_is_not_a_time_is_refused_rather_than_rounded() {
        let mut controls = AlarmControls::default();
        controls.entry.set(25, 0);
        assert_eq!(controls.entered(), None, "there is no 25th hour");
        controls.entry.set(10, 99);
        assert_eq!(controls.entered(), None, "there is no 99th minute");
        controls.entry.clear();
        assert_eq!(controls.entered(), None, "an empty field is not a time");
    }

    #[test]
    fn only_an_ambiguous_hour_listens_to_the_afternoon_toggle() {
        let mut controls = AlarmControls::default();
        controls.entry.set(2, 30);
        assert!(controls.hour_is_ambiguous());
        controls.afternoon = true;
        assert_eq!(controls.entered(), Some(at(14, 30)));

        // Past noon the digits have already said which half of the day this
        // is, so the toggle is greyed and must change nothing.
        controls.entry.set(15, 30);
        assert!(!controls.hour_is_ambiguous());
        assert_eq!(controls.entered(), Some(at(15, 30)));
    }

    #[test]
    fn a_length_that_is_not_a_length_is_refused_rather_than_rounded() {
        let mut controls = TimerControls::default();
        controls.entry.set(5, 30);
        assert_eq!(controls.entered(), Some(330), "five minutes thirty");

        controls.entry.set(1, 99);
        assert_eq!(controls.entered(), None, "there is no 99th second");
        controls.entry.set(0, 0);
        assert_eq!(controls.entered(), None, "no time at all is not a length");

        // Two digits of minutes cannot reach the cap, so the longest thing the
        // field can say is still a length the timer will run.
        controls.entry.set(99, 59);
        assert_eq!(controls.entered(), Some(99 * 60 + 59));
    }

    #[test]
    fn the_length_field_shows_what_the_timer_is_counting_to() {
        let controls = TimerControls::new(Some(5 * 60 + 30), true);
        assert_eq!(
            (controls.entry.left.as_str(), controls.entry.right.as_str()),
            ("05", "30")
        );
        assert_eq!(controls.caption(), "counting down from 5m 30s");

        // Whole minutes are said as whole minutes; the seconds are only
        // spelled out when there are some.
        assert_eq!(format_length(20 * 60), "20m");
        assert_eq!(format_length(45), "45s");
    }

    #[test]
    fn a_snooze_lasts_as_long_as_the_presenter_asked() {
        let mut controls = AlarmControls::new(alarms(&[at(14, 20)]));
        controls.snooze_minutes = 9;
        controls.ringing = Some(Alarm::new(at(14, 20), None));
        controls.snooze(at(14, 20));
        assert_eq!(controls.snoozed.as_ref().unwrap().at, at(14, 29));

        // And is a snooze, not a different alarm: an hour is past the point
        // where "in a moment" means anything.
        controls.nudge_snooze(500);
        assert_eq!(controls.snooze_minutes, MAX_SNOOZE_MINUTES);
        controls.nudge_snooze(-500);
        assert_eq!(
            controls.snooze_minutes, 1,
            "a snooze is never no time at all"
        );
    }

    #[test]
    fn running_over_pulses_from_the_moment_it_happened_until_it_is_answered() {
        let start = Instant::now();
        let mut controls = TimerControls::new(Some(20), true);
        let target = Some(Duration::from_secs(20 * 60));

        controls.note_overtime(Duration::from_secs(19 * 60), target, start);
        assert_eq!(controls.flash(start, false), None, "still inside its time");

        // The moment it goes over is the moment the pulse is timed from, and
        // a later tick must not restart it.
        let over = start + Duration::from_secs(60);
        controls.note_overtime(Duration::from_secs(20 * 60 + 1), target, over);
        assert_eq!(controls.overtime_since, Some(over));
        controls.note_overtime(
            Duration::from_secs(20 * 60 + 30),
            target,
            over + FLASH_PERIOD,
        );
        assert_eq!(
            controls.overtime_since,
            Some(over),
            "the pulse did not restart"
        );
        assert!(controls.flash(over, false).is_some());

        // Answered: no more pulse, and the reading is left alone about it.
        controls.dismiss_overtime();
        assert_eq!(controls.flash(over, false), None);
        controls.note_overtime(Duration::from_secs(20 * 60 + 90), target, over);
        assert_eq!(
            controls.flash(over, false),
            None,
            "an overrun answered once does not come back on the next tick"
        );

        // An open-ended talk is never over.
        let mut open_ended = TimerControls::new(None, false);
        open_ended.note_overtime(Duration::from_secs(9_000), None, start);
        assert_eq!(open_ended.flash(start, false), None);
    }

    #[test]
    fn snoozing_the_timer_moves_the_end_of_the_talk_and_clears_the_overrun() {
        let mut controls = TimerControls::new(Some(20 * 60), true);
        controls.snooze_minutes = 5;
        controls.dismiss_overtime();
        controls.snooze();
        assert_eq!(controls.target_seconds, Some(25 * 60));
        assert!(
            !controls.overtime_dismissed,
            "the talk is no longer over, so there is nothing to have acknowledged"
        );

        // An open-ended talk has no end to push out, and the line that offers
        // to push it is not drawn for one.
        let mut open_ended = TimerControls::new(None, false);
        open_ended.snooze();
        assert_eq!(open_ended.target_seconds, None);
    }

    #[test]
    fn the_list_is_sorted_deduplicated_and_capped() {
        let mut controls = AlarmControls::new(alarms(&[at(15, 00), at(9, 00), at(15, 00)]));
        assert_eq!(
            controls.alarms.iter().map(|a| a.at).collect::<Vec<_>>(),
            vec![at(9, 00), at(15, 00)],
            "sorted, and one cue per minute"
        );

        for minute in 0..20 {
            controls.add(Alarm::new(at(6, minute), None));
        }
        assert_eq!(controls.alarms.len(), MAX_ALARMS);
        assert!(controls.is_full());
    }

    #[test]
    fn removing_the_cue_that_is_going_off_stops_it_going_off() {
        let mut controls = AlarmControls::new(alarms(&[at(14, 20), at(15, 00)]));
        controls.ringing = Some(Alarm::new(at(14, 20), None));

        controls.remove(at(15, 00));
        assert!(controls.ringing.is_some(), "another cue is not this one");

        controls.remove(at(14, 20));
        assert!(
            controls.ringing.is_none(),
            "a marker for a cue that is gone could never be dismissed"
        );
    }

    #[test]
    fn the_flash_pulses_gently_and_rests_between_pulses() {
        let alpha = |ms: u64| flash_alpha(Duration::from_millis(ms), false);

        assert_eq!(alpha(0), 0.0, "it arrives without an edge");
        assert!(
            (alpha(600) - FLASH_PEAK).abs() < 1e-3,
            "strongest halfway through the visible part"
        );
        assert!(alpha(600) <= FLASH_PEAK, "and never stronger than the peak");
        assert_eq!(alpha(1_200), 0.0, "then it is gone");
        assert_eq!(alpha(3_000), 0.0, "most of the cycle is rest");

        // And it keeps asking: the next cycle is the same as the first.
        assert!((alpha(4_600) - alpha(600)).abs() < 1e-3);
        assert!(
            (alpha(40_600) - alpha(600)).abs() < 1e-3,
            "still going later"
        );
    }

    #[test]
    fn less_motion_means_a_steady_wash_rather_than_a_pulse() {
        for ms in [0, 600, 3_000, 4_600] {
            assert_eq!(
                flash_alpha(Duration::from_millis(ms), true),
                FLASH_STEADY,
                "a reduced-motion tint never moves"
            );
        }
    }

    #[test]
    fn a_snoozed_cue_asks_again_and_is_not_a_new_alarm() {
        let now = at(14, 20);
        let mut controls = AlarmControls::new(alarms(&[now]));
        let instant = std::time::Instant::now();

        controls.strike(now - 1, now, instant);
        assert!(controls.ringing.is_some() && controls.ringing_since.is_some());

        controls.snooze(now);
        assert!(controls.ringing.is_none(), "snoozing stops the flash");
        assert!(controls.ringing_since.is_none());
        assert_eq!(controls.snoozed.as_ref().unwrap().at, now + SNOOZE_SECONDS);
        assert_eq!(
            controls.alarms.len(),
            1,
            "a snooze is the same cue asking again, not another one set"
        );

        // Five minutes later it asks again, and stops being snoozed.
        let later = now + SNOOZE_SECONDS;
        controls.strike(later - 1, later, instant);
        assert!(controls.ringing.is_some());
        assert!(controls.snoozed.is_none());
    }

    #[test]
    fn a_snoozed_cue_keeps_what_it_was_for() {
        let now = at(9, 00);
        let mut controls = AlarmControls::new(vec![Alarm::new(now, Some("handoff".to_string()))]);
        controls.strike(now - 1, now, std::time::Instant::now());
        controls.snooze(now);
        assert_eq!(
            controls.snoozed.as_ref().unwrap().label.as_deref(),
            Some("handoff"),
            "a cue you put off is still the cue it was"
        );
    }

    #[test]
    fn an_alarm_past_midnight_wraps_rather_than_overflowing() {
        assert_eq!(Alarm::new(86_400 + 60, None).at, 60);
    }

    #[test]
    fn an_absurd_warning_threshold_is_clamped() {
        let mut options = TimerOptions {
            warning_minutes: 10_000,
        };
        options.sanitise();
        assert_eq!(options.warning_minutes, MAX_WARNING_MINUTES);
    }
}

/// An edit to the timer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimerPatch {
    WarningMinutes(u32),
}

impl TimerPatch {
    pub fn apply(self, options: &mut TimerOptions) {
        match self {
            TimerPatch::WarningMinutes(value) => options.warning_minutes = value,
        }
    }
}

/// An edit to the clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClockPatch {
    TwentyFourHour(bool),
    ShowSeconds(bool),
    ShowAlarms(bool),
}

impl ClockPatch {
    pub fn apply(self, options: &mut ClockOptions) {
        match self {
            ClockPatch::TwentyFourHour(value) => options.twenty_four_hour = value,
            ClockPatch::ShowSeconds(value) => options.show_seconds = value,
            ClockPatch::ShowAlarms(value) => options.show_alarms = value,
        }
    }
}

/// A timer below this height is decoration rather than information.
pub const READABLE_MINIMUM_HEIGHT: f32 = 44.0;

/// What is wrong with this timer at this size, if anything.
pub fn validate(scale: f32, inner: (f32, f32)) -> Vec<crate::widgets::Complaint> {
    let scaled = READABLE_MINIMUM_HEIGHT * scale.clamp(0.5, 2.0);
    if inner.1 < scaled {
        return vec![crate::widgets::Complaint {
            message: "Timer text may be unreadable at its current size",
            consequence: "The timer needs to be legible from where you will be standing.",
        }];
    }
    Vec::new()
}
