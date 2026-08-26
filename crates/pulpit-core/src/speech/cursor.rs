//! What is being read, where it has got to, and what to do next.
//!
//! This is the part of speech that is worth testing hardest, because it is
//! where every awkward case lives: a page whose text has not arrived yet, a
//! stop that races a synthesis that was already in flight, a resume after the
//! document reloaded under the reader, a page with no text on it at all.
//! None of those need audio to reproduce, so none of them are tested with
//! audio.
//!
//! The shape is a fold: a [`Reading`] takes one [`Event`] and returns the
//! [`Action`]s the outside world should carry out. It reads no clock, spawns
//! nothing and owns no handle, so a whole session — start, pause, three page
//! turns and a stop — is an ordinary unit test.

use crate::page::PageIndex;

use super::segment::{sentences, Sentence};

/// How much to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// This page, then stop.
    Page,
    /// This page and every page after it.
    Document,
    /// A selection, which belongs to no page after it is taken.
    Selection,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Page => "this page",
            Scope::Document => "the whole document",
            Scope::Selection => "the selection",
        }
    }
}

/// Where speech is.
///
/// `Starting` and `Speaking` are separate because the gap between them is
/// real — a synthesis takes a moment, and during it the reader has pressed a
/// button and deserves to see that something happened. Collapsing them would
/// make the UI either lie early or look dead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechState {
    /// Nothing is being read.
    Idle,
    /// Asked for, not yet audible.
    Starting,
    /// Audible now.
    Speaking,
    /// Stopped between sentences, position kept.
    Paused,
    /// Waiting for a page's text to arrive before it can go on.
    AwaitingText(PageIndex),
}

impl SpeechState {
    pub fn is_active(&self) -> bool {
        !matches!(self, SpeechState::Idle)
    }

    /// Whether a pause would do anything.
    pub fn is_pausable(&self) -> bool {
        matches!(
            self,
            SpeechState::Starting | SpeechState::Speaking | SpeechState::AwaitingText(_)
        )
    }
}

/// What happened, from the reader or from the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Read `scope`, beginning at `page`.
    Start {
        scope: Scope,
        page: PageIndex,
    },
    /// Read this text and nothing else. Used for a selection, which has no
    /// page to continue from.
    StartSelection {
        text: String,
        page: PageIndex,
    },
    /// A page's text arrived, in answer to [`Action::NeedText`].
    ///
    /// `text` empty means the page genuinely has none — a scanned image, a
    /// full-bleed photograph — which is a fact, not a failure.
    TextArrived {
        page: PageIndex,
        text: String,
    },
    /// The page requested does not exist: the end of the document.
    NoSuchPage(PageIndex),
    /// The engine began the utterance it was last given.
    UtteranceStarted,
    /// The engine finished the utterance it was last given.
    UtteranceFinished,
    /// The engine could not speak. Speech stops; the reason is the caller's
    /// to report.
    EngineFailed,
    Pause,
    Resume,
    Stop,
    /// Skip forward or back a sentence. Backwards from the first sentence of
    /// a page stays put rather than reopening the previous page: the reader
    /// asked for a sentence, and silently changing page would lose their
    /// place on the one they are looking at.
    Skip(Direction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Back,
}

/// What the outside world should do about it.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Synthesise and play this text.
    ///
    /// Exactly one utterance is *playing* at a time: the engine is told to
    /// speak, and nothing else is played until it reports finished or failed.
    Speak(String),
    /// Synthesise this text now but do not play it: it is what comes next.
    ///
    /// Spawning a synthesiser costs a process start, measured at between one
    /// and two seconds. Paid once, before the first sentence, that is a
    /// reasonable wait; paid between every pair of sentences it is a stutter
    /// that makes the feature feel broken. This action is how it gets paid
    /// only once — the gap is covered by the sentence currently playing.
    ///
    /// Always emitted immediately after the [`Action::Speak`] it follows, so
    /// a consumer that ignores it is merely slow, never wrong.
    Prefetch(String),
    /// Stop immediately and drop anything buffered.
    StopSpeaking,
    /// Fetch this page's text and send it back as [`Event::TextArrived`].
    NeedText(PageIndex),
    /// Show this page: speech has moved on to it.
    ShowPage(PageIndex),
    /// Highlight the span currently being spoken, as a byte range into the
    /// text of `page`. Absent for a selection, which the reader can already
    /// see.
    Highlight {
        page: PageIndex,
        range: std::ops::Range<usize>,
    },
    /// Reading finished on its own, having reached the end of the scope.
    Finished,
}

use serde::{Deserialize, Serialize};

