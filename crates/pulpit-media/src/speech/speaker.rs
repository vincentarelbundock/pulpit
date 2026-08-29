//! The speaking thread.
//!
//! ## Why a thread and not a worker process
//!
//! The renderer and the media runtimes get their own processes because they
//! link something heavy that can take the application down with it — PDFium,
//! a browser engine. Nothing here links anything. The synthesiser is an
//! installed program driven over a pipe and the audio player is another one,
//! so the isolation a worker process would buy is already present: if piper
//! segfaults, a child process this code spawned has died and the application
//! is untouched.
//!
//! `pulpit-media` states the rule this follows — a runtime that only
//! *launches* an installed program lives inside the application's own
//! executable, and only one that *links* an optional library needs a separate
//! binary. Speech is squarely the first kind, so it gets a thread, and the
//! blocking work is kept off the event loop that way instead.
//!
//! ## Two threads, because prefetch needs them
//!
//! One thread synthesises, one plays. That is what lets sentence *N+1* be
//! made while sentence *N* is audible, which is the whole reason speech does
//! not stutter between sentences at a one-to-two-second process start.
//! `stop` belongs to neither: it kills the player directly from the caller's
//! thread, so it never waits behind a queued command.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use pulpit_core::speech::SpeechRate;

use super::catalog::Voice;
use super::engine::{EngineStop, Pcm, SpeechEngine, SpeechError};
use super::sink::Player;

/// Identifies a synthesis request, so a prefetched result can be recognised.
///
/// The rate is part of the key: changing the speed mid-page must not replay a
/// sentence that was prefetched at the old one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    voice: String,
    /// Rate, quantised to avoid float keys. Thousandths are far finer than
    /// the control offers.
    rate_milli: u32,
}

impl Key {
    fn new(text: &str, voice: &Voice, rate: SpeechRate) -> Key {
        Key {
            text: text.to_string(),
            voice: voice.id.clone(),
            rate_milli: (rate.get() * 1000.0).round() as u32,
        }
    }
}

/// What the application asks the speaker to do.
#[derive(Debug, Clone)]
pub enum Command {
    Speak {
        text: String,
        /// The sentence after this one, so it can be synthesised *while* this
        /// one plays.
        ///
        /// Carried here rather than sent as its own command, and that is the
        /// whole point. A separate `Prefetch` message sits in the channel
        /// untouched while the control thread is blocked inside `play`, so it
        /// is only ever read once playback has finished — which is precisely
        /// too late to be a prefetch. Measured, that mistake cost about a
        /// second of dead air between every pair of sentences.
        next: Option<String>,
        voice: Box<Voice>,
        rate: SpeechRate,
        /// Stamped by [`Speaker::send`]; whatever a caller puts here is
        /// overwritten. An utterance whose generation is no longer current
        /// when its turn comes is discarded without a sound and without an
        /// event — it was stopped while it was still being made.
        generation: u64,
    },
    Shutdown,
}

/// What the speaker reports back.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Audio has begun. The reading cursor moves from `Starting` to
    /// `Speaking` on this.
    Started,
    /// The utterance played to its end.
    ///
    /// Deliberately *not* sent when playback was cut short by `stop`: the
    /// cursor advances on this event, and advancing after a stop is how a
    /// pause silently eats a sentence.
    Finished,
    /// Speech could not continue.
    Failed(String),
}

type Cache = Arc<Mutex<HashMap<Key, Result<Pcm, SpeechError>>>>;

