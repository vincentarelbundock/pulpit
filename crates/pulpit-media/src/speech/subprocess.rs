//! Driving an installed synthesiser over a pipe.
//!
//! The engine is described by data — a program, an argument template, where
//! the sample rate comes from — so adding a different CLI synthesiser is a
//! manifest, not a new module. That is the insurance against the one risk
//! that is real here: not that a model is abandoned (a pinned `.onnx` keeps
//! working forever), but that the *program* that runs it is replaced by
//! something else.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use pulpit_core::speech::SpeechRate;
use serde::{Deserialize, Serialize};

use super::catalog::Voice;
use super::engine::{EngineStop, Pcm, Result, SpeechEngine, SpeechError};

/// One placeholder-substituted argument.
///
/// Substitution is whole-token, never string interpolation into a shell:
/// there is no shell in this path at all, so a voice id or a path containing
/// a space, a quote or a semicolon is inert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArgTemplate(String);

impl ArgTemplate {
    pub fn new(text: impl Into<String>) -> ArgTemplate {
        ArgTemplate(text.into())
    }

    fn render(&self, model: &str, length_scale: f32) -> String {
        self.0
            .replace("{model}", model)
            .replace("{length_scale}", &format!("{length_scale:.4}"))
    }
}

/// How to run a synthesiser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineManifest {
    pub id: String,
    /// Arguments, with `{model}` and `{length_scale}` substituted per call.
    pub args: Vec<ArgTemplate>,
}

impl EngineManifest {
    /// The manifest for piper, which is the one the catalog ships voices for.
    ///
    /// `--output-raw` streams signed 16-bit mono PCM to stdout, and stdout
    /// closing is the end of the utterance. That is the whole reason this
    /// engine needs no framing protocol.
    pub fn piper() -> EngineManifest {
        EngineManifest {
            id: "piper".into(),
            args: vec![
                ArgTemplate::new("--model"),
                ArgTemplate::new("{model}"),
                ArgTemplate::new("--length-scale"),
                ArgTemplate::new("{length_scale}"),
                ArgTemplate::new("--output-raw"),
            ],
        }
    }
}

/// A synthesiser run as a child process, one utterance at a time.
pub struct SubprocessEngine {
    manifest: EngineManifest,
    program: PathBuf,
    /// Resolves a voice to its model file. Injected so the engine does not
    /// need to know about the store's search-path rules.
    resolve: ResolveVoice,
    /// Shared with whoever shuts the speaker down, so a synthesiser that has
    /// stopped answering can be killed rather than waited for.
    stop: EngineStop,
}

/// Maps a voice to the model file on disk that an engine is pointed at.
///
/// Injected rather than looked up here, so the engine needs to know nothing
/// about the store's system-then-user search order.
type ResolveVoice = Box<dyn Fn(&Voice) -> Option<PathBuf> + Send>;

impl SubprocessEngine {
    pub fn new(
        manifest: EngineManifest,
        program: PathBuf,
        resolve: impl Fn(&Voice) -> Option<PathBuf> + Send + 'static,
    ) -> SubprocessEngine {
        SubprocessEngine {
            manifest,
            program,
            resolve: Box::new(resolve),
            stop: EngineStop::default(),
        }
    }

    /// Find an already-installed synthesiser on `PATH`.
    ///
    /// Checked before offering a download: a reader who has piper installed
    /// should not be asked to fetch a second copy of it.
    pub fn discover(program: &str) -> Option<PathBuf> {
        let executable = if cfg!(target_os = "windows") {
            format!("{program}.exe")
        } else {
            program.to_string()
        };
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|directory| directory.join(&executable))
            .find(|candidate| candidate.is_file())
    }
}

impl SpeechEngine for SubprocessEngine {
    fn id(&self) -> &str {
        &self.manifest.id
    }

    fn stopper(&self) -> EngineStop {
        self.stop.clone()
    }

    fn synthesize(&mut self, text: &str, voice: &Voice, rate: SpeechRate) -> Result<Pcm> {
        // A stop has already landed: whatever is still queued behind it is a
        // synthesiser this process would have to wait for on its way out.
        if self.stop.is_stopping() {
            return Err(SpeechError::refused("the engine has been stopped"));
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Pcm {
                samples: Vec::new(),
                sample_rate: voice.sample_rate,
            });
        }
        let model = (self.resolve)(voice).ok_or_else(|| {
            SpeechError::refused(format!("the voice {} is not installed", voice.label()))
        })?;
        let model = model.to_string_lossy().to_string();

