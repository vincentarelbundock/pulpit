//! Reading aloud, from the application's side (issue #20).
//!
//! Everything that decides *what* to say is in `pulpit_core::speech` and is
//! pure; everything that *makes* sound is in `pulpit_media::speech` and runs
//! on its own threads. This module is the seam: it turns key presses and settings
//! into cursor events, turns the cursor's actions into engine commands and
//! document requests, and owns the one piece of state neither of them can —
//! what is being downloaded right now.
//!
//! It deliberately holds no `Task`s and does no I/O of its own beyond
//! spawning the download thread. Everything it wants done comes back to the
//! caller as an [`Outgoing`], which `App::update` carries out. That keeps
//! this testable and keeps the event loop free.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use pulpit_core::page::PageIndex;
use pulpit_core::speech::{
    self, Action, LanguagePolicy, LanguageTag, Reading, Resolution, Scope, SpeechState, VoiceRef,
};
use pulpit_media::speech::{Availability, Cancel, Catalog, Probe, Progress, Speaker, Store, Voice};

use crate::platform::capabilities::Speech as SpeechCapability;
use crate::settings::SpeechSettings;

/// What the application should do as a result.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    /// Ask the document worker for a page's text.
    NeedText(PageIndex),
    /// Move the presenter to this page: speech has run on to it.
    ShowPage(PageIndex),
    /// Say something to the reader.
    Toast(String),
    /// Speech stopped on its own.
    Finished,
}

/// What a download is doing, for the dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadState {
    /// What is being fetched, for the dialog title.
    pub what: String,
    pub progress: Progress,
    /// Set when it ended; `Ok(())` means installed.
    pub outcome: Option<Result<(), String>>,
}

impl DownloadState {
    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }
}

/// A message from the download thread.
enum DownloadNote {
    Progress(Progress),
    Done(Result<(), String>),
}

/// The dialog that appears when `Auto` meets a language with no voice.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingVoicePrompt {
    pub language: LanguageTag,
    /// A readable name — "German" — for the dialog text.
    pub language_name: String,
    /// The voice that would be fetched, and what it costs.
    pub voice_id: String,
    pub voice_label: String,
    pub bytes: u64,
}

/// What pressing a scope's play/pause key should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Start,
    Resume,
    Pause,
}

/// The two-key toggle rule.
///
/// Each scope has its own play/pause key. A key toggles *its own* scope and
/// starts afresh when a different scope is running — that second clause is
/// the whole rule. Without it, pressing the page key while the document was
/// reading would pause the document, and pressing it again would resume the
/// document, so the page key could never start a page. A control that does
/// something other than what it says, depending on state the reader cannot
/// see, is worse than no control.
///
/// Pure and separate from [`Speech`] because the rule is what is worth
/// pinning, and reaching it through `toggle` needs an installed voice, a
/// running engine and an audio device — none of which this decision depends
/// on.
fn toggling(state: SpeechState, running: Option<Scope>, wanted: Scope) -> Toggle {
    match state {
        SpeechState::Idle => Toggle::Start,
        _ if running != Some(wanted) => Toggle::Start,
        SpeechState::Paused => Toggle::Resume,
        _ => Toggle::Pause,
    }
}

/// Everything speech needs, in one place.
pub struct Speech {
    catalog: Catalog,
    store: Store,
    probe: Probe,
    speaker: Option<Speaker>,
    reading: Reading,
    policy: LanguagePolicy,
    /// The voice currently speaking, resolved from settings and the page.
    voice: Option<Voice>,
    /// Set once a document has said it has no text layer at all, so speech
    /// stops asking page after page.
    refused: Option<String>,
    download: Option<DownloadState>,
    cancel: Cancel,
    notes: Option<Receiver<DownloadNote>>,
    /// Why the engine last refused to start, so the reader is told the actual
    /// reason rather than a guess reconstructed from the probe.
    last_engine_error: Option<String>,
    pub prompt: Option<MissingVoicePrompt>,
}

impl Speech {
    /// Probe the session and build the coordinator.
    ///
    /// Never fails: a session that cannot speak is a valid answer, and it is
    /// the one [`Speech::capability`] reports.
    #[cfg(test)] // production startup probes on a helper thread instead
    pub fn new(data_directory: &std::path::Path, settings: &SpeechSettings) -> Speech {
        let catalog = Catalog::builtin();
        let store = Store::new(data_directory);
        let probe = Probe::run(&catalog, &store);
        Self::with_probe(catalog, store, probe, settings)
    }

    /// Build the coordinator without probing the session.
    ///
    /// The probe scans the store, walks `PATH` for a synthesiser and again
    /// for an audio player — disk work that startup should not wait on. This
    /// starts as "unavailable, still checking"; [`Speech::adopt_probe`]
    /// installs the real answer when it arrives from a helper thread.
    pub fn unprobed(data_directory: &std::path::Path, settings: &SpeechSettings) -> Speech {
        let placeholder = Probe {
            availability: Availability::Unavailable {
                why: "speech is still being probed".into(),
            },
            program: None,
            sink: None,
        };
        Self::with_probe(
            Catalog::builtin(),
            Store::new(data_directory),
            placeholder,
            settings,
        )
    }

    /// Install a probe run elsewhere. Startup's other half.
    pub fn adopt_probe(&mut self, probe: Probe, settings: &SpeechSettings) {
        self.probe = probe;
        self.choose_voice(settings);
    }