/// Handle to a running speaker.
pub struct Speaker {
    commands: Sender<Command>,
    events: Receiver<(u64, Event)>,
    player: Player,
    /// The generation every stop advances and every utterance is stamped
    /// with. This is what makes a stop total rather than merely loud.
    ///
    /// Killing the player silences the sound that exists, but an utterance
    /// still being *synthesised* has no sound to kill — and without this, the
    /// control loop would go on to play it the moment synthesis finished,
    /// which is precisely "I pressed stop and it kept talking". The same
    /// counter kills stale events: a `Finished` already queued when a stop or
    /// skip lands must not advance whatever reading comes next, and `drain`
    /// discards anything stamped with an older generation.
    generation: Arc<std::sync::atomic::AtomicU64>,
    /// Reaches the synthesiser the way `player` reaches the audio player.
    /// Used on shutdown and nowhere else — see [`EngineStop`].
    engine: EngineStop,
    control: Option<JoinHandle<()>>,
    synth: Option<JoinHandle<()>>,
}

impl Speaker {
    /// Start the threads.
    pub fn start(engine: Box<dyn SpeechEngine>, player: Player) -> Speaker {
        let (commands, command_rx) = channel::<Command>();
        let (events, event_rx) = channel::<(u64, Event)>();
        let (synth_tx, synth_rx) = channel::<(Key, String, Box<Voice>, SpeechRate)>();
        let (ready_tx, ready_rx) = channel::<Key>();
        let cache: Cache = Arc::new(Mutex::new(HashMap::new()));
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        // Taken before the engine moves onto its thread: after that, the only
        // way to reach it is this handle.
        let engine_stop = engine.stopper();

        let synth = {
            let cache = Arc::clone(&cache);
            std::thread::Builder::new()
                .name("pulpit-synth".into())
                .spawn(move || synth_loop(engine, synth_rx, ready_tx, cache))
                .expect("spawning the synthesis thread")
        };

        let control = {
            let cache = Arc::clone(&cache);
            let player = player.clone();
            let generation = Arc::clone(&generation);
            std::thread::Builder::new()
                .name("pulpit-speech".into())
                .spawn(move || {
                    control_loop(
                        command_rx, events, synth_tx, ready_rx, cache, player, generation,
                    )
                })
                .expect("spawning the speech thread")
        };

        Speaker {
            commands,
            events: event_rx,
            player,
            generation,
            engine: engine_stop,
            control: Some(control),
            synth: Some(synth),
        }
    }

    pub fn send(&self, command: Command) {
        // Stamped here rather than by the caller, so a command can never
        // carry a generation other than the one current when it was sent —
        // which is the ordering the whole scheme rests on: the application
        // stops, then sends, and the send sees the post-stop generation.
        let command = match command {
            Command::Speak {
                text,
                next,
                voice,
                rate,
                ..
            } => Command::Speak {
                text,
                next,
                voice,
                rate,
                generation: self.generation.load(std::sync::atomic::Ordering::SeqCst),
            },
            other => other,
        };
        // A closed channel means the threads are gone, which only happens
        // during shutdown. Nothing useful can be done about it here.
        let _ = self.commands.send(command);
    }

    /// Stop immediately, and stop *everything*: the sound, the utterance
    /// still being synthesised, and every event already in flight.
    ///
    /// Does not go through the command channel on purpose: the control thread
    /// may be blocked inside `play`, and a stop that queued behind it would
    /// take effect at the end of the sentence rather than now. The order
    /// matters — the generation advances first, so that by the time the
    /// player dies, anything the control loop does next already sees its work
    /// as stale.
    pub fn stop(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.player.stop();
    }

    /// Events that have arrived, without blocking. Events from before the
    /// most recent stop are discarded here, unseen.
    ///
    /// Drained by the application's subscription each turn of the event loop.
    pub fn drain(&self) -> Vec<Event> {
        let current = self.generation.load(std::sync::atomic::Ordering::SeqCst);
        let mut out = Vec::new();
        // `Disconnected` ends the drain exactly as `Empty` does: the threads
        // are gone, which is shutdown, and there is nothing to report about
        // it that the caller can act on.
        while let Ok((generation, event)) = self.events.try_recv() {
            if generation == current {
                out.push(event);
            }
        }
        out
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.player.stop();
        // Both children die before anything is joined, and that order is the
        // whole point. `Command::Shutdown` is only read between utterances,
        // and the control thread may be parked inside `ready.recv()` waiting
        // on a synthesiser that has stopped answering — in which case the
        // join below never returns, and the window it is being run from can
        // never close. Killing the synthesiser is what ends that wait.
        self.engine.stop();
        let _ = self.commands.send(Command::Shutdown);
        // Joining matters: a synthesiser child left running would keep
        // talking after the window closed.
        if let Some(handle) = self.control.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.synth.take() {
            let _ = handle.join();
        }
    }
}

