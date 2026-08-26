//! The engine boundary.
//!
//! Everything above this trait — the reading cursor, the language policy, the
//! settings, the download UI — is engine-agnostic. Everything below it is one
//! synthesiser's peculiarities. Two implementations exist because two is the
//! number that makes a boundary real: a subprocess engine driven by a
//! manifest, and (behind the same trait, when a session has one) the
//! platform's own speech service.
//!
//! ## Why a whole sentence at a time
//!
//! `synthesize` returns finished audio rather than a stream. That is a
//! measured decision, not a simplification: a sentence of speech is a couple
//! of seconds, which is on the order of a hundred kilobytes of PCM, and
//! holding one is free. What it buys is that the *frame boundary is not a
//! problem to solve* — a subprocess's stdout closing at EOF is the end of the
//! utterance, unambiguously, with no byte counting and no sentinel in an
//! undelimited byte stream. Streaming would reintroduce exactly that problem
//! in exchange for latency this design gets back another way.
//!
//! ## Where the latency goes instead
//!
//! Spawning a process per sentence costs its startup. That is hidden by
//! synthesising sentence *N+1* while sentence *N* is playing, which the
//! worker does. The reader hears the cost once, before the first sentence,
//! and never again.

use pulpit_core::speech::SpeechRate;

use super::catalog::Voice;

/// Finished audio for one utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    /// Signed 16-bit little-endian, mono.
    pub samples: Vec<u8>,
    /// Hertz. Comes from the voice, is carried with the audio, and is never
    /// assumed anywhere downstream.
    pub sample_rate: u32,
}

impl Pcm {
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// How long this will take to play. Used to decide whether there is time
    /// to prefetch the next sentence, and in tests.
    pub fn duration(&self) -> std::time::Duration {
        let frames = self.samples.len() as u64 / 2;
        let rate = self.sample_rate.max(1) as u64;
        std::time::Duration::from_micros(frames * 1_000_000 / rate)
    }
}

/// Why speech could not happen.
///
/// Mirrors the application's `Outcome` rather than importing it: this crate
/// does not depend on the application, and the four cases have to survive the
/// trip anyway because the user-visible consequence differs in each.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpeechError {
    /// Understood and declined — a voice that is not installed, a request
    /// while another is in flight.
    #[error("{0}")]
    Refused(String),
    /// Nothing in this session can do it: no engine, no audio output.
    #[error("{0}")]
    Unsupported(String),
    /// Attempted and failed.
    #[error("{0}")]
    Failed(String),
}

impl SpeechError {
    pub fn refused(reason: impl Into<String>) -> SpeechError {
        SpeechError::Refused(reason.into())
    }
    pub fn unsupported(reason: impl Into<String>) -> SpeechError {
        SpeechError::Unsupported(reason.into())
    }
    pub fn failed(reason: impl Into<String>) -> SpeechError {
        SpeechError::Failed(reason.into())
    }
}

pub type Result<T> = std::result::Result<T, SpeechError>;

/// Something that turns text into audio.
pub trait SpeechEngine: Send {
    /// A name for diagnostics.
    fn id(&self) -> &str;

    /// Synthesise one utterance. Blocking; the worker calls it off the
    /// event loop, on a thread that is allowed to take its time.
    fn synthesize(&mut self, text: &str, voice: &Voice, rate: SpeechRate) -> Result<Pcm>;

    /// The longest input this engine will accept in one call, in bytes, if it
    /// has a limit.
    ///
    /// Piper has none worth modelling; a model with a fixed token window does,
    /// and a legal sentence can exceed it. `None` means "hand it whatever a
    /// sentence turns out to be".
    fn input_limit(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_follows_the_rate_the_audio_declares() {
        // One second of 22050 Hz mono s16.
        let one_second = Pcm {
            samples: vec![0; 22050 * 2],
            sample_rate: 22050,
        };
        assert_eq!(one_second.duration().as_millis(), 1000);

        // The same bytes at 44100 Hz are half a second — which is exactly the
        // bug that sounds like chipmunk speech if the rate is assumed.
        let faster = Pcm {
            samples: one_second.samples.clone(),
            sample_rate: 44100,
        };
        assert_eq!(faster.duration().as_millis(), 500);
    }

    #[test]
    fn empty_audio_is_zero_length_rather_than_a_panic() {
        let nothing = Pcm {
            samples: Vec::new(),
            sample_rate: 0,
        };
        assert!(nothing.is_empty());
        assert_eq!(nothing.duration().as_millis(), 0);
    }
}