/// The reading session.
#[derive(Debug, Clone, Default)]
pub struct Reading {
    state: SpeechStateInner,
}

#[derive(Debug, Clone, Default)]
struct SpeechStateInner {
    state: Option<Active>,
}

#[derive(Debug, Clone)]
struct Active {
    scope: Scope,
    page: PageIndex,
    text: String,
    units: Vec<Sentence>,
    /// Index of the sentence being spoken, or about to be.
    index: usize,
    state: SpeechState,
}

// There is deliberately no generation counter in here. Staleness — a
// `Finished` from an utterance that was stopped, skipped past, or superseded —
// is decided where the utterances live, in the speaker, whose events carry a
// generation and whose drain discards the stale ones before this fold ever
// sees them. The one race this fold can see on its own, a finish that lands
// after a pause, is handled by the `Paused` check in `advance`. An earlier
// version kept a counter here as well; it was incremented in four places and
// read in none, which is the natural fate of a defence duplicated on the
// wrong side of a boundary.

impl Reading {
    pub fn new() -> Reading {
        Reading::default()
    }

    pub fn state(&self) -> SpeechState {
        self.state
            .state
            .as_ref()
            .map(|active| active.state.clone())
            .unwrap_or(SpeechState::Idle)
    }

    pub fn scope(&self) -> Option<Scope> {
        self.state.state.as_ref().map(|active| active.scope)
    }

    /// The page speech is on, for the UI to show alongside the controls.
    pub fn page(&self) -> Option<PageIndex> {
        self.state.state.as_ref().map(|active| active.page)
    }

    /// How far through the current page, as (sentence, total). Useful as a
    /// progress readout and in tests.
    pub fn progress(&self) -> Option<(usize, usize)> {
        let active = self.state.state.as_ref()?;
        Some((active.index, active.units.len()))
    }

    /// The text of the sentence currently being spoken.
    pub fn current_text(&self) -> Option<&str> {
        let active = self.state.state.as_ref()?;
        let unit = active.units.get(active.index)?;
        Some(unit.text(&active.text))
    }