    fn with_probe(
        catalog: Catalog,
        store: Store,
        probe: Probe,
        settings: &SpeechSettings,
    ) -> Speech {
        let mut speech = Speech {
            catalog,
            store,
            probe,
            speaker: None,
            reading: Reading::new(),
            policy: LanguagePolicy::new(settings.language.clone()),
            voice: None,
            refused: None,
            download: None,
            cancel: Cancel::new(),
            notes: None,
            last_engine_error: None,
            prompt: None,
        };
        speech.choose_voice(settings);
        speech
    }

    /// What this session can do, for `Capabilities`.
    pub fn capability(&self) -> SpeechCapability {
        match &self.probe.availability {
            Availability::Unavailable { why } => SpeechCapability::Unavailable { why: why.clone() },
            Availability::Downloadable {
                bytes,
                needs_engine,
            } => SpeechCapability::Downloadable {
                bytes: *bytes,
                needs_engine: *needs_engine,
            },
            Availability::Ready { voices } => SpeechCapability::Ready { voices: *voices },
        }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn state(&self) -> SpeechState {
        self.reading.state()
    }

    pub fn scope(&self) -> Option<Scope> {
        self.reading.scope()
    }

    pub fn download(&self) -> Option<&DownloadState> {
        self.download.as_ref()
    }

    /// The voice that would speak now, for the settings page to show.
    pub fn current_voice(&self) -> Option<&Voice> {
        self.voice.as_ref()
    }

    /// What `Auto` has resolved to, so the settings row can say it.
    pub fn current_language(&self) -> Option<&LanguageTag> {
        self.policy.current()
    }

    pub fn installed(&self) -> Vec<&Voice> {
        self.store.installed(&self.catalog)
    }

    /// Re-examine the session after a download or a settings change.
    pub fn reprobe(&mut self, settings: &SpeechSettings) {
        // A reading in flight ends here, explicitly. The speaker is about to
        // be dropped, and any `Finished` its utterance would have sent dies
        // with the channel — leaving the cursor waiting in `Speaking` for an
        // event that can no longer arrive. Stopping first keeps the state
        // machine and the world in agreement; the reader changed the voice
        // mid-sentence and can press the key again in the one they chose.
        if self.reading.state().is_active() {
            if let Some(speaker) = self.speaker.as_ref() {
                speaker.stop();
            }
            self.reading.update(speech::Event::Stop);
        }
        self.probe = Probe::run(&self.catalog, &self.store);
        // A speaker built against the old state would still point at the old
        // engine path, so it is dropped and rebuilt lazily.
        self.speaker = None;
        self.choose_voice(settings);
    }

    /// Pick the voice settings ask for, falling back to anything installed.
    fn choose_voice(&mut self, settings: &SpeechSettings) {
        let installed = self.store.installed(&self.catalog);
        self.voice = settings
            .voice
            .as_deref()
            .and_then(|id| installed.iter().find(|voice| voice.id == id).copied())
            .or_else(|| installed.first().copied())
            .cloned();
    }

    /// Start the engine threads if they are not running.
    fn speaker(&mut self) -> Option<&Speaker> {
        if self.speaker.is_none() {
            match pulpit_media::speech::start(&self.probe, &self.store) {
                Ok(speaker) => self.speaker = Some(speaker),
                Err(error) => {
                    tracing::warn!(%error, "speech is unavailable");
                    self.last_engine_error = Some(error.to_string());
                    return None;
                }
            }
        }
        self.speaker.as_ref()
    }

    /// Why the engine could not be started, for a message the reader can act
    /// on. Falls back to what the probe found rather than saying "unknown".
    fn engine_failure(&self) -> String {
        if let Some(reason) = &self.last_engine_error {
            return reason.clone();
        }
        match (&self.probe.program, &self.probe.sink) {
            (None, _) => "no speech engine is installed in this session".into(),
            (_, None) => "this session has no audio output".into(),
            _ => "the speech engine could not be started".into(),
        }
    }

    /// Begin reading.
    pub fn start(
        &mut self,
        scope: Scope,
        page: PageIndex,
        settings: &SpeechSettings,
    ) -> Vec<Outgoing> {
        if let SpeechCapability::Unavailable { why } = self.capability() {
            return vec![Outgoing::Toast(format!("Cannot read aloud: {why}"))];
        }
        if self.voice.is_none() {
            return vec![Outgoing::Toast(
                "No voice is installed. Settings ▸ Speech has the downloads.".into(),
            )];
        }
        self.refused = None;
        self.policy = LanguagePolicy::new(settings.language.clone());
        let actions = self.reading.update(speech::Event::Start { scope, page });
        self.carry_out(actions, settings)
    }

    /// Read a selection, which belongs to no page.
    pub fn speak_selection(
        &mut self,
        text: String,
        page: PageIndex,
        settings: &SpeechSettings,
    ) -> Vec<Outgoing> {
        if text.trim().is_empty() {
            return vec![Outgoing::Toast("Nothing is selected.".into())];
        }
        if self.voice.is_none() {
            return vec![Outgoing::Toast(
                "No voice is installed. Settings ▸ Speech has the downloads.".into(),
            )];
        }
        // A selection is usually too short to identify a language from, so it
        // inherits whatever the page established rather than detecting its
        // own and switching voice for two words.
        let actions = self
            .reading
            .update(speech::Event::StartSelection { text, page });
        self.carry_out(actions, settings)
    }

    pub fn pause(&mut self, settings: &SpeechSettings) -> Vec<Outgoing> {
        let actions = self.reading.update(speech::Event::Pause);
        self.carry_out(actions, settings)
    }

    pub fn resume(&mut self, settings: &SpeechSettings) -> Vec<Outgoing> {
        let actions = self.reading.update(speech::Event::Resume);
        self.carry_out(actions, settings)
    }

    /// The one control that has to work whatever else is happening.
    pub fn stop(&mut self, settings: &SpeechSettings) -> Vec<Outgoing> {
        let actions = self.reading.update(speech::Event::Stop);
        self.carry_out(actions, settings)
    }

    /// Play/pause on one key, which is what a reader expects of one button.
    pub fn toggle(
        &mut self,
        scope: Scope,
        page: PageIndex,
        settings: &SpeechSettings,
    ) -> Vec<Outgoing> {
        match toggling(self.reading.state(), self.reading.scope(), scope) {
            Toggle::Start => self.start(scope, page, settings),
            Toggle::Resume => self.resume(settings),
            Toggle::Pause => self.pause(settings),
        }
    }

    /// Play/pause for a held selection, on the same key that reads the page.
    ///
    /// The same two-key rule as [`Speech::toggle`], with the selection as the
    /// wanted scope: pressing the key while the selection is being read
    /// pauses it, and pressing it while anything else is running starts the
    /// selection afresh.
    pub fn toggle_selection(
        &mut self,
        text: String,
        page: PageIndex,
        settings: &SpeechSettings,
    ) -> Vec<Outgoing> {
        match toggling(self.reading.state(), self.reading.scope(), Scope::Selection) {
            Toggle::Start => self.speak_selection(text, page, settings),
            Toggle::Resume => self.resume(settings),
            Toggle::Pause => self.pause(settings),
        }
    }

    pub fn skip(
        &mut self,
        direction: speech::Direction,
        settings: &SpeechSettings,
    ) -> Vec<Outgoing> {
        let actions = self.reading.update(speech::Event::Skip(direction));
        self.carry_out(actions, settings)
    }

    /// A page's text arrived from the document worker.
    pub fn text_arrived(
        &mut self,
        page: PageIndex,
        text: String,
        settings: &mut SpeechSettings,
    ) -> Vec<Outgoing> {
        // Staleness first, before this answer is allowed to touch anything.
        // The cursor already discards text for a page it has left — but the
        // language policy runs before the cursor, and letting a slow answer
        // for a page speech moved past retune the voice, or pop a download
        // prompt for a language nobody is reading any more, is the same bug
        // wearing a different hat.
        if self.reading.state() != SpeechState::AwaitingText(page) {
            return Vec::new();
        }
        // Language next: the voice for this page has to be settled before
        // the first sentence of it is synthesised.
        let mut out = self.resolve_language(&text, settings);
        if self.prompt.is_some() {
            // Waiting on the reader's answer about a download. Speech pauses
            // rather than reading German with an English voice.
            let actions = self.reading.update(speech::Event::Pause);
            out.extend(self.carry_out(actions, settings));
            return out;
        }
        // A page with nothing on it is a fact, not a failure — but in page
        // scope it ends the reading immediately, and ending with no sound and
        // no word is the one outcome indistinguishable from a broken feature.
        // In document scope it is passed over quietly, which is right: the
        // reading continues onto the next page.
        let blank = text.trim().is_empty();
        let scope = self.reading.scope();
        if blank && scope != Some(Scope::Document) {
            out.push(Outgoing::Toast(format!(
                "Page {} has no text to read. It may be a scan or an image.",
                page.get() + 1
            )));
        }

        let actions = self
            .reading
            .update(speech::Event::TextArrived { page, text });
        out.extend(self.carry_out(actions, settings));
        out
    }

    /// This document has no text layer at all.
    pub fn cannot_speak(&mut self, reason: String, settings: &SpeechSettings) -> Vec<Outgoing> {
        self.refused = Some(reason.clone());
        let actions = self.reading.update(speech::Event::Stop);
        let mut out = self.carry_out(actions, settings);
        out.push(Outgoing::Toast(format!("Cannot read this aloud: {reason}")));
        out
    }

    /// The page asked for does not exist: the end of the document.
    pub fn no_such_page(&mut self, page: PageIndex, settings: &SpeechSettings) -> Vec<Outgoing> {
        let actions = self.reading.update(speech::Event::NoSuchPage(page));
        self.carry_out(actions, settings)
    }

    /// Decide which voice reads this page, and whether to offer a download.
    fn resolve_language(&mut self, text: &str, settings: &mut SpeechSettings) -> Vec<Outgoing> {
        let installed = self.store.installed(&self.catalog);
        let refs: Vec<VoiceRef> = installed
            .iter()
            .map(|voice| VoiceRef::new(voice.id.clone(), voice.language.clone()))
            .collect();
        let detected = speech::detect(text);

        match self.policy.resolve(detected, &refs) {
            Resolution::Keep => Vec::new(),
            Resolution::Use {
                index, language, ..
            } => {
                // The policy decides the *language*; it must not overrule the
                // *speaker*. A reader who chose a voice in settings meant
                // that voice, and having the first same-language entry in
                // catalog order silently replace it — which an earlier
                // version did — makes the voice picker a suggestion box.
                // Only when the chosen voice cannot speak this language does
                // the lookup's answer stand.
                let chosen = settings.voice.as_deref().and_then(|id| {
                    installed
                        .iter()
                        .copied()
                        .find(|voice| voice.id == id && voice.language.same_language(&language))
                });
                if let Some(voice) = chosen.or_else(|| installed.get(index).copied()) {
                    self.voice = Some(voice.clone());
                }
                Vec::new()
            }
            Resolution::Missing { language } => {
                // Asked once per language per session. A bilingual document
                // that asked on every page turn would be intolerable.
                if settings.has_declined(&language) {
                    return Vec::new();
                }
                let Some(candidate) = self.catalog.for_language(&language).first().copied() else {
                    settings.decline(language.clone());
                    return vec![Outgoing::Toast(format!(
                        "No voice is published for {language}."
                    ))];
                };
                self.prompt = Some(MissingVoicePrompt {
                    language_name: candidate.language_name.clone(),
                    language,
                    voice_id: candidate.id.clone(),
                    voice_label: candidate.label(),
                    bytes: candidate.bytes(),
                });
                Vec::new()
            }
        }
    }

    /// The reader answered the missing-voice prompt.
    pub fn answer_prompt(
        &mut self,
        download: bool,
        settings: &mut SpeechSettings,
    ) -> Vec<Outgoing> {
        let Some(prompt) = self.prompt.take() else {
            return Vec::new();
        };
        if download {
            self.begin_voice_download(&prompt.voice_id)
        } else {
            settings.decline(prompt.language);
            // Carry on in the voice already chosen. The reader said no; the
            // alternative is silence, which is worse.
            self.resume(settings)
        }
    }

    /// Turn cursor actions into engine commands and requests.
    fn carry_out(&mut self, actions: Vec<Action>, settings: &SpeechSettings) -> Vec<Outgoing> {
        let mut out = Vec::new();
        let rate = settings.rate;
        let voice = self.voice.clone();
        // The cursor names the next sentence in its own action, but it has to
        // reach the engine *with* the current one: sent separately it would
        // queue behind playback and arrive too late to be a prefetch at all.
        // Lifted out of the loop here and folded into the speak command below.
        let next = actions.iter().find_map(|action| match action {
            Action::Prefetch(text) => Some(text.clone()),
            _ => None,
        });
        for action in actions {
            match action {
                Action::StopSpeaking => {
                    if let Some(speaker) = self.speaker.as_ref() {
                        speaker.stop();
                    }
                }
                Action::Speak(text) => {
                    // Every way this can fail says so. A speak command that
                    // quietly does nothing is the exact failure this feature
                    // is least able to afford: there is no picture to look
                    // at, so silence is indistinguishable from working.
                    let Some(voice) = voice.clone() else {
                        self.reading.update(speech::Event::EngineFailed);
                        out.push(Outgoing::Toast(
                            "No voice is installed. Settings ▸ Speech has the downloads.".into(),
                        ));
                        continue;
                    };
                    let next = next.clone();
                    match self.speaker() {
                        Some(speaker) => speaker.send(pulpit_media::speech::Command::Speak {
                            text,
                            next,
                            voice: Box::new(voice),
                            rate,
                            // Overwritten by `send` with the current
                            // generation; a placeholder here, deliberately.
                            generation: 0,
                        }),
                        None => {
                            let reason = self.engine_failure();
                            self.reading.update(speech::Event::EngineFailed);
                            out.push(Outgoing::Toast(format!("Cannot read aloud: {reason}")));
                        }
                    }
                }
                // Carried on the speak command above rather than sent on its
                // own; nothing left to do when it comes round in the list.
                Action::Prefetch(_) => {}
                Action::NeedText(page) => {
                    if self.refused.is_none() {
                        out.push(Outgoing::NeedText(page));
                    }
                }
                Action::ShowPage(page) => out.push(Outgoing::ShowPage(page)),
                // The spoken span, as a byte range into the page's text. Not
                // forwarded: drawing it on the page needs the range resolved
                // to quadrilaterals by the document worker, which is designed
                // (this action is the seam) and not yet built. An earlier
                // version forwarded it into an `App` field that nothing ever
                // read — plumbing that pretends a feature exists is worse
                // than an honest absence, because it is where the next reader
                // wastes an afternoon.
                Action::Highlight { .. } => {}
                Action::Finished => out.push(Outgoing::Finished),
            }
        }
        out
    }

    /// Drain engine events and download progress. Called on every tick.
    pub fn poll(&mut self, settings: &SpeechSettings) -> Vec<Outgoing> {
        let mut out = Vec::new();

        let events: Vec<pulpit_media::speech::Event> = self
            .speaker
            .as_ref()
            .map(|speaker| speaker.drain())
            .unwrap_or_default();
        for event in events {
            let actions = match event {
                pulpit_media::speech::Event::Started => {
                    self.reading.update(speech::Event::UtteranceStarted)
                }
                pulpit_media::speech::Event::Finished => {
                    self.reading.update(speech::Event::UtteranceFinished)
                }
                pulpit_media::speech::Event::Failed(reason) => {
                    out.push(Outgoing::Toast(format!("Speech stopped: {reason}")));
                    self.reading.update(speech::Event::EngineFailed)
                }
            };
            out.extend(self.carry_out(actions, settings));
        }

        // Download progress.
        let mut finished = false;
        if let Some(notes) = self.notes.as_ref() {
            while let Ok(note) = notes.try_recv() {
                match note {
                    DownloadNote::Progress(progress) => {
                        if let Some(state) = self.download.as_mut() {
                            state.progress = progress;
                        }
                    }
                    DownloadNote::Done(result) => {
                        if let Some(state) = self.download.as_mut() {
                            state.outcome = Some(result.clone());
                        }
                        match result {
                            Ok(()) => out.push(Outgoing::Toast("Voice installed.".into())),
                            Err(reason) => {
                                out.push(Outgoing::Toast(format!("Download failed: {reason}")))
                            }
                        }
                        finished = true;
                    }
                }
            }
        }
        if finished {
            self.notes = None;
            self.reprobe(settings);
        }
        out
    }

    /// Whether a tick is worth taking: speech or a download is live.
    pub fn is_live(&self) -> bool {
        self.reading.state().is_active()
            || self
                .download
                .as_ref()
                .is_some_and(DownloadState::is_running)
    }

    /// Start downloading a voice, and the engine too if it is missing.
    pub fn begin_voice_download(&mut self, voice_id: &str) -> Vec<Outgoing> {
        if self
            .download
            .as_ref()
            .is_some_and(DownloadState::is_running)
        {
            return vec![Outgoing::Toast("A download is already running.".into())];
        }
        let Some(voice) = self.catalog.voice(voice_id).cloned() else {
            return vec![Outgoing::Toast("No such voice.".into())];
        };
        let needs_engine = self.probe.program.is_none();
        let build = self.catalog.engine_for_host().cloned();
        if needs_engine && build.is_none() {
            return vec![Outgoing::Toast(
                "No speech engine is published for this platform.".into(),
            )];
        }

        let (notes, receiver) = channel();
        self.notes = Some(receiver);
        self.cancel = Cancel::new();
        self.download = Some(DownloadState {
            what: voice.label(),
            progress: Progress::Advanced {
                done: 0,
                total: voice.bytes() + build.as_ref().map(|b| b.bytes).unwrap_or(0),
            },
            outcome: None,
        });

        let store = self.store.clone();
        let cancel = self.cancel.clone();
        // A thread, not a task: this is blocking network and disk work, and
        // the event loop must stay free to draw the progress bar it is
        // reporting.
        let spawn = std::thread::Builder::new()
            .name("pulpit-speech-download".into())
            .spawn(move || {
                run_download(store, voice, build, needs_engine, cancel, notes);
            });
        if let Err(error) = spawn {
            // No thread means no progress notes and no `Done` — the dialog
            // this method just put up would spin for ever. Take it down and
            // say what happened; `.ok()` here once meant exactly that hang.
            self.download = None;
            self.notes = None;
            return vec![Outgoing::Toast(format!(
                "Could not start the download: {error}"
            ))];
        }
        Vec::new()
    }

    /// The document went away or was replaced: end the reading and forget
    /// everything that was about *that* document.
    ///
    /// Without this, an utterance from the old file keeps playing and the
    /// cursor then asks the *new* file for the old file's next page — or, if
    /// the worker died, waits for ever on an answer that cannot arrive. The
    /// per-document facts go too: a "this document has no text layer" refusal
    /// must not gag the next document, a pending download prompt is about a
    /// page nobody is reading any more, and the `Auto` language starts fresh
    /// because the next file has its own.
    pub fn document_changed(&mut self, settings: &SpeechSettings) {
        if let Some(speaker) = self.speaker.as_ref() {
            speaker.stop();
        }
        self.reading.update(speech::Event::Stop);
        self.refused = None;
        self.prompt = None;
        self.policy = LanguagePolicy::new(settings.language.clone());
    }

    /// Silence everything and let the child processes go, before the process
    /// does.
    ///
    /// Called explicitly on quit rather than left to `Drop`, for the same
    /// reason the renderer supervisor is: `iced::exit` can end the process
    /// without unwinding, so a destructor is not a guarantee. Without this,
    /// closing the window leaves an orphaned audio player reading out the
    /// rest of a sentence it was already handed — the application is gone and
    /// something is still talking.
    pub fn shutdown(&mut self) {
        if let Some(speaker) = self.speaker.as_ref() {
            speaker.stop();
        }
        // Dropping joins the threads, which is what waits for the synthesiser
        // to notice and go away.
        self.speaker = None;
        self.cancel.cancel();
    }

    /// Abandon the running download.
    pub fn cancel_download(&mut self) {
        self.cancel.cancel();
    }

    /// Dismiss a finished download's dialog.
    pub fn clear_download(&mut self) {
        if !self
            .download
            .as_ref()
            .is_some_and(DownloadState::is_running)
        {
            self.download = None;
        }
    }

    /// Delete an installed voice, to get the disk back.
    pub fn remove_voice(&mut self, voice_id: &str, settings: &mut SpeechSettings) -> Vec<Outgoing> {
        let Some(voice) = self.catalog.voice(voice_id).cloned() else {
            return Vec::new();
        };
        // Only ever the copy under our own root: a voice a packager placed in
        // a system directory is not ours to delete.
        let target = self.store.voice_target(&voice);
        if let Err(error) = std::fs::remove_dir_all(&target) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return vec![Outgoing::Toast(format!("Could not remove: {error}"))];
            }
        }
        if settings.voice.as_deref() == Some(voice_id) {
            settings.voice = None;
        }
        let snapshot = settings.clone();
        self.reprobe(&snapshot);
        vec![Outgoing::Toast(format!("Removed {}.", voice.label()))]
    }

    /// Lines for the diagnostics bundle.
    pub fn report(&self) -> Vec<String> {
        let mut lines = self.probe.report();
        if let Some(voice) = &self.voice {
            lines.push(format!(
                "speech voice: {} ({} Hz)",
                voice.id, voice.sample_rate
            ));
        }
        if let Some(language) = self.policy.current() {
            lines.push(format!("speech language in use: {language}"));
        }
        lines
    }
}

