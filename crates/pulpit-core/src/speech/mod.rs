//! Reading a document aloud: what to say, in what order, in which language.
//!
//! Everything here is pure. No audio device is opened, no engine is spawned,
//! no clock is read and no file is touched — those live in `pulpit-speech`,
//! behind a worker process, for the same reason rendering does. What is left
//! is the part that decides things, which is also the part with all the awkward
//! cases in it, and it is therefore the part that is tested exhaustively
//! without a sound card.
//!
//! Four pieces:
//!
//! * [`language`] — what language a run of text is in, and which of the
//!   installed voices can read it (RFC 4647 lookup).
//! * [`segment`] — splitting text into sentences, the unit that is
//!   synthesised, prefetched, paused between and highlighted.
//! * [`cursor`] — the reading session: a fold from events to actions.
//! * [`policy`] — the `Auto` language setting, including the hysteresis that
//!   stops one quoted line from flipping the voice, and the speaking rate.

pub mod cursor;
pub mod language;
pub mod policy;
pub mod segment;

pub use cursor::{Action, Direction, Event, Reading, Scope, SpeechState};
pub use language::{
    detect, lookup, Confidence, Detection, LanguageTag, MatchQuality, MIN_LETTERS_FOR_CONFIDENCE,
};
pub use policy::{LanguagePolicy, LanguageSetting, Resolution, SpeechRate, VoiceRef};
pub use segment::{sentences, split_long, Sentence};
