//! Whether this session can speak, and if not, whether it could.
//!
//! The tri-state exists because "no" is two different answers with two
//! different remedies, and collapsing them produces the greyed-out control
//! that tells a reader nothing. A session with no audio player is a session
//! where speech is impossible; a session with a player and no voice installed
//! is one download away. The first is `Unavailable`, the second is
//! `Downloadable`, and the UI says so in words.
//!
//! This is the same argument `accessibility_bridge` already makes in
//! `Capabilities`: report what is actually true, including when the honest
//! answer is unwelcome.

use std::path::PathBuf;

use super::catalog::{Catalog, Store};
use super::sink::Sink;
use super::subprocess::SubprocessEngine;

/// The engine the shipped catalog speaks with.
pub const ENGINE: &str = "piper";

/// What speech can do here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Nothing can be done in this session. `why` is shown to the reader.
    Unavailable { why: String },
    /// Everything needed is present except the artifacts, which can be
    /// fetched. `bytes` is what the smallest useful download would cost, so
    /// the button can say what it will do before it does it.
    Downloadable { bytes: u64, needs_engine: bool },
    /// Ready to speak, with this many voices installed.
    Ready { voices: usize },
}

impl Availability {
    pub fn can_speak(&self) -> bool {
        matches!(self, Availability::Ready { .. })
    }

    /// One line for the settings page and the diagnostics bundle.
    pub fn summary(&self) -> String {
        match self {
            Availability::Unavailable { why } => format!("speech: unavailable — {why}"),
            Availability::Downloadable {
                bytes,
                needs_engine,
            } => {
                let what = if *needs_engine {
                    "a voice and the speech engine"
                } else {
                    "a voice"
                };
                format!(
                    "speech: available once {what} is downloaded ({})",
                    human_bytes(*bytes)
                )
            }
            Availability::Ready { voices } => {
                format!("speech: ready, {voices} voice{} installed", plural(*voices))
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// "63 MB", "1.2 GB". Decimal units, because that is what a download is
/// advertised in and what the reader will compare against.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", 1_000_000_000), ("MB", 1_000_000), ("kB", 1_000)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            let value = bytes as f64 / scale as f64;
            return if value < 10.0 {
                format!("{value:.1} {unit}")
            } else {
                format!("{value:.0} {unit}")
            };
        }
    }
    format!("{bytes} bytes")
}

/// What was found on this machine.
#[derive(Debug, Clone)]
pub struct Probe {
    pub availability: Availability,
    /// The synthesiser to run, if one was found.
    pub program: Option<PathBuf>,
    /// The audio player, if one was found.
    pub sink: Option<Sink>,
}

impl Probe {
    /// Look at the session and decide.
    ///
    /// Ordered so the *insurmountable* problem is reported first: a reader
    /// with no audio output should be told that, not offered a 63 MB download
    /// that will not help.
    pub fn run(catalog: &Catalog, store: &Store) -> Probe {
        let sink = Sink::discover();
        let Some(sink) = sink else {
            return Probe {
                availability: Availability::Unavailable {
                    why: "this session has no audio output".into(),
                },
                program: None,
                sink: None,
            };
        };

        // An installed copy first: a reader who already has the synthesiser
        // should never be asked to download a second one.
        let build = catalog.engine_for_host();
        let program = SubprocessEngine::discover(ENGINE)
            .or_else(|| build.and_then(|build| store.engine_program(ENGINE, build)));

        let installed = store.installed(catalog).len();
        let availability = match (&program, installed) {
            (Some(_), voices) if voices > 0 => Availability::Ready { voices },
            _ => {
                let Some(build) = build else {
                    return Probe {
                        availability: Availability::Unavailable {
                            why: format!(
                                "no speech engine is published for {}/{}",
                                super::catalog::host_os(),
                                super::catalog::host_arch()
                            ),
                        },
                        program: None,
                        sink: Some(sink),
                    };
                };
                let needs_engine = program.is_none();
                // The cheapest useful download: one voice, plus the engine if
                // it is not already there.
                let voice_bytes = catalog
                    .voices()
                    .iter()
                    .map(|voice| voice.bytes())
                    .min()
                    .unwrap_or(0);
                let bytes = if needs_engine {
                    build.bytes + voice_bytes
                } else {
                    voice_bytes
                };
                Availability::Downloadable {
                    bytes,
                    needs_engine,
                }
            }
        };

        Probe {
            availability,
            program,
            sink: Some(sink),
        }
    }

    /// Lines for the diagnostics bundle.
    pub fn report(&self) -> Vec<String> {
        let mut lines = vec![self.availability.summary()];
        lines.push(match &self.sink {
            Some(sink) => format!("speech audio: {}", sink.label()),
            None => "speech audio: none found".to_string(),
        });
        lines.push(match &self.program {
            Some(path) => format!("speech engine: {}", path.display()),
            None => "speech engine: not installed".to_string(),
        });
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_read_the_way_a_download_is_advertised() {
        assert_eq!(human_bytes(63_201_294), "63 MB");
        assert_eq!(human_bytes(5_024), "5.0 kB");
        assert_eq!(human_bytes(1_500_000_000), "1.5 GB");
        assert_eq!(human_bytes(12), "12 bytes");
    }

    #[test]
    fn no_audio_output_is_unavailable_rather_than_downloadable() {
        // The remedy for this is not a download, and offering one would waste
        // 63 MB of a stranger's conference wifi to no effect.
        let availability = Availability::Unavailable {
            why: "this session has no audio output".into(),
        };
        assert!(!availability.can_speak());
        assert!(availability.summary().contains("no audio output"));
    }

    #[test]
    fn downloadable_says_what_it_will_cost_and_whether_the_engine_is_included() {
        let with_engine = Availability::Downloadable {
            bytes: 90_000_000,
            needs_engine: true,
        };
        assert!(with_engine.summary().contains("engine"));
        assert!(with_engine.summary().contains("90 MB"));
        assert!(!with_engine.can_speak());

        let voice_only = Availability::Downloadable {
            bytes: 63_000_000,
            needs_engine: false,
        };
        assert!(!voice_only.summary().contains("engine"));
    }

    #[test]
    fn ready_counts_voices_and_says_so_in_english() {
        assert!(Availability::Ready { voices: 1 }
            .summary()
            .contains("1 voice "));
        assert!(Availability::Ready { voices: 3 }
            .summary()
            .contains("3 voices"));
        assert!(Availability::Ready { voices: 2 }.can_speak());
    }

    #[test]
    fn a_probe_on_a_bare_store_never_claims_to_be_ready() {
        // Whatever this machine has installed, an empty voice store cannot be
        // ready: readiness requires a voice, not just an engine.
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::under(temporary.path());
        let probe = Probe::run(&Catalog::builtin(), &store);
        assert!(!probe.availability.can_speak());
        assert_eq!(probe.report().len(), 3);
    }
}