        // Piper's `--length-scale` is seconds-per-unit, so it is the
        // reciprocal of a speaking rate: larger is slower. `SpeechRate`
        // converts, in one place, because inverting it by accident produces a
        // speed control that works backwards.
        let arguments: Vec<String> = self
            .manifest
            .args
            .iter()
            .map(|argument| argument.render(&model, rate.length_scale()))
            .collect();

        let mut child = Command::new(&self.program)
            .args(&arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited, so a synthesiser's complaints land in the log the
            // supervisor is already capturing. Never piped without being
            // drained: a full stderr pipe deadlocks the child mid-utterance.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                SpeechError::failed(format!(
                    "could not start {}: {e}",
                    self.program.to_string_lossy()
                ))
            })?;

        // Both pipes come out before the child is handed over, because from
        // here on the child belongs to the stop handle and this thread only
        // borrows it back at the end.
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();

        // Registered so `EngineStop::stop` can reach it. Everything after
        // this point blocks on a program that may never answer, and this is
        // what makes that bounded.
        if !self.stop.adopt(child) {
            return Err(SpeechError::refused("the engine has been stopped"));
        }
        // From here, `?` would leak the registration, so every exit goes
        // through this: reclaim what is left of the child and kill it.
        let abandon = |stop: &EngineStop| {
            if let Some(mut child) = stop.reclaim() {
                let _ = child.kill();
                let _ = child.wait();
            }
        };

        // The text goes in and the pipe closes: one line, one utterance, EOF
        // on stdout marks the end. Writing happens on this thread and reading
        // after it, which is safe only because a sentence is far smaller than
        // a pipe buffer's worth of *input*; the output is what needs draining.
        {
            let Some(mut stdin) = stdin else {
                abandon(&self.stop);
                return Err(SpeechError::failed("engine stdin closed"));
            };
            let line = format!("{}\n", trimmed.replace(['\r', '\n'], " "));
            if let Err(e) = stdin.write_all(line.as_bytes()) {
                abandon(&self.stop);
                return Err(SpeechError::failed(format!("writing to the engine: {e}")));
            }
        }

        let mut samples = Vec::new();
        if let Some(mut stdout) = stdout {
            if let Err(e) = stdout.read_to_end(&mut samples) {
                abandon(&self.stop);
                return Err(SpeechError::failed(format!("reading from the engine: {e}")));
            }
        }

        // Gone from the slot means a stop took it, killed it and is waiting
        // for this thread to notice. Saying so is what turns a kill into an
        // orderly end rather than a mysterious empty utterance.
        let Some(mut child) = self.stop.reclaim() else {
            return Err(SpeechError::refused("the engine was stopped mid-utterance"));
        };
        let status = child
            .wait()
            .map_err(|e| SpeechError::failed(format!("waiting for the engine: {e}")))?;
        if !status.success() {
            return Err(SpeechError::failed(format!(
                "the engine exited with {status}"
            )));
        }
        if samples.is_empty() {
            return Err(SpeechError::failed(
                "the engine produced no audio".to_string(),
            ));
        }
        // An odd byte count is a truncated final sample, which clicks. Drop
        // the stray byte rather than passing a malformed frame on.
        if samples.len() % 2 == 1 {
            samples.pop();
        }
        Ok(Pcm {
            samples,
            sample_rate: voice.sample_rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::catalog::Catalog;

    fn voice() -> Voice {
        Catalog::builtin()
            .voice("en_US-lessac-medium")
            .expect("shipped")
            .clone()
    }

    #[test]
    fn arguments_substitute_the_model_and_the_speed() {
        let manifest = EngineManifest::piper();
        let rendered: Vec<String> = manifest
            .args
            .iter()
            .map(|a| a.render("/voices/x.onnx", SpeechRate::new(2.0).length_scale()))
            .collect();
        assert!(rendered.contains(&"/voices/x.onnx".to_string()));
        // Twice as fast is half the length scale.
        assert!(rendered.contains(&"0.5000".to_string()));
        assert!(rendered.contains(&"--output-raw".to_string()));
    }

    #[test]
    fn a_path_with_shell_metacharacters_is_inert() {
        // There is no shell in this path; the substitution is whole-token.
        let argument = ArgTemplate::new("{model}");
        let rendered = argument.render("/voices/a b; rm -rf /.onnx", 1.0);
        assert_eq!(rendered, "/voices/a b; rm -rf /.onnx");
    }

    #[test]
    fn an_uninstalled_voice_is_refused_not_failed() {
        // The distinction matters: refused means "install it", failed means
        // "something is broken", and they get different UI.
        let mut engine = SubprocessEngine::new(
            EngineManifest::piper(),
            PathBuf::from("/nonexistent/piper"),
            |_| None,
        );
        let error = engine
            .synthesize("hello", &voice(), SpeechRate::NORMAL)
            .unwrap_err();
        assert!(matches!(error, SpeechError::Refused(_)), "got {error:?}");
    }

    #[test]
    fn a_missing_engine_binary_fails_with_its_name() {
        let mut engine = SubprocessEngine::new(
            EngineManifest::piper(),
            PathBuf::from("/nonexistent/piper"),
            |_| Some(PathBuf::from("/voices/x.onnx")),
        );
        let error = engine
            .synthesize("hello", &voice(), SpeechRate::NORMAL)
            .unwrap_err();
        match error {
            SpeechError::Failed(reason) => assert!(reason.contains("piper"), "got {reason}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn empty_text_is_silence_rather_than_a_spawn() {
        // Nothing to say is not an error, and must not cost a process.
        let mut engine = SubprocessEngine::new(
            EngineManifest::piper(),
            PathBuf::from("/nonexistent/piper"),
            |_| Some(PathBuf::from("/voices/x.onnx")),
        );
        let pcm = engine
            .synthesize("   \n ", &voice(), SpeechRate::NORMAL)
            .expect("silence is fine");
        assert!(pcm.is_empty());
        assert_eq!(pcm.sample_rate, voice().sample_rate);
    }

    /// A synthesiser that never produces a byte and never exits, which is
    /// what a wedged model load looks like from here.
    ///
    /// `exec`, because some shells (Ubuntu's dash among them) run the command
    /// as a child and stay: the kill then reaches only the shell, and the
    /// orphaned sleep holds the stdout pipe open for its full term. A real
    /// engine is launched directly, so the exec is what makes this stand-in
    /// die the way piper would.
    #[cfg(unix)]
    fn never_answers() -> SubprocessEngine {
        SubprocessEngine::new(
            EngineManifest {
                id: "sleeper".into(),
                args: vec![ArgTemplate::new("-c"), ArgTemplate::new("exec sleep 300")],
            },
            PathBuf::from("/bin/sh"),
            |_| Some(PathBuf::from("/voices/x.onnx")),
        )
    }

    #[test]
    #[cfg(unix)]
    fn stopping_ends_a_synthesiser_that_would_never_answer() {
        // Without this the shutdown join is unbounded: nothing else in the
        // process can reach the child, and `read_to_end` waits as long as it
        // takes.
        let mut engine = never_answers();
        let stop = engine.stopper();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            stop.stop();
        });
        let started = std::time::Instant::now();
        let error = engine
            .synthesize("hello", &voice(), SpeechRate::NORMAL)
            .unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the stop did not reach the child"
        );
        // Refused, not failed: nothing is broken, the reader is leaving.
        assert!(matches!(error, SpeechError::Refused(_)), "got {error:?}");
    }

    #[test]
    #[cfg(unix)]
    fn a_stopped_engine_does_not_start_another_synthesiser() {
        // The queue behind a stop is the second way to hold the join: every
        // request still in it would spawn a fresh child to wait for.
        let mut engine = never_answers();
        engine.stopper().stop();
        let started = std::time::Instant::now();
        let error = engine
            .synthesize("hello", &voice(), SpeechRate::NORMAL)
            .unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        assert!(matches!(error, SpeechError::Refused(_)), "got {error:?}");
    }

    #[test]
    fn discovery_finds_nothing_for_a_program_that_does_not_exist() {
        assert!(SubprocessEngine::discover("definitely-not-a-real-program-xyz").is_none());
    }
}