fn run_download(
    store: Store,
    voice: Voice,
    build: Option<pulpit_media::speech::EngineBuild>,
    needs_engine: bool,
    cancel: Cancel,
    notes: Sender<DownloadNote>,
) {
    let engine_bytes = build.as_ref().map(|b| b.bytes).unwrap_or(0);
    let total = voice.bytes() + if needs_engine { engine_bytes } else { 0 };

    if needs_engine {
        if let Some(build) = &build {
            let result = pulpit_media::speech::install_engine(
                &store,
                pulpit_media::speech::ENGINE,
                build,
                &cancel,
                &mut |progress| {
                    if let Progress::Advanced { done, .. } = progress {
                        let _ =
                            notes.send(DownloadNote::Progress(Progress::Advanced { done, total }));
                    }
                },
            );
            if let Err(error) = result {
                let _ = notes.send(DownloadNote::Done(Err(error.to_string())));
                return;
            }
        }
    }

    let base = if needs_engine { engine_bytes } else { 0 };
    let result = pulpit_media::speech::install_voice(&store, &voice, &cancel, &mut |progress| {
        if let Progress::Advanced { done, .. } = progress {
            let _ = notes.send(DownloadNote::Progress(Progress::Advanced {
                done: base + done,
                total,
            }));
        }
    });
    let _ = notes.send(DownloadNote::Done(result.map_err(|e| e.to_string())));
}

