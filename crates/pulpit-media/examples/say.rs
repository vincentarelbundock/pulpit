//! Speak a line of text with an installed engine and voice.
//!
//! The development counterpart to `make launch`: it exercises the real
//! engine, the real voice files and the real audio path, which is the part a
//! unit test deliberately does not.
//!
//!     cargo run -p pulpit-media --example say -- "hello there"
//!
//! `PULPIT_SPEECH_DIR` points it at a voice store; `PULPIT_SPEECH_VOICE`
//! chooses a voice from it.

use pulpit_core::speech::{sentences, SpeechRate};
use pulpit_media::speech::{Catalog, Probe, Store};

fn main() {
    let text = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let text = if text.trim().is_empty() {
        "The reconciliation function is pure and idempotent. \
         Swapping the two screens is a role exchange, never an ad-hoc window move."
            .to_string()
    } else {
        text
    };

    let catalog = Catalog::builtin();
    let root = std::env::var_os("PULPIT_SPEECH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("pulpit-speech"));
    let store = Store::under(&root);

    let probe = Probe::run(&catalog, &store);
    for line in probe.report() {
        eprintln!("{line}");
    }

    let installed = store.installed(&catalog);
    if installed.is_empty() {
        eprintln!("\nNo voices installed under {}.", root.display());
        eprintln!("Install one there, or set PULPIT_SPEECH_DIR to a store that has one.");
        std::process::exit(1);
    }
    let wanted = std::env::var("PULPIT_SPEECH_VOICE").ok();
    let voice = wanted
        .as_deref()
        .and_then(|id| installed.iter().find(|voice| voice.id == id).copied())
        .unwrap_or(installed[0]);
    eprintln!("voice: {} ({} Hz)\n", voice.label(), voice.sample_rate);

    let speaker = match pulpit_media::speech::start(&probe, &store) {
        Ok(speaker) => speaker,
        Err(error) => {
            eprintln!("cannot speak: {error}");
            std::process::exit(1);
        }
    };

    // Sentence by sentence, prefetching the next one — the same shape the
    // application uses, so this exercises the prefetch path rather than
    // bypassing it.
    let units = sentences(&text);
    for (index, unit) in units.iter().enumerate() {
        let sentence = unit.text(&text);
        eprintln!("  [{}/{}] {sentence}", index + 1, units.len());
        speaker.send(pulpit_media::speech::Command::Speak {
            text: sentence.to_string(),
            // The next sentence travels with this one so it is synthesised
            // while this one plays; sent separately it would arrive after
            // playback, which is not a prefetch.
            next: units
                .get(index + 1)
                .map(|unit| unit.text(&text).to_string()),
            voice: Box::new(voice.clone()),
            rate: SpeechRate::NORMAL,
            // Overwritten by `send`.
            generation: 0,
        });
        // Wait for this sentence to finish before queueing the next, which is
        // what the reading cursor does.
        loop {
            let mut done = false;
            for event in speaker.drain() {
                match event {
                    pulpit_media::speech::Event::Finished => done = true,
                    pulpit_media::speech::Event::Failed(reason) => {
                        eprintln!("failed: {reason}");
                        std::process::exit(1);
                    }
                    pulpit_media::speech::Event::Started => {}
                }
            }
            if done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
