//! Getting PCM to the speakers without linking an audio library.
//!
//! Every desktop already ships a command that plays a stream of samples. Using
//! it costs a process and buys three things worth more than the process: no
//! ALSA or CoreAudio symbol linked into the application, nothing new that can
//! fail to load at startup on a machine without the right shared object, and
//! a stop that is a `kill` — which is as immediate as stopping gets, and
//! immediacy is the requirement this feature is actually judged on.
//!
//! Two shapes of player exist and both are needed. On Linux the players read
//! raw samples from stdin, which is ideal. On macOS and Windows the available
//! player takes a *file*, so a WAV header is written around the samples and
//! the file is handed over. The WAV is temporary and one sentence long.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use super::engine::{Pcm, Result, SpeechError};

/// Silence prepended to every utterance, in milliseconds.
///
/// A player takes a moment to negotiate and start a stream, and whatever it
/// misses in that moment is lost from the front of the audio. Piper's output
/// begins essentially at once — measured, the first clearly audible sample is
/// 11 ms in — so there is no natural padding to absorb it, and the casualty
/// is the first word's opening consonant.
///
/// Paid per utterance, because there is a fresh player process per sentence.
/// That is not purely a cost: with prefetch keeping the next sentence ready,
/// the gap between sentences had fallen to nothing, and running them together
/// with no breath at all sounds worse than a short one.
const LEAD_IN_MS: u32 = 150;

/// `pcm` with [`LEAD_IN_MS`] of silence in front of it.
fn with_lead_in(pcm: &Pcm) -> Vec<u8> {
    // Two bytes per sample, and a whole number of samples so the frame
    // boundary is never split.
    let samples = (pcm.sample_rate as u64 * u64::from(LEAD_IN_MS) / 1000) as usize;
    let mut out = vec![0u8; samples * 2];
    out.extend_from_slice(&pcm.samples);
    out
}

/// How a player wants its audio.
///
/// Both variants are used, but never on the same platform: the candidate
/// table below is `cfg`-selected, so on Linux nothing constructs `WavFile` and
/// on macOS nothing constructs `RawStdin`. Deleting whichever one this build
/// does not use would delete the other platform's only way of playing sound.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Feed {
    /// Raw samples on stdin; the rate is passed as an argument.
    RawStdin,
    /// A WAV file named on the command line.
    WavFile,
}

/// A discovered audio player.
#[derive(Debug, Clone)]
pub struct Sink {
    program: PathBuf,
    /// Arguments with `{rate}` and `{file}` substituted per utterance.
    args: Vec<String>,
    feed: Feed,
}

/// Candidates in preference order, per platform.
///
/// Ordered by how modern the audio server is, not alphabetically: on a
/// PipeWire session `pw-play` is the direct route and `aplay` reaches it
/// through two compatibility layers.
#[cfg(all(unix, not(target_os = "macos")))]
const CANDIDATES: &[(&str, &[&str], Feed)] = &[
    (
        "pw-play",
        &[
            // `--raw` is not optional, and leaving it out is a refusal rather
            // than a degradation: without it the stream goes to libsndfile,
            // which looks for a header, does not find one, and exits with
            // "Format not recognised". The samples were always fine.
            "--raw",
            "--rate",
            "{rate}",
            "--format",
            "s16",
            "--channels",
            "1",
            "-",
        ],
        Feed::RawStdin,
    ),
    (
        "paplay",
        &["--raw", "--rate={rate}", "--format=s16le", "--channels=1"],
        Feed::RawStdin,
    ),
    (
        "aplay",
        &[
            "-q", "-t", "raw", "-f", "S16_LE", "-r", "{rate}", "-c", "1", "-",
        ],
        Feed::RawStdin,
    ),
];

#[cfg(target_os = "macos")]
const CANDIDATES: &[(&str, &[&str], Feed)] = &[("afplay", &["{file}"], Feed::WavFile)];

#[cfg(target_os = "windows")]
const CANDIDATES: &[(&str, &[&str], Feed)] = &[(
    "powershell",
    &[
        "-NoProfile",
        "-Command",
        "(New-Object Media.SoundPlayer '{file}').PlaySync()",
    ],
    Feed::WavFile,
)];