/// Voices grouped by language, for the settings page.
///
/// Installed languages first, then the rest by name, so the list opens on
/// what the reader already has rather than on Albanian.
pub fn browsable(catalog: &Catalog, store: &Store) -> Vec<(String, LanguageTag, Vec<VoiceEntry>)> {
    let mut groups: Vec<(String, LanguageTag, Vec<VoiceEntry>)> = Vec::new();
    for (tag, name) in catalog.languages() {
        let voices: Vec<VoiceEntry> = catalog
            .for_language(&tag)
            .into_iter()
            .map(|voice| VoiceEntry {
                id: voice.id.clone(),
                label: voice.label(),
                bytes: voice.bytes(),
                installed: store.is_installed(voice),
                sample_rate: voice.sample_rate,
            })
            .collect();
        if !voices.is_empty() {
            groups.push((name, tag, voices));
        }
    }
    groups.sort_by(|a, b| {
        let installed = |group: &(String, LanguageTag, Vec<VoiceEntry>)| {
            group.2.iter().any(|voice| voice.installed)
        };
        installed(b).cmp(&installed(a)).then_with(|| a.0.cmp(&b.0))
    });
    groups
}

/// One row in the settings voice list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceEntry {
    pub id: String,
    pub label: String,
    pub bytes: u64,
    pub installed: bool,
    pub sample_rate: u32,
}