fn synth_loop(
    mut engine: Box<dyn SpeechEngine>,
    requests: Receiver<(Key, String, Box<Voice>, SpeechRate)>,
    ready: Sender<Key>,
    cache: Cache,
) {
    while let Ok((key, text, voice, rate)) = requests.recv() {
        // Already done — the same sentence was prefetched and then asked for.
        if cache.lock().map(|c| c.contains_key(&key)).unwrap_or(false) {
            let _ = ready.send(key);
            continue;
        }
        let result = engine.synthesize(&text, &voice, rate);
        if let Ok(mut cache) = cache.lock() {
            // A bound, so a long document cannot grow this without limit. The
            // only entries worth keeping are the one playing and the one
            // prefetched; anything else is a sentence already spoken.
            if cache.len() > 8 {
                cache.clear();
            }
            cache.insert(key.clone(), result);
        }
        let _ = ready.send(key);
    }
}

fn control_loop(
    commands: Receiver<Command>,
    events: Sender<(u64, Event)>,
    synth: Sender<(Key, String, Box<Voice>, SpeechRate)>,
    ready: Receiver<Key>,
    cache: Cache,
    player: Player,
    current: Arc<std::sync::atomic::AtomicU64>,
) {
    let is_current =
        |generation: u64| current.load(std::sync::atomic::Ordering::SeqCst) == generation;
    while let Ok(command) = commands.recv() {
        match command {
            Command::Shutdown => break,
            Command::Speak {
                text,
                next,
                voice,
                rate,
                generation,
            } => {
                // Already stale on arrival: a stop landed while this sat in
                // the queue. Not even worth synthesising.
                if !is_current(generation) {
                    continue;
                }
                let key = Key::new(&text, &voice, rate);
                let audio = take_or_synthesize(&key, &text, &voice, rate, &synth, &ready, &cache);

                // The next sentence goes to the synthesis thread *here*:
                // after this one is in hand, so the single synthesiser works
                // on them in the order they will be heard, and before `play`
                // blocks, so the work happens under the sound rather than
                // after it.
                if let Some(next) = next {
                    let next_key = Key::new(&next, &voice, rate);
                    let known = cache
                        .lock()
                        .map(|c| c.contains_key(&next_key))
                        .unwrap_or(false);
                    if !known {
                        let _ = synth.send((next_key, next, voice.clone(), rate));
                    }
                }

                // The check that makes stop total. Synthesis takes real time,
                // and a stop that lands during it has no sound to kill — the
                // only thing standing between the reader and the "I pressed
                // stop and it kept talking" bug is refusing, here, to play
                // audio for a generation that is over.
                if !is_current(generation) {
                    continue;
                }

                match audio {
                    Ok(pcm) => {
                        if events.send((generation, Event::Started)).is_err() {
                            break;
                        }
                        // Armed after the generation check: `stop` may have
                        // set the interrupt flag for the *previous* utterance,
                        // and this one — sent after that stop — must not
                        // inherit it.
                        player.arm();
                        match player.play(&pcm) {
                            // Played to the end: the cursor may advance.
                            Ok(true) => {
                                if events.send((generation, Event::Finished)).is_err() {
                                    break;
                                }
                            }
                            // Cut short by a stop. Silence is the correct
                            // report: the cursor already knows where it is.
                            Ok(false) => {}
                            Err(error) => {
                                if events
                                    .send((generation, Event::Failed(error.to_string())))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if events
                            .send((generation, Event::Failed(error.to_string())))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
    }
    player.stop();
}

/// Get the audio for `key`, waiting for a prefetch that is already running
/// rather than starting a second synthesis of the same sentence.
fn take_or_synthesize(
    key: &Key,
    text: &str,
    voice: &Voice,
    rate: SpeechRate,
    synth: &Sender<(Key, String, Box<Voice>, SpeechRate)>,
    ready: &Receiver<Key>,
    cache: &Cache,
) -> Result<Pcm, SpeechError> {
    if let Some(found) = cache.lock().ok().and_then(|mut c| c.remove(key)) {
        return found;
    }
    if synth
        .send((key.clone(), text.to_string(), Box::new(voice.clone()), rate))
        .is_err()
    {
        return Err(SpeechError::failed("the synthesis thread has stopped"));
    }
    // Results for *other* keys may arrive first — a prefetch that finished
    // after we asked for something else. They stay in the cache for later
    // rather than being discarded.
    loop {
        match ready.recv() {
            Ok(done) if done == *key => {
                return cache
                    .lock()
                    .ok()
                    .and_then(|mut c| c.remove(key))
                    .unwrap_or_else(|| Err(SpeechError::failed("the synthesis result was lost")));
            }
            Ok(_) => continue,
            Err(_) => return Err(SpeechError::failed("the synthesis thread has stopped")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::catalog::Catalog;
    use crate::speech::sink::Sink;

    fn voice() -> Voice {
        Catalog::builtin()
            .voice("en_US-lessac-medium")
            .expect("shipped")
            .clone()
    }

    /// An engine that answers instantly and records what it was asked.
    struct Recording {
        asked: Arc<Mutex<Vec<String>>>,
        fail: bool,
    }

    impl SpeechEngine for Recording {
        fn id(&self) -> &str {
            "recording"
        }
        fn synthesize(
            &mut self,
            text: &str,
            voice: &Voice,
            _rate: SpeechRate,
        ) -> crate::speech::engine::Result<Pcm> {
            self.asked.lock().unwrap().push(text.to_string());
            if self.fail {
                return Err(SpeechError::failed("no engine"));
            }
            Ok(Pcm {
                // Empty audio plays instantly and needs no sound card, which
                // is what makes this a unit test rather than a manual one.
                samples: Vec::new(),
                sample_rate: voice.sample_rate,
            })
        }
    }

    /// An engine that never answers until its stop handle is pulled, which
    /// is what a synthesiser wedged on a cold model load looks like.
    struct Wedged {
        stop: EngineStop,
        entered: Arc<Mutex<bool>>,
    }

    impl SpeechEngine for Wedged {
        fn id(&self) -> &str {
            "wedged"
        }
        fn stopper(&self) -> EngineStop {
            self.stop.clone()
        }
        fn synthesize(
            &mut self,
            _text: &str,
            _voice: &Voice,
            _rate: SpeechRate,
        ) -> crate::speech::engine::Result<Pcm> {
            *self.entered.lock().unwrap() = true;
            while !self.stop.is_stopping() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(SpeechError::refused("stopped"))
        }
    }

    #[test]
    fn dropping_the_speaker_ends_a_synthesis_that_is_still_running() {
        // The hang this exists to prevent: `Shutdown` is only read between
        // utterances, so a control thread parked inside synthesis would hold
        // the join in `Drop` for as long as the synthesiser felt like — with
        // the window up, unresponsive and impossible to close.
        let entered = Arc::new(Mutex::new(false));
        let engine = Wedged {
            stop: EngineStop::default(),
            entered: Arc::clone(&entered),
        };
        let speaker = Speaker::start(Box::new(engine), Player::new(Sink::null_for_tests()));
        speaker.send(Command::Speak {
            text: "one".into(),
            next: None,
            voice: Box::new(voice()),
            rate: SpeechRate::NORMAL,
            generation: 0,
        });
        // Wait until synthesis is genuinely under way, so the drop below is
        // the case under test rather than a race that passes by arriving
        // first.
        for _ in 0..400 {
            if *entered.lock().unwrap() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(*entered.lock().unwrap(), "synthesis never started");

        // Dropped on a scratch thread so the test reports a hang as a
        // failure rather than hanging with it.
        let (done, finished) = channel::<()>();
        std::thread::spawn(move || {
            drop(speaker);
            let _ = done.send(());
        });
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "dropping the speaker did not return: the join is still unbounded"
        );
    }

    fn key_of(text: &str) -> Key {
        Key::new(text, &voice(), SpeechRate::NORMAL)
    }

    #[test]
    fn the_rate_is_part_of_the_cache_key() {
        // Changing the speed must not replay audio made at the old speed.
        let slow = Key::new("hello", &voice(), SpeechRate::new(0.8));
        let fast = Key::new("hello", &voice(), SpeechRate::new(1.6));
        assert_ne!(slow, fast);
        assert_eq!(key_of("hello"), key_of("hello"));
        assert_ne!(key_of("hello"), key_of("goodbye"));
    }

    #[test]
    fn the_voice_is_part_of_the_cache_key() {
        let catalog = Catalog::builtin();
        let other = catalog.voice("en_GB-alan-medium").unwrap().clone();
        let a = Key::new("hello", &voice(), SpeechRate::NORMAL);
        let b = Key::new("hello", &other, SpeechRate::NORMAL);
        assert_ne!(a, b, "switching voices must re-synthesise");
    }

    #[test]
    fn speaking_reports_started_then_finished() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let engine = Box::new(Recording {
            asked: Arc::clone(&asked),
            fail: false,
        });
        // A null sink: every test utterance is empty audio, which never
        // reaches a player process, so these run on machines with no sound.
        let sink = Sink::null_for_tests();
        let speaker = Speaker::start(engine, Player::new(sink));
        speaker.send(Command::Speak {
            text: "One.".into(),
            next: None,
            voice: Box::new(voice()),
            rate: SpeechRate::NORMAL,
            // Overwritten by `send`; the value here is irrelevant.
            generation: 0,
        });

        let events = wait_for(&speaker, 2);
        assert_eq!(events, vec![Event::Started, Event::Finished]);
        assert_eq!(asked.lock().unwrap().as_slice(), ["One."]);
    }

    #[test]
    fn a_prefetched_sentence_is_not_synthesised_twice() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let engine = Box::new(Recording {
            asked: Arc::clone(&asked),
            fail: false,
        });
        // A null sink: every test utterance is empty audio, which never
        // reaches a player process, so these run on machines with no sound.
        let sink = Sink::null_for_tests();
        let speaker = Speaker::start(engine, Player::new(sink));
        // One sentence naming the next: the second is synthesised while the
        // first plays, and when it is asked for it is already in hand.
        speaker.send(Command::Speak {
            text: "One.".into(),
            next: Some("Two.".into()),
            voice: Box::new(voice()),
            rate: SpeechRate::NORMAL,
            // Overwritten by `send`; the value here is irrelevant.
            generation: 0,
        });
        let _ = wait_for(&speaker, 2);
        // Long enough for the prefetch to have completed under the playback.
        std::thread::sleep(std::time::Duration::from_millis(120));
        speaker.send(Command::Speak {
            text: "Two.".into(),
            next: None,
            voice: Box::new(voice()),
            rate: SpeechRate::NORMAL,
            // Overwritten by `send`; the value here is irrelevant.
            generation: 0,
        });
        let events = wait_for(&speaker, 4);
        assert!(events.contains(&Event::Finished));
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "each sentence was synthesised exactly once: {:?}",
            asked.lock().unwrap()
        );
    }

    /// An engine that takes real time, so a stop can land mid-synthesis.
    struct Slow {
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl SpeechEngine for Slow {
        fn id(&self) -> &str {
            "slow"
        }
        fn synthesize(
            &mut self,
            text: &str,
            voice: &Voice,
            _rate: SpeechRate,
        ) -> crate::speech::engine::Result<Pcm> {
            std::thread::sleep(std::time::Duration::from_millis(300));
            self.asked.lock().unwrap().push(text.to_string());
            Ok(Pcm {
                // Non-empty on purpose: empty audio short-circuits `play`
                // before any player is spawned, and the whole point of the
                // test using this engine is to see whether playback happens.
                samples: vec![0; 64],
                sample_rate: voice.sample_rate,
            })
        }
    }

    /// The race that shipped: a stop pressed while the sentence was still
    /// being *synthesised* had no sound to kill, and the utterance played in
    /// full the moment synthesis finished — with its events then advancing
    /// whatever reading came next. The generation check is what closes it,
    /// and this is the test that fails if anyone takes the check out.
    #[cfg(unix)]
    #[test]
    fn a_stop_during_synthesis_kills_the_utterance_and_its_events() {
        // The sink's "playback" is creating a marker file. That is the only
        // honest observable here: the event side of this bug is masked by
        // `drain`'s own filtering, so a test that watched events alone would
        // pass with the fix reverted — while the room kept hearing the
        // sentence. The marker is the sound.
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("played");
        let Some(sink) = Sink::touching_for_tests(&marker) else {
            eprintln!("no touch on this machine; skipping");
            return;
        };
        let asked = Arc::new(Mutex::new(Vec::new()));
        let engine = Box::new(Slow {
            asked: Arc::clone(&asked),
        });
        let speaker = Speaker::start(engine, Player::new(sink));
        speaker.send(Command::Speak {
            text: "Never heard.".into(),
            next: None,
            voice: Box::new(voice()),
            rate: SpeechRate::NORMAL,
            generation: 0,
        });
        // Land the stop inside the 300 ms synthesis window.
        std::thread::sleep(std::time::Duration::from_millis(80));
        speaker.stop();

        // Give the stale utterance every chance to misbehave, then look.
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(!marker.exists(), "the stopped utterance was played anyway");
        assert_eq!(
            speaker.drain(),
            Vec::new(),
            "a stopped utterance emits nothing — no Started, no Finished"
        );

        // And the speaker is not wedged: a fresh utterance still plays.
        speaker.send(Command::Speak {
            text: "Heard.".into(),
            next: None,
            voice: Box::new(voice()),
            rate: SpeechRate::NORMAL,
            generation: 0,
        });
        let events = wait_for(&speaker, 2);
        assert_eq!(events, vec![Event::Started, Event::Finished]);
        assert!(marker.exists(), "the fresh utterance really played");
    }

    #[test]
    fn an_engine_failure_is_reported_rather_than_silently_dropped() {
        let engine = Box::new(Recording {
            asked: Arc::new(Mutex::new(Vec::new())),
            fail: true,
        });
        // A null sink: every test utterance is empty audio, which never
        // reaches a player process, so these run on machines with no sound.
        let sink = Sink::null_for_tests();
        let speaker = Speaker::start(engine, Player::new(sink));
        speaker.send(Command::Speak {
            text: "Three.".into(),
            next: None,
            voice: Box::new(voice()),
            rate: SpeechRate::NORMAL,
            // Overwritten by `send`; the value here is irrelevant.
            generation: 0,
        });
        let events = wait_for(&speaker, 1);
        assert!(matches!(events.first(), Some(Event::Failed(_))));
    }

    fn wait_for(speaker: &Speaker, count: usize) -> Vec<Event> {
        let mut out = Vec::new();
        for _ in 0..200 {
            out.extend(speaker.drain());
            if out.len() >= count {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        out
    }
}