impl Sink {
    /// Find a player this session can use.
    ///
    /// `None` is a real answer — a headless session, a container with no
    /// audio — and it is reported as such rather than being papered over,
    /// because a speech control that silently does nothing is the outcome
    /// this whole design is trying to avoid.
    pub fn discover() -> Option<Sink> {
        let path = std::env::var_os("PATH")?;
        let directories: Vec<PathBuf> = std::env::split_paths(&path).collect();
        for (program, args, feed) in CANDIDATES {
            let executable = if cfg!(target_os = "windows") {
                format!("{program}.exe")
            } else {
                (*program).to_string()
            };
            if let Some(found) = directories
                .iter()
                .map(|directory| directory.join(&executable))
                .find(|candidate| candidate.is_file())
            {
                return Some(Sink {
                    program: found,
                    args: args.iter().map(|a| (*a).to_string()).collect(),
                    feed: *feed,
                });
            }
        }
        None
    }

    /// A sink whose player can never start, for tests that play only empty
    /// audio — which `Player::play` short-circuits before any spawn.
    ///
    /// This is what lets the speaker's threading tests run on a machine with
    /// no sound card at all, instead of skipping there — and a test that
    /// skips on exactly the machines CI runs on is a test in name only.
    #[cfg(test)]
    pub(crate) fn null_for_tests() -> Sink {
        Sink {
            program: PathBuf::from("/nonexistent/no-player"),
            args: Vec::new(),
            feed: Feed::RawStdin,
        }
    }

    /// A sink whose "playback" is creating `marker`, so a test can prove
    /// whether an utterance was actually played — which no amount of event
    /// inspection can, since stale events are filtered before a test sees
    /// them. Unix only, because it leans on an installed `touch`.
    #[cfg(all(test, unix))]
    pub(crate) fn touching_for_tests(marker: &std::path::Path) -> Option<Sink> {
        let path = std::env::var_os("PATH")?;
        let touch = std::env::split_paths(&path)
            .map(|directory| directory.join("touch"))
            .find(|candidate| candidate.is_file())?;
        Some(Sink {
            program: touch,
            args: vec![marker.to_string_lossy().to_string(), "{file}".to_string()],
            feed: Feed::WavFile,
        })
    }

    /// A name for the diagnostics bundle.
    pub fn label(&self) -> String {
        self.program
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into())
    }
}

/// Plays one utterance at a time, and can be stopped from another thread.
///
/// The child handle is shared rather than owned by the playing thread so that
/// `stop` does not have to wait for anything: it takes the lock, kills, and
/// returns. A stop that had to wait for the current sentence would be exactly
/// the "it doesn't stop when I press stop" complaint.
#[derive(Clone)]
pub struct Player {
    sink: Sink,
    current: Arc<Mutex<Option<Child>>>,
    /// Set by [`Player::stop`], cleared at the start of each playback.
    ///
    /// This is what tells a player we killed apart from one that died on its
    /// own, and the distinction is not cosmetic: a stop is silent by design,
    /// so treating a *failure* as a stop loses the only signal there was.
    /// That mistake once left speech wedged in "speaking" for ever with no
    /// sound and no message, because the player was rejecting every stream
    /// and the rejection was being read as "the reader pressed pause".
    stopping: Arc<std::sync::atomic::AtomicBool>,
}