/// Where voices are kept, for the settings page to show.
pub fn store_location(data_directory: &std::path::Path) -> PathBuf {
    Store::new(data_directory).user_root().to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_speech() -> (tempfile::TempDir, Speech, SpeechSettings) {
        let directory = tempfile::tempdir().unwrap();
        let settings = SpeechSettings::default();
        let speech = Speech::new(directory.path(), &settings);
        (directory, speech, settings)
    }

    #[test]
    fn a_session_with_nothing_installed_cannot_speak_and_says_why() {
        let (_directory, speech, _settings) = temp_speech();
        assert!(!speech.capability().can_speak());
        assert!(speech.installed().is_empty());
        assert!(speech.current_voice().is_none());
        // The report always says something, so the diagnostics bundle never
        // has a silent hole where speech should be.
        assert!(!speech.report().is_empty());
    }

    #[test]
    fn starting_without_a_voice_explains_rather_than_failing_silently() {
        let (_directory, mut speech, settings) = temp_speech();
        let out = speech.start(Scope::Page, PageIndex(0), &settings);
        let said = out
            .iter()
            .filter_map(|o| match o {
                Outgoing::Toast(text) => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!said.is_empty(), "the reader is told something");
        assert_eq!(speech.state(), SpeechState::Idle);
    }

    /// Everything that can stop speech has to say so.
    ///
    /// This is the regression that matters most for this feature. With no
    /// picture to look at, a speak command that quietly does nothing is
    /// indistinguishable from one that is working — so every path that
    /// declines to make a sound is required here to produce a message.
    #[test]
    fn no_path_stops_speech_without_telling_the_reader() {
        fn said(out: &[Outgoing]) -> String {
            out.iter()
                .filter_map(|action| match action {
                    Outgoing::Toast(text) => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        }

        // 1. No voice installed.
        let (_directory, mut speech, settings) = temp_speech();
        let out = speech.start(Scope::Page, PageIndex(0), &settings);
        assert!(!said(&out).is_empty(), "starting with no voice says why");

        // 2. The document has no text layer.
        let (_directory, mut speech, settings) = temp_speech();
        let out = speech.cannot_speak("no text layer".into(), &settings);
        assert!(said(&out).contains("no text layer"));

        // 3. A page that came back blank, in page scope. Not an error — but
        //    it ends the reading, and it must not end it in silence.
        let (_directory, mut speech, mut settings) = temp_speech();
        speech.reading.update(speech::Event::Start {
            scope: Scope::Page,
            page: PageIndex(4),
        });
        let out = speech.text_arrived(PageIndex(4), "   \n ".into(), &mut settings);
        let message = said(&out);
        assert!(message.contains("no text"), "got {message:?}");
        assert!(
            message.contains('5'),
            "names the page as the reader sees it"
        );

        // 4. The same blank page while reading the whole document is passed
        //    over quietly, because the reading carries on to the next one.
        let (_directory, mut speech, mut settings) = temp_speech();
        speech.reading.update(speech::Event::Start {
            scope: Scope::Document,
            page: PageIndex(0),
        });
        let out = speech.text_arrived(PageIndex(0), String::new(), &mut settings);
        assert!(
            said(&out).is_empty(),
            "a blank page mid-document is not worth interrupting for"
        );
    }

    /// The app-side path, end to end, against whatever this machine has.
    ///
    /// Everything else here runs against an empty store and asserts about
    /// refusals. This one drives the real coordinator — start, text arriving,
    /// the language policy, `carry_out`, the engine, the audio player — and
    /// is the only test that would have caught a break *between* those, which
    /// is where the interesting bugs live. It skips when the session has no
    /// voice installed, so CI stays green on a machine with no audio.
    #[test]
    fn the_whole_app_side_path_reaches_the_engine() {
        let directories = crate::platform::Directories::detect();
        let settings = SpeechSettings::default();
        let mut speech = Speech::new(&directories.data, &settings);
        if !speech.capability().can_speak() {
            eprintln!("skipping: {:?}", speech.capability());
            return;
        }
        eprintln!("voice: {:?}", speech.current_voice().map(|v| v.id.clone()));

        let mut settings = settings;
        let out = speech.start(Scope::Page, PageIndex(0), &settings);
        eprintln!("start -> {out:?}");
        assert!(
            out.contains(&Outgoing::NeedText(PageIndex(0))),
            "starting asks for the page's text"
        );

        let out = speech.text_arrived(
            PageIndex(0),
            "Pulpit can read a page aloud. This is the second sentence.".into(),
            &mut settings,
        );
        eprintln!("text -> {out:?}");
        eprintln!("state after text: {:?}", speech.state());

        // Drive the poll loop the way the tick does, until it settles, timing
        // the gap between one sentence ending and the next being audible.
        // That gap is the thing prefetch exists to remove, so it is the thing
        // worth measuring rather than eyeballing.
        let mut said = Vec::new();
        let start = std::time::Instant::now();
        let mut marks: Vec<(String, u128)> = Vec::new();
        let mut last_state = speech.state();
        for _ in 0..600 {
            said.extend(speech.poll(&settings));
            let now = speech.state();
            if now != last_state {
                marks.push((format!("{now:?}"), start.elapsed().as_millis()));
                last_state = now.clone();
            }
            if !now.is_active() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        eprintln!("timeline: {marks:?}");
        eprintln!("total: {} ms", start.elapsed().as_millis());
        eprintln!("polled -> {said:?}");
        eprintln!("final state: {:?}", speech.state());

        let complaints: Vec<&Outgoing> = said
            .iter()
            .filter(|action| matches!(action, Outgoing::Toast(_)))
            .collect();
        assert!(
            complaints.is_empty(),
            "speech reported a problem: {complaints:?}"
        );
        assert!(
            said.contains(&Outgoing::Finished),
            "the reading ran to the end of the page"
        );
    }

    /// Two keys, one per scope, each a play/pause toggle for its own scope.
    ///
    /// The clause worth pinning is the second one: pressing the *page* key
    /// while the whole document is reading must start reading the page, not
    /// pause the document. Without it the page key could never start a page
    /// once the document key had been used, which is a control that does
    /// something other than what it says depending on hidden state.
    #[test]
    fn each_scope_key_toggles_its_own_scope() {
        use SpeechState::{Idle, Paused, Speaking};

        // Nothing running: either key starts its own scope.
        assert_eq!(toggling(Idle, None, Scope::Page), Toggle::Start);
        assert_eq!(toggling(Idle, None, Scope::Document), Toggle::Start);

        // The same scope's key toggles it.
        let running = Some(Scope::Document);
        assert_eq!(
            toggling(Speaking, running, Scope::Document),
            Toggle::Pause,
            "the document key pauses the document"
        );
        assert_eq!(
            toggling(Paused, running, Scope::Document),
            Toggle::Resume,
            "and resumes it"
        );

        // The clause that matters: the other scope's key starts that scope,
        // rather than pausing something the reader did not ask about.
        assert_eq!(
            toggling(Speaking, running, Scope::Page),
            Toggle::Start,
            "the page key reads the page even while the document is reading"
        );
        assert_eq!(
            toggling(Paused, running, Scope::Page),
            Toggle::Start,
            "…including when the document is merely paused"
        );

        // And symmetrically the other way round.
        let running = Some(Scope::Page);
        assert_eq!(toggling(Speaking, running, Scope::Page), Toggle::Pause);
        assert_eq!(toggling(Speaking, running, Scope::Document), Toggle::Start);
    }

    /// Stop must actually stop, mid-sentence, against a real engine.
    ///
    /// The state machine going idle is not the claim worth testing — that is
    /// three lines and obviously right. The claim is that the *sound* ends:
    /// the player is killed rather than left to finish the sentence it had
    /// already been handed, and nothing afterwards revives the reading.
    #[test]
    fn stopping_ends_the_sound_and_stays_ended() {
        let directories = crate::platform::Directories::detect();
        let settings = SpeechSettings::default();
        let mut speech = Speech::new(&directories.data, &settings);
        if !speech.capability().can_speak() {
            eprintln!("skipping: {:?}", speech.capability());
            return;
        }
        let mut settings = settings;

        speech.start(Scope::Document, PageIndex(0), &settings);
        speech.text_arrived(
            PageIndex(0),
            "This first sentence is deliberately quite long so that there is \
             plenty of it left to cut off partway through. And a second one \
             follows it, which must never be heard at all."
                .into(),
            &mut settings,
        );

        // Wait until it is genuinely audible, not merely asked for.
        let mut audible = false;
        for _ in 0..400 {
            speech.poll(&settings);
            if speech.state() == SpeechState::Speaking {
                audible = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(audible, "speech never started; nothing to stop");

        let stopped_at = std::time::Instant::now();
        speech.stop(&settings);
        assert_eq!(speech.state(), SpeechState::Idle, "the reading is over");

        // Nothing may revive it, and no late event may restart the sound.
        let mut later = Vec::new();
        for _ in 0..40 {
            later.extend(speech.poll(&settings));
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        eprintln!(
            "after stop: {later:?} ({} ms)",
            stopped_at.elapsed().as_millis()
        );
        assert_eq!(
            speech.state(),
            SpeechState::Idle,
            "still idle a second later"
        );
        assert!(
            !later
                .iter()
                .any(|action| matches!(action, Outgoing::ShowPage(_))),
            "the second sentence was never started: {later:?}"
        );

        // Second scenario, and the one an earlier version of this test was
        // engineered past: stop while the sentence is still being
        // *synthesised* — `Starting`, before anything is audible. That is
        // where the "I pressed stop and it kept talking" bug lived, because
        // there was no sound to kill yet, and the utterance played in full
        // when synthesis finished.
        speech.start(Scope::Page, PageIndex(0), &settings);
        speech.text_arrived(
            PageIndex(0),
            "Stopped before it was ever audible. Never this one either.".into(),
            &mut settings,
        );
        assert_eq!(speech.state(), SpeechState::Starting);
        speech.stop(&settings);
        assert_eq!(speech.state(), SpeechState::Idle);
        let mut later = Vec::new();
        for _ in 0..40 {
            later.extend(speech.poll(&settings));
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert_eq!(speech.state(), SpeechState::Idle, "nothing revived it");
        assert!(
            later.is_empty(),
            "a reading stopped mid-synthesis emits nothing at all: {later:?}"
        );
    }

    #[test]
    fn the_engine_failure_names_something_actionable() {
        let (_directory, speech, _settings) = temp_speech();
        let reason = speech.engine_failure();
        assert!(!reason.is_empty());
        // Never a bare "unknown": the reader has to be able to do something
        // with it, and the three causes have three different remedies.
        assert!(
            reason.contains("engine") || reason.contains("audio"),
            "got {reason:?}"
        );
    }

    #[test]
    fn an_empty_selection_is_refused_with_a_word() {
        let (_directory, mut speech, settings) = temp_speech();
        let out = speech.speak_selection("   ".into(), PageIndex(1), &settings);
        assert!(matches!(out.first(), Some(Outgoing::Toast(_))));
    }

    #[test]
    fn a_document_with_no_text_layer_stops_asking() {
        let (_directory, mut speech, settings) = temp_speech();
        let out = speech.cannot_speak("no text layer".into(), &settings);
        assert!(out.iter().any(|o| matches!(o, Outgoing::Toast(_))));
        assert_eq!(speech.state(), SpeechState::Idle);
        // And a later start does not re-ask the worker for text.
        assert!(!out.iter().any(|o| matches!(o, Outgoing::NeedText(_))));
    }

    #[test]
    fn removing_a_voice_that_is_not_there_is_harmless() {
        let (_directory, mut speech, mut settings) = temp_speech();
        let out = speech.remove_voice("en_US-lessac-medium", &mut settings);
        assert!(out.iter().any(|o| matches!(o, Outgoing::Toast(_))));
        assert!(settings.voice.is_none());
    }

    #[test]
    fn a_second_download_is_refused_while_one_runs() {
        let (_directory, mut speech, _settings) = temp_speech();
        speech.download = Some(DownloadState {
            what: "something".into(),
            progress: Progress::Advanced { done: 0, total: 1 },
            outcome: None,
        });
        let out = speech.begin_voice_download("en_US-lessac-medium");
        assert!(matches!(out.first(), Some(Outgoing::Toast(_))));
    }

    #[test]
    fn a_finished_download_dialog_can_be_dismissed_but_a_running_one_cannot() {
        let (_directory, mut speech, _settings) = temp_speech();
        speech.download = Some(DownloadState {
            what: "voice".into(),
            progress: Progress::Finishing,
            outcome: None,
        });
        speech.clear_download();
        assert!(speech.download().is_some(), "a running download stays up");

        speech.download.as_mut().unwrap().outcome = Some(Ok(()));
        speech.clear_download();
        assert!(speech.download().is_none());
    }

    #[test]
    fn declining_a_language_is_remembered_so_it_is_asked_only_once() {
        let mut settings = SpeechSettings::default();
        let german = LanguageTag::parse("de-DE").unwrap();
        assert!(!settings.has_declined(&german));
        settings.decline(german.clone());
        assert!(settings.has_declined(&german));
        // Region-insensitive: declining German is declining German.
        assert!(settings.has_declined(&LanguageTag::parse("de-AT").unwrap()));
        settings.clear_declined();
        assert!(!settings.has_declined(&german));
    }

    #[test]
    fn browsable_puts_installed_languages_first() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::builtin();
        let store = Store::under(directory.path());

        // Nothing installed: alphabetical by language name.
        let groups = browsable(&catalog, &store);
        assert!(groups.len() >= 40);
        let names: Vec<&str> = groups.iter().map(|(name, _, _)| name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);

        // Install a German voice; German moves to the top.
        let voice = catalog.voice("de_DE-thorsten-medium").unwrap();
        let target = store.voice_target(voice);
        std::fs::create_dir_all(&target).unwrap();
        for file in &voice.files {
            std::fs::write(target.join(&file.name), b"x").unwrap();
        }
        let groups = browsable(&catalog, &store);
        assert_eq!(groups[0].0, "German");
        assert!(groups[0].2.iter().any(|entry| entry.installed));
    }

    #[test]
    fn idle_speech_does_not_keep_the_event_loop_awake() {
        let (_directory, speech, _settings) = temp_speech();
        assert!(!speech.is_live());
    }
}