    pub fn update(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Start { scope, page } => self.start(scope, page),
            Event::StartSelection { text, page } => self.start_selection(text, page),
            Event::TextArrived { page, text } => self.text_arrived(page, text),
            Event::NoSuchPage(_) => self.finish(),
            Event::UtteranceStarted => {
                if let Some(active) = self.state.state.as_mut() {
                    if active.state == SpeechState::Starting {
                        active.state = SpeechState::Speaking;
                    }
                }
                Vec::new()
            }
            Event::UtteranceFinished => self.advance(),
            Event::EngineFailed => {
                self.state.state = None;
                vec![Action::StopSpeaking]
            }
            Event::Pause => self.pause(),
            Event::Resume => self.resume(),
            Event::Stop => self.stop(),
            Event::Skip(direction) => self.skip(direction),
        }
    }

    fn start(&mut self, scope: Scope, page: PageIndex) -> Vec<Action> {
        self.state.state = Some(Active {
            scope,
            page,
            text: String::new(),
            units: Vec::new(),
            index: 0,
            state: SpeechState::AwaitingText(page),
        });
        vec![Action::StopSpeaking, Action::NeedText(page)]
    }

    fn start_selection(&mut self, text: String, page: PageIndex) -> Vec<Action> {
        let units = sentences(&text);
        if units.is_empty() {
            self.state.state = None;
            return vec![Action::StopSpeaking, Action::Finished];
        }
        self.state.state = Some(Active {
            scope: Scope::Selection,
            page,
            text,
            units,
            index: 0,
            state: SpeechState::Starting,
        });
        let mut actions = vec![Action::StopSpeaking];
        actions.extend(self.speak_current());
        actions
    }

    fn text_arrived(&mut self, page: PageIndex, text: String) -> Vec<Action> {
        let Some(active) = self.state.state.as_mut() else {
            return Vec::new();
        };
        // Text for a page we have since moved off, or a stale answer after a
        // stop: ignore it rather than jumping back.
        if active.state != SpeechState::AwaitingText(page) {
            return Vec::new();
        }
        active.units = sentences(&text);
        active.text = text;
        active.index = 0;

        if active.units.is_empty() {
            // A page with no text is not an error and not the end: in
            // document scope, read on.
            return match active.scope {
                Scope::Document => self.next_page(),
                _ => self.finish(),
            };
        }
        active.state = SpeechState::Starting;
        self.speak_current()
    }

    fn speak_current(&mut self) -> Vec<Action> {
        let Some(active) = self.state.state.as_mut() else {
            return Vec::new();
        };
        let Some(unit) = active.units.get(active.index) else {
            return Vec::new();
        };
        let text = unit.text(&active.text).to_string();
        let range = unit.range.clone();
        let page = active.page;
        let scope = active.scope;
        active.state = SpeechState::Starting;

        let next = active
            .units
            .get(active.index + 1)
            .map(|unit| unit.text(&active.text).to_string());

        let mut actions = Vec::new();
        if scope != Scope::Selection {
            actions.push(Action::Highlight { page, range });
        }
        actions.push(Action::Speak(text));
        // The last sentence of a page has nothing to prefetch. The first
        // sentence of the *next* page cannot be prefetched either, because its
        // text has not been asked for yet — which is why a page turn is the
        // one place a small gap is expected.
        if let Some(next) = next {
            actions.push(Action::Prefetch(next));
        }
        actions
    }

    fn advance(&mut self) -> Vec<Action> {
        let Some(active) = self.state.state.as_mut() else {
            return Vec::new();
        };
        // A finish that arrives while paused belongs to the utterance that
        // was already playing when the pause landed. The pause has already
        // decided where we are; do not move.
        if active.state == SpeechState::Paused {
            return Vec::new();
        }
        active.index += 1;
        if active.index < active.units.len() {
            return self.speak_current();
        }
        match active.scope {
            Scope::Document => self.next_page(),
            Scope::Page | Scope::Selection => self.finish(),
        }
    }

    fn next_page(&mut self) -> Vec<Action> {
        let Some(active) = self.state.state.as_mut() else {
            return Vec::new();
        };
        let next = PageIndex(active.page.0 + 1);
        active.page = next;
        active.index = 0;
        active.units.clear();
        active.text.clear();
        active.state = SpeechState::AwaitingText(next);
        vec![Action::ShowPage(next), Action::NeedText(next)]
    }

    fn pause(&mut self) -> Vec<Action> {
        let Some(active) = self.state.state.as_mut() else {
            return Vec::new();
        };
        if !active.state.is_pausable() {
            return Vec::new();
        }
        active.state = SpeechState::Paused;
        vec![Action::StopSpeaking]
    }

    fn resume(&mut self) -> Vec<Action> {
        let Some(active) = self.state.state.as_ref() else {
            return Vec::new();
        };
        if active.state != SpeechState::Paused {
            return Vec::new();
        }
        // Nothing was loaded when the pause landed — a pause during
        // AwaitingText — so ask again rather than speaking an empty unit.
        if active.units.is_empty() {
            let page = active.page;
            if let Some(active) = self.state.state.as_mut() {
                active.state = SpeechState::AwaitingText(page);
            }
            return vec![Action::NeedText(page)];
        }
        self.speak_current()
    }

    fn stop(&mut self) -> Vec<Action> {
        if self.state.state.is_none() {
            return Vec::new();
        }
        self.state.state = None;
        vec![Action::StopSpeaking]
    }

    fn skip(&mut self, direction: Direction) -> Vec<Action> {
        let Some(active) = self.state.state.as_mut() else {
            return Vec::new();
        };
        if active.units.is_empty() {
            return Vec::new();
        }
        let was_paused = active.state == SpeechState::Paused;
        match direction {
            Direction::Forward => {
                if active.index + 1 >= active.units.len() {
                    // Past the last sentence: the same thing finishing
                    // naturally does.
                    return match active.scope {
                        Scope::Document => {
                            let mut actions = vec![Action::StopSpeaking];
                            actions.extend(self.next_page());
                            actions
                        }
                        _ => {
                            let mut actions = vec![Action::StopSpeaking];
                            actions.extend(self.finish());
                            actions
                        }
                    };
                }
                active.index += 1;
            }
            Direction::Back => {
                active.index = active.index.saturating_sub(1);
            }
        }
        let mut actions = vec![Action::StopSpeaking];
        if was_paused {
            // Skipping while paused moves the cursor and shows the new
            // position, but does not start talking: the reader paused on
            // purpose.
            let Some(active) = self.state.state.as_mut() else {
                return actions;
            };
            active.state = SpeechState::Paused;
            if active.scope != Scope::Selection {
                if let Some(unit) = active.units.get(active.index) {
                    actions.push(Action::Highlight {
                        page: active.page,
                        range: unit.range.clone(),
                    });
                }
            }
            return actions;
        }
        actions.extend(self.speak_current());
        actions
    }

    fn finish(&mut self) -> Vec<Action> {
        self.state.state = None;
        vec![Action::StopSpeaking, Action::Finished]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "First sentence here. Second sentence here. Third one.";

    fn started() -> (Reading, Vec<Action>) {
        let mut reading = Reading::new();
        let actions = reading.update(Event::Start {
            scope: Scope::Page,
            page: PageIndex(3),
        });
        (reading, actions)
    }

    #[test]
    fn starting_asks_for_text_before_it_can_speak() {
        let (reading, actions) = started();
        assert_eq!(
            actions,
            vec![Action::StopSpeaking, Action::NeedText(PageIndex(3))]
        );
        assert_eq!(reading.state(), SpeechState::AwaitingText(PageIndex(3)));
    }

    #[test]
    fn text_arriving_starts_the_first_sentence_and_highlights_it() {
        let (mut reading, _) = started();
        let actions = reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        assert_eq!(
            actions,
            vec![
                Action::Highlight {
                    page: PageIndex(3),
                    range: 0..20
                },
                Action::Speak("First sentence here.".into()),
                Action::Prefetch("Second sentence here.".into()),
            ]
        );
        assert_eq!(reading.state(), SpeechState::Starting);
        reading.update(Event::UtteranceStarted);
        assert_eq!(reading.state(), SpeechState::Speaking);
        assert_eq!(reading.progress(), Some((0, 3)));
    }

    #[test]
    fn finishing_a_sentence_speaks_the_next() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        let actions = reading.update(Event::UtteranceFinished);
        assert!(actions.contains(&Action::Speak("Second sentence here.".into())));
        assert_eq!(reading.progress(), Some((1, 3)));
    }

    #[test]
    fn the_last_sentence_of_a_page_prefetches_nothing() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        reading.update(Event::UtteranceFinished);
        let actions = reading.update(Event::UtteranceFinished); // last sentence
        assert!(actions.contains(&Action::Speak("Third one.".into())));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Prefetch(_))),
            "nothing follows it on this page"
        );
    }

    #[test]
    fn page_scope_finishes_at_the_end_of_the_page() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        reading.update(Event::UtteranceFinished);
        reading.update(Event::UtteranceFinished);
        let actions = reading.update(Event::UtteranceFinished);
        assert!(actions.contains(&Action::Finished));
        assert_eq!(reading.state(), SpeechState::Idle);
    }

    #[test]
    fn document_scope_turns_the_page_and_shows_it() {
        let mut reading = Reading::new();
        reading.update(Event::Start {
            scope: Scope::Document,
            page: PageIndex(3),
        });
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: "Only one sentence.".into(),
        });
        let actions = reading.update(Event::UtteranceFinished);
        assert_eq!(
            actions,
            vec![
                Action::ShowPage(PageIndex(4)),
                Action::NeedText(PageIndex(4))
            ]
        );
        assert_eq!(reading.page(), Some(PageIndex(4)));
    }

    #[test]
    fn document_scope_reads_through_a_page_with_no_text() {
        let mut reading = Reading::new();
        reading.update(Event::Start {
            scope: Scope::Document,
            page: PageIndex(0),
        });
        reading.update(Event::TextArrived {
            page: PageIndex(0),
            text: "One.".into(),
        });
        reading.update(Event::UtteranceFinished);
        // A full-bleed image: no text, not an error, not the end.
        let actions = reading.update(Event::TextArrived {
            page: PageIndex(1),
            text: String::new(),
        });
        assert_eq!(
            actions,
            vec![
                Action::ShowPage(PageIndex(2)),
                Action::NeedText(PageIndex(2))
            ]
        );
    }

    #[test]
    fn the_end_of_the_document_finishes_rather_than_erroring() {
        let mut reading = Reading::new();
        reading.update(Event::Start {
            scope: Scope::Document,
            page: PageIndex(9),
        });
        reading.update(Event::TextArrived {
            page: PageIndex(9),
            text: "Last.".into(),
        });
        reading.update(Event::UtteranceFinished);
        let actions = reading.update(Event::NoSuchPage(PageIndex(10)));
        assert!(actions.contains(&Action::Finished));
        assert_eq!(reading.state(), SpeechState::Idle);
    }

    #[test]
    fn pause_stops_and_resume_repeats_the_same_sentence() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        reading.update(Event::UtteranceFinished); // on sentence 1
        assert_eq!(reading.progress(), Some((1, 3)));

        assert_eq!(reading.update(Event::Pause), vec![Action::StopSpeaking]);
        assert_eq!(reading.state(), SpeechState::Paused);

        let actions = reading.update(Event::Resume);
        assert!(actions.contains(&Action::Speak("Second sentence here.".into())));
        assert_eq!(reading.progress(), Some((1, 3)));
    }

    #[test]
    fn a_finish_that_races_a_pause_does_not_skip_a_sentence() {
        // The engine had already sent the whole utterance to the audio device
        // when the pause landed, so `UtteranceFinished` arrives *after* the
        // pause. Advancing on it would silently drop a sentence, and the
        // reader would only notice on resume.
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        reading.update(Event::Pause);
        let actions = reading.update(Event::UtteranceFinished);
        assert!(actions.is_empty());
        assert_eq!(reading.progress(), Some((0, 3)));

        let actions = reading.update(Event::Resume);
        assert!(actions.contains(&Action::Speak("First sentence here.".into())));
    }

    #[test]
    fn text_for_a_page_we_have_left_is_ignored() {
        let mut reading = Reading::new();
        reading.update(Event::Start {
            scope: Scope::Document,
            page: PageIndex(0),
        });
        reading.update(Event::TextArrived {
            page: PageIndex(0),
            text: "One.".into(),
        });
        reading.update(Event::UtteranceFinished); // now awaiting page 1
                                                  // A slow answer for page 0 arriving late must not reopen it.
        let actions = reading.update(Event::TextArrived {
            page: PageIndex(0),
            text: "One.".into(),
        });
        assert!(actions.is_empty());
        assert_eq!(reading.page(), Some(PageIndex(1)));
    }

    #[test]
    fn stopping_forgets_everything() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        assert_eq!(reading.update(Event::Stop), vec![Action::StopSpeaking]);
        assert_eq!(reading.state(), SpeechState::Idle);
        // Events after a stop are inert rather than reviving the session.
        assert!(reading.update(Event::UtteranceFinished).is_empty());
        assert!(reading.update(Event::Resume).is_empty());
        assert_eq!(reading.state(), SpeechState::Idle);
    }

    #[test]
    fn skipping_moves_a_sentence_at_a_time() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        let actions = reading.update(Event::Skip(Direction::Forward));
        assert!(actions.contains(&Action::Speak("Second sentence here.".into())));
        let actions = reading.update(Event::Skip(Direction::Back));
        assert!(actions.contains(&Action::Speak("First sentence here.".into())));
        // Back from the first stays put rather than changing page.
        let actions = reading.update(Event::Skip(Direction::Back));
        assert!(actions.contains(&Action::Speak("First sentence here.".into())));
        assert_eq!(reading.page(), Some(PageIndex(3)));
    }

    #[test]
    fn skipping_while_paused_moves_without_starting_to_talk() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        reading.update(Event::Pause);
        let actions = reading.update(Event::Skip(Direction::Forward));
        assert!(!actions.iter().any(|a| matches!(a, Action::Speak(_))));
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Highlight { .. })));
        assert_eq!(reading.state(), SpeechState::Paused);
        assert_eq!(reading.progress(), Some((1, 3)));
    }

    #[test]
    fn a_selection_is_read_once_and_never_highlighted() {
        let mut reading = Reading::new();
        let actions = reading.update(Event::StartSelection {
            text: "Just this. And this.".into(),
            page: PageIndex(2),
        });
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::Highlight { .. })));
        assert!(actions.contains(&Action::Speak("Just this.".into())));
        reading.update(Event::UtteranceFinished);
        let actions = reading.update(Event::UtteranceFinished);
        assert!(actions.contains(&Action::Finished));
    }

    #[test]
    fn an_empty_selection_finishes_instead_of_speaking_nothing() {
        let mut reading = Reading::new();
        let actions = reading.update(Event::StartSelection {
            text: "   \n ".into(),
            page: PageIndex(2),
        });
        assert!(actions.contains(&Action::Finished));
        assert_eq!(reading.state(), SpeechState::Idle);
    }

    #[test]
    fn an_engine_failure_ends_the_session() {
        let (mut reading, _) = started();
        reading.update(Event::TextArrived {
            page: PageIndex(3),
            text: PAGE.into(),
        });
        assert_eq!(
            reading.update(Event::EngineFailed),
            vec![Action::StopSpeaking]
        );
        assert_eq!(reading.state(), SpeechState::Idle);
    }

    #[test]
    fn pausing_while_the_page_is_still_loading_resumes_by_asking_again() {
        let (mut reading, _) = started();
        assert_eq!(reading.state(), SpeechState::AwaitingText(PageIndex(3)));
        reading.update(Event::Pause);
        assert_eq!(reading.state(), SpeechState::Paused);
        let actions = reading.update(Event::Resume);
        assert_eq!(actions, vec![Action::NeedText(PageIndex(3))]);
    }
}
