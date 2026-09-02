//! Speech (issue #20, §79.4): the poll that drains the coordinator
//! ([`App::poll_speech`]), carrying out what it asks for
//! ([`App::apply_speech`]), and the dozen `Message` handlers a reader's
//! speech controls and the settings page send.
//!
//! The two speech fields (`speech`, `expanded_speech_language`) stay on
//! `App` in app.rs, the same shape as the other `app::*` extractions.

use iced::Task;

use crate::toast::Intent;

use super::{App, Message};

impl App {
    /// Which page "read this page" means.
    ///
    /// Not `state.committed()`, which counts slides and stays where it is
    /// while a Reader scrolls — reading the whole document from page one
    /// however far down you had scrolled. The two viewers count in different
    /// units, so this asks the same question `current_place` does and answers
    /// it in the unit speech needs.
    fn speech_page(&self) -> pulpit_core::page::PageIndex {
        if self.uses_document_viewer() {
            return self.reader.controls().page;
        }
        // A slide is not always a page. On a deck whose notes are paired into
        // the same file, slide three lives on PDF page six — and speech asks
        // the document worker for a *physical* page, so the mapping has to be
        // applied or a paired deck reads the wrong half of the wrong sheet.
        let mapped = self.state.document().and_then(|document| {
            self.state
                .mapping()
                .audience_source(self.state.committed(), document.pdf_pages)
        });
        match mapped {
            Some(source) => pulpit_core::page::PageIndex(source.pdf_page),
            None => pulpit_core::page::PageIndex(self.state.committed()),
        }
    }

    /// Re-read what speech can do into the capability snapshot.
    ///
    /// Called after anything that could change the answer — a download, a
    /// removal — because `Capabilities` is what every view asks, and a stale
    /// snapshot would leave a download button offering something already
    /// installed.
    fn refresh_speech_capability(&mut self) {
        self.platform.capabilities.speech = self.speech.capability();
    }

    /// Drain speech events and download progress.
    pub(super) fn poll_speech(&mut self) {
        // §82.4: a disjoint field borrow, not a clone — `self.speech` and
        // `self.settings.speech` are different fields, and this runs on
        // every tick.
        let outgoing = self.speech.poll(&self.settings.speech);
        if !outgoing.is_empty() {
            self.apply_speech(outgoing);
        }
        // A download that has just finished changes what is installed.
        let current = self.speech.capability();
        if self.platform.capabilities.speech != current {
            self.platform.capabilities.speech = current;
        }
    }

    /// Carry out what the speech coordinator asked for.
    ///
    /// The coordinator does no I/O and holds no handles, so everything it
    /// wants comes back through here — which is also what keeps its whole
    /// state machine testable without an event loop.
    pub(super) fn apply_speech(&mut self, outgoing: Vec<crate::speech::Outgoing>) {
        use crate::speech::Outgoing;
        for action in outgoing {
            match action {
                Outgoing::NeedText(page) => {
                    // No document worker means no text layer to read, and
                    // saying so beats going quiet: with nothing to look at,
                    // silence is indistinguishable from speech that is
                    // working. This is the case an image directory and a
                    // failed document worker both land in.
                    match self.reader_link.as_mut() {
                        Some(link) => {
                            link.ask(crate::reader_link::Ask::PageText { page });
                        }
                        None => {
                            // Two different absences, and the remedy differs:
                            // open a file, versus this file has nothing to
                            // read. Saying "no text layer" when nothing is
                            // open at all would send the reader looking for a
                            // problem in a document they have not chosen yet.
                            let reason = if self.state.document().is_none() {
                                "no document is open — open one first"
                            } else {
                                "this document has no text layer to read"
                            };
                            let stopped = self
                                .speech
                                .cannot_speak(reason.into(), &self.settings.speech);
                            for action in stopped {
                                if let Outgoing::Toast(message) = action {
                                    self.toasts.push(Intent::Info, message, None, self.now);
                                }
                            }
                        }
                    }
                }
                Outgoing::ShowPage(page) => {
                    // Speech has run past the end of the page it was on.
                    // Reaching past the last page is the end of the document,
                    // and is answered here rather than by asking the worker
                    // for a page that does not exist: the count is already
                    // known, and a refusal round trip would read as an error
                    // rather than as an ending.
                    // Physical pages, not slides: speech counts in the unit
                    // the document worker answers in, and on a paired deck
                    // there are twice as many pages as slides.
                    let pages = self
                        .state
                        .document()
                        .map(|document| document.pdf_pages)
                        .unwrap_or_else(|| self.state.slide_count());
                    if page.get() >= pages {
                        let finished = self.speech.no_such_page(page, &self.settings.speech);
                        // One level deep only: `no_such_page` ends the
                        // reading, and an ended reading asks for nothing more.
                        for action in finished {
                            if let Outgoing::Toast(message) = action {
                                self.toasts.push(Intent::Info, message, None, self.now);
                            }
                        }
                        continue;
                    }
                    // Deferred to the next tick rather than performed here.
                    // `on_read_command` returns an iced task — the scroll
                    // that actually moves the reader's column — and this
                    // method has callers with no way to run one: the reader
                    // pump returns a bool. An earlier version called it here
                    // and discarded the task, which turned the page in every
                    // data structure while the screen stayed where it was.
                    // The tick, which always runs at the fast cadence while
                    // speech is live, picks this up and runs the task
                    // properly; one tick of latency is nothing against the
                    // synthesis gap a page turn already carries.
                    self.deferred.push(Message::Read(
                        crate::widgets::event::ReadCommand::GoToPage(page),
                    ));
                }
                Outgoing::Toast(message) => {
                    self.diagnostics.note(message.clone());
                    self.toasts.push(Intent::Info, message, None, self.now);
                }
                // Nothing to clear: reading having ended is fully expressed
                // by the speech state the views already ask.
                Outgoing::Finished => {}
            }
        }
    }