impl Player {
    pub fn new(sink: Sink) -> Player {
        Player {
            sink,
            current: Arc::new(Mutex::new(None)),
            stopping: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn sink(&self) -> &Sink {
        &self.sink
    }

    /// Play `pcm`, returning when it has finished or been stopped.
    ///
    /// `Ok(false)` means it was stopped part-way, which is not a failure and
    /// must not be reported as one: it is what every pause and every page
    /// turn does.
    pub fn play(&self, pcm: &Pcm) -> Result<bool> {
        if pcm.is_empty() {
            return Ok(true);
        }
        // The interrupt flag is deliberately NOT cleared here. `play` cannot
        // know whether a set flag is leftover from the previous sentence or a
        // stop aimed at this one — only the caller can, because only the
        // caller knows this utterance's generation is still current. It says
        // so by calling [`Player::arm`] first. An earlier version cleared the
        // flag here, which made a stop pressed during synthesis evaporate the
        // moment playback began: the flag it had set was wiped by the very
        // utterance it was trying to prevent.
        //
        // Padded once, here, so both feeds get the same audio and the WAV
        // header describes what the file actually contains.
        let audio = with_lead_in(pcm);
        let temporary = match self.sink.feed {
            Feed::RawStdin => None,
            Feed::WavFile => Some(write_wav(&audio, pcm.sample_rate)?),
        };
        let arguments: Vec<String> = self
            .sink
            .args
            .iter()
            .map(|argument| {
                argument
                    .replace("{rate}", &pcm.sample_rate.to_string())
                    .replace(
                        "{file}",
                        &temporary
                            .as_ref()
                            .map(|path| path.to_string_lossy().to_string())
                            .unwrap_or_default(),
                    )
            })
            .collect();

        let mut command = Command::new(&self.sink.program);
        command
            .args(&arguments)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match self.sink.feed {
            Feed::RawStdin => {
                command.stdin(Stdio::piped());
            }
            Feed::WavFile => {
                command.stdin(Stdio::null());
            }
        }

        let mut child = command.spawn().map_err(|e| {
            SpeechError::failed(format!(
                "could not start {}: {e}",
                self.sink.program.to_string_lossy()
            ))
        })?;

        let mut stdin = child.stdin.take();
        {
            let mut slot = self
                .current
                .lock()
                .map_err(|_| SpeechError::failed("the player lock was poisoned"))?;
            // A stop that landed between `arm` and this registration set the
            // flag but found no child to kill. Registering and then checking,
            // under the same lock `stop` uses, closes that window: whichever
            // order the two ran in, the child ends up dead.
            *slot = Some(child);
            if self.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(mut child) = slot.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        let mut interrupted = false;
        if let Some(mut pipe) = stdin.take() {
            // A broken pipe here is the normal consequence of `stop`, not a
            // failure: the player was killed while we were still feeding it.
            if let Err(error) = pipe.write_all(&audio) {
                if error.kind() != std::io::ErrorKind::BrokenPipe {
                    self.stop();
                    return Err(SpeechError::failed(format!(
                        "writing to the player: {e}",
                        e = error
                    )));
                }
                interrupted = true;
            }
            drop(pipe);
        }

        // Polled with `try_wait`, releasing the lock between attempts, and
        // that is the whole point rather than a style choice.
        //
        // `wait` blocks until the child exits. Called while holding this
        // lock — which is the lock `stop` needs in order to reach the child —
        // it makes stopping impossible: the request queues behind the very
        // playback it is trying to end, so speech stops only when it was
        // going to stop anyway. Worse, on shutdown the player is left running
        // with a whole sentence already buffered, and the application exits
        // while its orphaned child talks on.
        let status = loop {
            {
                let mut slot = self
                    .current
                    .lock()
                    .map_err(|_| SpeechError::failed("the player lock was poisoned"))?;
                match slot.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => {
                            *slot = None;
                            break Some(status);
                        }
                        // Still playing: drop the lock and let `stop` in.
                        Ok(None) => {}
                        Err(_) => {
                            *slot = None;
                            break None;
                        }
                    },
                    // `stop` got here first and reaped it.
                    None => break None,
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // The slot is already cleared by whichever of the loop above or
        // `stop` got to the child first; nothing to tidy here.
        if let Some(path) = temporary {
            let _ = std::fs::remove_file(path);
        }

        // Three outcomes, and conflating the last two is what made a broken
        // player look like a paused one.
        let stopped = self.stopping.load(std::sync::atomic::Ordering::SeqCst);
        match status {
            // Played to the end.
            Some(status) if status.success() && !interrupted => Ok(true),
            // We killed it: a pause, a stop, a page turn. Silent by design.
            _ if stopped => Ok(false),
            // It died on its own. Saying so is the whole point: the reader
            // hears nothing either way, and only this branch can tell them
            // why.
            Some(status) => Err(SpeechError::failed(format!(
                "the audio player {} exited with {status}",
                self.sink.label()
            ))),
            None => Err(SpeechError::failed(format!(
                "the audio player {} could not be waited for",
                self.sink.label()
            ))),
        }
    }

    /// Stop immediately, dropping whatever is buffered.
    /// Clear the interrupt flag, because the next playback is legitimate.
    ///
    /// Called by the control loop immediately before playing an utterance it
    /// has verified is current. `play` itself must not do this — see the
    /// comment inside it.
    pub fn arm(&self) {
        self.stopping
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn stop(&self) {
        // Recorded before the kill, so the playing thread — which may be
        // inside `wait` right now — reads it as a stop rather than a crash.
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut slot) = self.current.lock() {
            if let Some(mut child) = slot.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// Wrap samples in a minimal WAV header for the file-based players.
fn write_wav(samples: &[u8], rate: u32) -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!("pulpit-speech-{}.wav", std::process::id()));
    let mut file = std::fs::File::create(&path)
        .map_err(|e| SpeechError::failed(format!("creating a temporary sound file: {e}")))?;
    file.write_all(&wav_header(samples.len(), rate))
        .and_then(|()| file.write_all(samples))
        .map_err(|e| SpeechError::failed(format!("writing a temporary sound file: {e}")))?;
    Ok(path)
}

/// A canonical 44-byte PCM WAV header.
fn wav_header(bytes: usize, rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;
    let data_len = bytes as u32;

    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&bits.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wav_header_declares_the_rate_the_audio_actually_has() {
        let header = wav_header(100, 44100);
        assert_eq!(header.len(), 44);
        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(&header[36..40], b"data");
        // Sample rate at offset 24, and the byte rate that follows it.
        assert_eq!(
            u32::from_le_bytes(header[24..28].try_into().unwrap()),
            44100
        );
        assert_eq!(
            u32::from_le_bytes(header[28..32].try_into().unwrap()),
            44100 * 2
        );
        assert_eq!(u32::from_le_bytes(header[40..44].try_into().unwrap()), 100);
    }

    #[test]
    fn every_utterance_gets_a_lead_in_so_the_first_word_survives() {
        // Piper starts speaking within about 11 ms, and a player loses the
        // front of a stream while it is starting one. Without this pad the
        // casualty is the opening consonant of the first word.
        let pcm = Pcm {
            samples: vec![7; 200],
            sample_rate: 22050,
        };
        let padded = with_lead_in(&pcm);

        let expected = (22050 * LEAD_IN_MS as usize / 1000) * 2;
        assert_eq!(padded.len(), expected + 200);
        assert!(
            padded[..expected].iter().all(|byte| *byte == 0),
            "the pad is silence"
        );
        assert_eq!(&padded[expected..], &pcm.samples[..], "and nothing is lost");
        // A whole number of samples, or every frame after it is off by a
        // byte and the audio becomes noise.
        assert_eq!(padded.len() % 2, 0);
    }

    #[test]
    fn the_lead_in_scales_with_the_voice_rate() {
        // A fixed byte count would be twice as long on a 16 kHz voice as on a
        // 44.1 kHz one; the pad is a duration, not a size.
        let millis = |rate: u32| {
            let pcm = Pcm {
                samples: Vec::new(),
                sample_rate: rate,
            };
            with_lead_in(&pcm).len() as u32 / 2 * 1000 / rate
        };
        // Within a millisecond: a rate that does not divide evenly leaves a
        // fractional sample, and rounding it down is right — a pad is a
        // floor, not an exact figure.
        for rate in [16000, 22050, 44100] {
            let actual = millis(rate);
            assert!(
                actual.abs_diff(LEAD_IN_MS) <= 1,
                "{rate} Hz padded {actual} ms, wanted about {LEAD_IN_MS}"
            );
        }
    }

    #[test]
    fn a_16khz_voice_gets_a_16khz_header() {
        // The catalog really does ship one, and playing it at 22050 would be
        // audibly wrong rather than subtly wrong.
        let header = wav_header(8, 16000);
        assert_eq!(
            u32::from_le_bytes(header[24..28].try_into().unwrap()),
            16000
        );
    }

    #[test]
    fn playing_nothing_succeeds_without_a_player() {
        let sink = Sink {
            program: PathBuf::from("/nonexistent/player"),
            args: Vec::new(),
            feed: Feed::RawStdin,
        };
        let player = Player::new(sink);
        let silence = Pcm {
            samples: Vec::new(),
            sample_rate: 22050,
        };
        assert_eq!(player.play(&silence), Ok(true));
    }

    #[test]
    fn a_missing_player_fails_by_name() {
        let sink = Sink {
            program: PathBuf::from("/nonexistent/player"),
            args: Vec::new(),
            feed: Feed::RawStdin,
        };
        let player = Player::new(sink);
        let pcm = Pcm {
            samples: vec![0; 32],
            sample_rate: 22050,
        };
        match player.play(&pcm) {
            Err(SpeechError::Failed(reason)) => assert!(reason.contains("player"), "{reason}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_player_that_dies_on_its_own_is_a_failure_not_a_pause() {
        // The regression that cost the most: a player rejecting every stream
        // exited non-zero, that was read as "the reader pressed stop", no
        // event was emitted, and speech sat in "speaking" for ever with no
        // sound and nothing to explain it. A stop is silent by design, so
        // only an *actual* stop may be silent.
        let sink = Sink {
            program: PathBuf::from("/nonexistent/player"),
            args: Vec::new(),
            feed: Feed::RawStdin,
        };
        let player = Player::new(sink);
        let pcm = Pcm {
            samples: vec![0; 32],
            sample_rate: 22050,
        };
        assert!(
            matches!(player.play(&pcm), Err(SpeechError::Failed(_))),
            "a player that will not start is reported, not swallowed"
        );
    }

    #[test]
    fn the_raw_flag_is_on_every_candidate_that_reads_a_stream() {
        // Without it, pw-play and paplay hand the samples to libsndfile,
        // which rejects them as headerless. The audio was never the problem;
        // the missing flag was, and it failed silently.
        for (program, args, feed) in CANDIDATES {
            if *feed != Feed::RawStdin {
                continue;
            }
            assert!(
                args.iter().any(|argument| argument.contains("raw")),
                "{program} is fed a raw stream and must say so"
            );
            assert!(
                args.iter().any(|argument| argument.contains("{rate}")),
                "{program} must be told the voice's sample rate"
            );
        }
    }

    /// `stop` must not wait for the playback it is ending.
    ///
    /// This is the bug that made stopping look broken and left an orphaned
    /// player talking after the window closed: `wait` was called while
    /// holding the lock that `stop` needs to reach the child, so a stop
    /// queued behind the very sentence it was trying to cut short. Everything
    /// looked right — the state machine went idle, the message was sent — and
    /// the room kept hearing the document.
    ///
    /// `sleep` stands in for a player: it is a child process that runs for a
    /// known time and ignores its input, which is all this needs.
    #[test]
    fn stop_does_not_wait_for_the_playback_it_ends() {
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file());
        let Some(sleep) = sleep else {
            eprintln!("skipping: no sleep binary to stand in for a player");
            return;
        };
        let player = Player::new(Sink {
            program: sleep,
            args: vec!["30".to_string()],
            feed: Feed::RawStdin,
        });

        let playing = player.clone();
        let handle = std::thread::spawn(move || {
            playing.play(&Pcm {
                samples: vec![0; 64],
                sample_rate: 22050,
            })
        });

        // Let the child get going and register itself.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let asked_at = std::time::Instant::now();
        player.stop();
        let took = asked_at.elapsed();

        assert!(
            took < std::time::Duration::from_secs(2),
            "stop took {took:?}; it waited for the playback instead of ending it"
        );
        let outcome = handle.join().expect("the playing thread finished");
        assert_eq!(
            outcome,
            Ok(false),
            "a stopped playback reports interrupted, not failed"
        );
    }

    #[test]
    fn stopping_when_nothing_plays_is_harmless() {
        let sink = Sink {
            program: PathBuf::from("/nonexistent/player"),
            args: Vec::new(),
            feed: Feed::RawStdin,
        };
        let player = Player::new(sink);
        player.stop();
        player.stop();
    }
}
