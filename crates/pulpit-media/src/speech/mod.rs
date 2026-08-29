//! Reading documents aloud (`docs-src/internals.typ`, issue #20).
//!
//! Speech belongs here for the reason the rest of this crate exists: it is a
//! heavy runtime that pulpit *launches* rather than links, driven from the
//! application over a boundary the application does not have to understand.
//! A browser renders an overlay, a synthesiser renders a sentence; both are
//! installed programs, both are supervised from here, and neither puts an
//! engine inside the presenter binary.
//!
//! This module owns everything about *producing* speech: which synthesiser to
//! run, which voices exist, what has been downloaded, how audio reaches the
//! speakers. It owns nothing about *what* to say or *when* — that is
//! `pulpit_core::speech`, which is pure and decides the reading order, the
//! sentence boundaries and the language. The two never depend on one another
//! beyond that: this module takes a string and gives back audio.
//!
//! It shares no types with the media half of the crate and needs none: the
//! two are neighbours under one roof rather than one mechanism. What they
//! share is the roof — the policy that a runtime pulpit does not link is
//! discovered, supervised and reported on from `pulpit-media`.
//!
//! The invariants worth stating once:
//!
//! * **Nothing is linked.** No synthesis engine, no inference runtime, no
//!   audio library. Both the synthesiser and the audio player are installed
//!   programs driven as child processes. That keeps the application's
//!   dependency surface unchanged, keeps a GPL synthesiser at arm's length
//!   from an MIT/Apache binary, and makes "stop" a `kill`.
//! * **Nothing is trusted.** Every downloaded artifact is verified against a
//!   sha256 pinned in the binary before it is used, and deleted if it does not
//!   match. Nothing appears under its final name until it has passed.
//! * **Nothing is claimed.** [`capability::Availability`] distinguishes "this
//!   session cannot speak" from "this session could, after a download", and
//!   the UI says which.
//! * **Sample rate travels with the audio.** It is a property of the voice,
//!   not of the engine, the quality tier or the platform; the shipped catalog
//!   contains three different rates and two voices of the same language and
//!   tier disagree.

pub mod capability;
pub mod catalog;
pub mod download;
pub mod engine;
pub mod sink;
pub mod speaker;
pub mod subprocess;

pub use capability::{human_bytes, Availability, Probe, ENGINE};
pub use catalog::{ArchiveKind, Catalog, EngineBuild, Quality, Store, Voice, VoiceFile};
pub use download::{install_engine, install_voice, Cancel, Progress};
pub use engine::{EngineStop, Pcm, SpeechEngine, SpeechError};
pub use sink::{Player, Sink};
pub use speaker::{Command, Event, Speaker};
pub use subprocess::{EngineManifest, SubprocessEngine};

/// Build the speaker this session can actually run, if it can run one.
///
/// One place that assembles engine, player and threads, so the application
/// does not have to know how they fit together — and so the failure to build
/// one is a single explicit answer rather than three separate `None`s at the
/// call site.
pub fn start(probe: &Probe, store: &Store) -> std::result::Result<Speaker, SpeechError> {
    let Some(program) = probe.program.clone() else {
        return Err(SpeechError::unsupported(
            "no speech engine is installed in this session",
        ));
    };
    let Some(sink) = probe.sink.clone() else {
        return Err(SpeechError::unsupported("this session has no audio output"));
    };
    let store = store.clone();
    let engine = SubprocessEngine::new(EngineManifest::piper(), program, move |voice| {
        store.model_path(voice)
    });
    Ok(Speaker::start(Box::new(engine), Player::new(sink)))
}