    pub(super) fn handle_speak_toggle_scope(
        &mut self,
        scope: pulpit_core::speech::Scope,
    ) -> Task<Message> {
        let page = self.speech_page();
        // The page's key narrows to the selection when one is held
        // (issue #9): the selection is lit up on the page, so the key
        // reading it rather than the sheet around it is the key doing
        // what it looks like it will. The menu row says so too —
        // "Read page or selection".
        let selected = match scope {
            pulpit_core::speech::Scope::Page => self.selected_text(),
            _ => None,
        };
        let outgoing = match selected {
            Some(text) => self
                .speech
                .toggle_selection(text, page, &self.settings.speech),
            None => self.speech.toggle(scope, page, &self.settings.speech),
        };
        self.apply_speech(outgoing);
        Task::none()
    }
    pub(super) fn handle_speak_stop(&mut self) -> Task<Message> {
        let outgoing = self.speech.stop(&self.settings.speech);
        self.apply_speech(outgoing);
        Task::none()
    }
    pub(super) fn handle_speak_skip(
        &mut self,
        direction: pulpit_core::speech::Direction,
    ) -> Task<Message> {
        let outgoing = self.speech.skip(direction, &self.settings.speech);
        self.apply_speech(outgoing);
        Task::none()
    }
    pub(super) fn handle_set_speech_rate(&mut self, rate: f32) -> Task<Message> {
        self.settings.speech.rate = pulpit_core::speech::SpeechRate::new(rate);
        // Not restarted: the change takes effect on the next
        // sentence, which is a boundary the reader can hear. Cutting
        // the current one off to apply it immediately would be a
        // worse answer to a slider drag.
        self.persist();
        Task::none()
    }
    pub(super) fn handle_set_speech_voice(&mut self, id: String) -> Task<Message> {
        self.settings.speech.voice = Some(id);
        self.speech.reprobe(&self.settings.speech);
        self.refresh_speech_capability();
        self.persist();
        Task::none()
    }
    pub(super) fn handle_set_speech_language(
        &mut self,
        language: Option<pulpit_core::speech::LanguageTag>,
    ) -> Task<Message> {
        self.settings.speech.language = match language {
            Some(tag) => pulpit_core::speech::LanguageSetting::Explicit(tag),
            None => pulpit_core::speech::LanguageSetting::Auto,
        };
        self.speech.reprobe(&self.settings.speech);
        self.persist();
        Task::none()
    }
    pub(super) fn handle_download_voice(&mut self, id: String) -> Task<Message> {
        let outgoing = self.speech.begin_voice_download(&id);
        self.apply_speech(outgoing);
        Task::none()
    }
    pub(super) fn handle_cancel_voice_download(&mut self) -> Task<Message> {
        self.speech.cancel_download();
        Task::none()
    }
    pub(super) fn handle_clear_voice_download(&mut self) -> Task<Message> {
        self.speech.clear_download();
        Task::none()
    }
    pub(super) fn handle_remove_voice(&mut self, id: String) -> Task<Message> {
        let outgoing = self.speech.remove_voice(&id, &mut self.settings.speech);
        self.apply_speech(outgoing);
        self.refresh_speech_capability();
        self.persist();
        Task::none()
    }
    pub(super) fn handle_answer_voice_prompt(&mut self, download: bool) -> Task<Message> {
        let outgoing = self
            .speech
            .answer_prompt(download, &mut self.settings.speech);
        self.apply_speech(outgoing);
        self.persist();
        Task::none()
    }
    pub(super) fn handle_forget_declined_languages(&mut self) -> Task<Message> {
        self.settings.speech.clear_declined();
        self.persist();
        Task::none()
    }
    pub(super) fn handle_toggle_speech_language(
        &mut self,
        tag: pulpit_core::speech::LanguageTag,
    ) -> Task<Message> {
        if self.expanded_speech_language.as_ref() == Some(&tag) {
            self.expanded_speech_language = None;
        } else {
            self.expanded_speech_language = Some(tag);
        }
        Task::none()
    }
}
