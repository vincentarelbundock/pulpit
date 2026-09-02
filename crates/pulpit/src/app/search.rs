//! The document/notes search pane (§79.4): the query round trip
//! ([`App::on_find_command`]), the scan itself ([`App::pump_search`],
//! [`App::pump_one_search_chunk`]), and opening/closing the pane
//! ([`App::open_search`], [`App::close_search`]), which remembers
//! [`SearchOrigin`] so a search abandoned rather than confirmed puts the
//! reader or the presenter back where it found them.
//!
//! The seven search fields (`search`, `search_pane`, `search_origin`,
//! `search_settle_at`, `search_scroll`, `search_viewport`, plus
//! `keyboard_region`'s two search variants) stay on `App` in app.rs, the
//! same as `app::print`'s fields — only the behaviour moves here.

use std::time::Duration;

use iced::Task;

use super::{App, KeyboardRegion, Message};

/// How long the search box waits for typing to stop before it scans.
///
/// Two ticks: short enough that it reads as "while I was still typing", long
/// enough that a fast typist starts one scan rather than one per letter.
const SEARCH_SETTLE: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy)]
pub(super) enum SearchOrigin {
    Reader {
        page: pulpit_core::page::PageIndex,
        zoom: crate::widgets::document::model::Zoom,
        fraction: f32,
        spread: crate::widgets::document::model::PageSpread,
    },
    Presenter(pulpit_core::Place),
}

impl App {
    /// Something the search pane asked for.
    ///
    /// A query change restarts the search: the in-process sources — speaker
    /// notes and the bookmark tree — are answered here and now, so the list
    /// has something in it before the first chunk of page text comes back,
    /// and the page scan is left to [`App::pump_search`].
    pub(super) fn on_find_command(
        &mut self,
        command: crate::widgets::event::FindCommand,
    ) -> Task<Message> {
        use crate::widgets::event::FindCommand;
        let input_was_focused = self.keyboard_region == KeyboardRegion::SearchInput;
        let leave_input = || {
            iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::unfocus())
        };
        match command {
            FindCommand::Type(typed) => {
                self.keyboard_region = KeyboardRegion::SearchInput;
                let mut query = pulpit_core::search::Query::new(
                    &typed,
                    self.search.query().case_sensitive,
                    self.search.query().whole_word,
                );
                query.regex = self.search.query().regex;
                self.restart_search(query);
                // A keystroke, not a decision: hold the document scan until
                // the typing settles. The toggles below do not wait, because
                // pressing one *is* the decision.
                if self.search.scanning() {
                    self.search_settle_at = Some(self.now + SEARCH_SETTLE);
                }
                Task::none()
            }
            FindCommand::ToggleCaseSensitive
            | FindCommand::ToggleWholeWord
            | FindCommand::ToggleRegex => {
                let current = self.search.query();
                let case_sensitive =
                    current.case_sensitive ^ (command == FindCommand::ToggleCaseSensitive);
                let whole_word = current.whole_word ^ (command == FindCommand::ToggleWholeWord);
                let regex = current.regex ^ (command == FindCommand::ToggleRegex);
                let mut query =
                    pulpit_core::search::Query::new(current.text(), case_sensitive, whole_word);
                query.regex = regex;
                self.restart_search(query);
                Task::none()
            }
            FindCommand::Clear => {
                self.search.clear();
                Task::none()
            }
            FindCommand::Next => {
                if self.search_pane.is_open() {
                    self.keyboard_region = KeyboardRegion::SearchResults;
                }
                let hit = self.search.advance().cloned();
                Task::batch([
                    self.go_to_hit(hit),
                    self.reveal_search_selection(crate::widgets::scroll::RevealDirection::Down),
                    if input_was_focused {
                        leave_input()
                    } else {
                        Task::none()
                    },
                ])
            }
            FindCommand::Previous => {
                if self.search_pane.is_open() {
                    self.keyboard_region = KeyboardRegion::SearchResults;
                }
                let hit = self.search.retreat().cloned();
                Task::batch([
                    self.go_to_hit(hit),
                    self.reveal_search_selection(crate::widgets::scroll::RevealDirection::Up),
                    if input_was_focused {
                        leave_input()
                    } else {
                        Task::none()
                    },
                ])
            }
            FindCommand::Focus(key) => {
                self.keyboard_region = KeyboardRegion::SearchResults;
                let hit = self.search.focus_key(key).cloned();
                // A pressed result is a choice, so closing Search later must
                // not undo it. Search itself stays open for further matches.
                self.search_origin = None;
                Task::batch([
                    self.go_to_hit(hit),
                    self.reveal_search_selection(crate::widgets::scroll::RevealDirection::Nearest),
                    if input_was_focused {
                        leave_input()
                    } else {
                        Task::none()
                    },
                ])
            }
            FindCommand::ActivateCurrent => {
                self.search_origin = None;
                let hit = self.search.current().cloned();
                self.go_to_hit(hit)
            }
            FindCommand::DragScrollTo(offset) => {
                let offset = offset as f32;
                // The list has not moved by itself, so it is told where to
                // go as well as being recorded.
                self.search_scroll = offset;
                iced::widget::operation::scroll_to(
                    crate::widgets::search::view::results_id(),
                    iced::widget::operation::AbsoluteOffset { x: 0.0, y: offset },
                )
            }
            FindCommand::Scrolled { offset, viewport } => {
                self.search_scroll = offset as f32;
                self.search_viewport.set(viewport as f32);
                Task::none()
            }
        }
    }

    fn reveal_search_selection(
        &self,
        direction: crate::widgets::scroll::RevealDirection,
    ) -> Task<Message> {
        let Some(index) = self.search.position().map(|position| position - 1) else {
            return Task::none();
        };
        let offset = crate::widgets::scroll::reveal_offset(
            index,
            crate::widgets::search::view::RESULT_ROW_HEIGHT,
            self.search_scroll,
            self.search_viewport.get(),
            self.search.hits().len(),
            direction,
        );
        if (offset - self.search_scroll).abs() <= f32::EPSILON {
            Task::none()
        } else {
            iced::widget::operation::scroll_to(
                crate::widgets::search::view::results_id(),
                iced::widget::operation::AbsoluteOffset { x: 0.0, y: offset },
            )
        }
    }

    pub(super) fn open_search(&mut self) -> Task<Message> {
        self.search_origin = if self.uses_document_viewer() {
            self.reader
                .reading_position()
                .map(|(page, zoom, fraction)| SearchOrigin::Reader {
                    page,
                    zoom,
                    fraction,
                    spread: self.reader.controls().spread,
                })
        } else {
            Some(SearchOrigin::Presenter(self.current_place()))
        };
        // Scan outwards from the page in front of the reader. Somebody who
        // opens the box on page 300 is looking for something near page 300,
        // and a scan that starts at page one makes them wait for 299 pages of
        // answers they did not ask for.
        self.search.begin_at(self.showing_page());
        self.search_pane.set(true, self.motion, self.now);
        self.keyboard_region = KeyboardRegion::SearchInput;
        // Queued rather than focused directly: the search input is not in
        // the widget tree until the view pass that draws it with the pane
        // now open has run (§79.1).
        self.deferred.push(Message::FocusSearchInput);
        self.overview = false;
        Task::none()
    }

    pub(super) fn close_search(&mut self, restore_origin: bool) -> Task<Message> {
        self.search_pane.set(false, self.motion, self.now);
        self.keyboard_region = KeyboardRegion::Document;
        self.deferred
            .retain(|message| !matches!(message, Message::FocusSearchInput));
        let origin = self.search_origin.take();
        if !restore_origin {
            return Task::none();
        }
        match origin {
            Some(SearchOrigin::Reader {
                page,
                zoom,
                fraction,
                spread,
            }) if self.uses_document_viewer() => {
                self.navigating_history = true;
                let spread_task =
                    self.on_read_command(crate::widgets::event::ReadCommand::SetSpread(spread));
                let zoom_task =
                    self.on_read_command(crate::widgets::event::ReadCommand::SetZoom(zoom));
                self.reader.restore_position(page, Some(zoom), fraction);
                self.navigating_history = false;
                // Both commands above worked out a scroll from where the
                // search left the reader standing, and both were built
                // before the restore moved it. The restore has the last
                // word, so it is the last thing the surface hears.
                Task::batch([spread_task, zoom_task, self.scroll_surface_to_reader()])
            }
            Some(SearchOrigin::Presenter(place)) if !self.uses_document_viewer() => {
                self.go_to_place(place)
            }
            _ => Task::none(),
        }
    }

    /// The search pane's reveal at this frame's clock, for the views that
    /// have the application but not its `now`.
    pub(crate) fn search_reveal(&self) -> f32 {
        self.search_pane.reveal(self.now)
    }

    pub(crate) fn search_input_focused(&self) -> bool {
        self.keyboard_region == KeyboardRegion::SearchInput
    }

    pub(crate) fn search_results_focused(&self) -> bool {
        self.keyboard_region == KeyboardRegion::SearchResults
    }

    /// How many pages the search has to cover.
    ///
    /// From whichever half is open: in document mode the reader knows it, in
    /// presentation mode the deck does.
    fn searchable_page_count(&self) -> usize {
        if self.reader.is_open() {
            self.reader.page_count()
        } else {
            self.state
                .document()
                .map(|document| document.pdf_pages)
                .unwrap_or(0)
        }
    }

    /// Run the query over the notes and the outline, which are in this
    /// process and need no round trip.
    ///
    /// In the presenter this is often the more useful half — "which slide was
    /// the one about X" is usually answered by what the speaker wrote — and
    /// having it before the first chunk arrives is what makes the box feel
    /// instant on a long deck.
    fn absorb_local_hits(&mut self) {
        let mut found = Vec::new();
        if let Some(document) = self.state.document() {
            if let Some(notes) = document.text_notes.as_ref() {
                found.extend(pulpit_core::search::search_notes(
                    self.search.query(),
                    notes,
                    document.pdf_pages,
                ));
            }
            if let Some(navigation) = self.navigation.get(&document.id.0) {
                found.extend(pulpit_core::search::search_outline(
                    self.search.query(),
                    &navigation.outline,
                ));
            }
        }
        self.search.absorb(found);
    }

    /// The document under the search changed, so what was found in the old one
    /// is no longer true of this one.
    ///
    /// A hit is a page number and a set of rectangles on that page. When a
    /// deck is rebuilt those rectangles describe where the words used to be:
    /// the results list points at text that has moved and the overlay marks
    /// bare paper. The page count moves too, so a rebuild that added pages
    /// would never scan them.
    ///
    /// So the query is kept and everything found for it is thrown away and
    /// looked for again. Keeping the query is the point — a deck is rebuilt
    /// while you are looking for something in it, and being made to type it
    /// again on every recompile is its own bug.
    pub(super) fn rescan_search_after_document_change(&mut self) {
        // `open` restarts under a new generation, so chunks already in flight
        // for the old document land nowhere.
        self.search.open(self.searchable_page_count());
        if self.search.query().is_empty() {
            return;
        }
        self.search_scroll = 0.0;
        self.absorb_local_hits();
    }

    /// Point the search at the open document under a new query.
    fn restart_search(&mut self, query: pulpit_core::search::Query) {
        self.search_scroll = 0.0;
        // A toggle or a fresh query goes out at once. Only `Type` holds it
        // back, and it arms the delay itself after this returns.
        self.search_settle_at = None;
        let pages = self.searchable_page_count();
        self.search.open(pages);
        let invalid = query.validate().err();
        let generation = self.search.set_query(query);
        if self.search.query().is_empty() {
            return;
        }
        if let Some(problem) = invalid {
            self.search.fail(
                generation,
                pulpit_core::search::SearchProblem::InvalidPattern(problem),
            );
            return;
        }
        self.absorb_local_hits();
    }

    /// Show a hit: put its page on screen in whichever view is mounted.
    ///
    /// Navigation goes through the ordinary verbs — the reader's `GoToPage`,
    /// the presentation's preview move — rather than a second way to move,
    /// because in presentation mode a page change has an audience window on
    /// the other end of it.
    fn go_to_hit(&mut self, hit: Option<pulpit_core::search::Hit>) -> Task<Message> {
        let Some(hit) = hit else {
            return Task::none();
        };
        if crate::layout::PrimaryViewer::of(&self.active_layout)
            == crate::layout::PrimaryViewer::Document
        {
            return self.on_read_command(crate::widgets::event::ReadCommand::GoToPage(hit.page));
        }
        // In presentation mode the presenter moves and the audience does not:
        // finding a slide is looking for it, not showing it to the room.
        let slide = self.slide_showing(hit.page.get());
        self.dispatch(Message::Nav(pulpit_core::Command::PreviewGoTo(slide)))
    }

    /// Ask for the next chunk of page text, if a search is running and the
    /// document worker is there to answer.
    ///
    /// Called from the tick, like every other round trip: a scan must not
    /// start inside a draw.
    /// Send whatever the scan is ready to ask for, up to what the link
    /// carries at once.
    ///
    /// Called on the tick *and* the moment a chunk lands, so the next request
    /// leaves in the same event-loop turn as the answer that freed its slot
    /// rather than one tick later.
    pub(super) fn pump_search(&mut self) {
        // Typing that has not settled yet is not a scan. The query is already
        // set, so a chunk for the previous one that is still in flight will be
        // discarded on arrival either way.
        if let Some(settle_at) = self.search_settle_at {
            if self.now < settle_at {
                return;
            }
            self.search_settle_at = None;
        }
        while self.pump_one_search_chunk() {}
    }

    /// One request, or false when there is nothing to ask or nobody to ask.
    fn pump_one_search_chunk(&mut self) -> bool {
        let Some((generation, pages)) = self.search.next_request() else {
            return false;
        };
        let query = self.search.query().clone();
        let sent = self
            .reader_link
            .as_mut()
            .map(|link| {
                link.ask(crate::reader_link::Ask::FindText {
                    generation,
                    query,
                    from_page: pages.start,
                    to_page: pages.end,
                })
            })
            .unwrap_or(false);
        if sent {
            return true;
        }
        // No document worker — presentation mode, where the render pool holds
        // the deck. It searches through the same matcher over the same text
        // layer, so what the presenter finds is what the reader would.
        let asked = self.state.document().map(|document| document.id.0);
        if let (Some(document), Some(supervisor)) = (asked, self.supervisor.as_mut()) {
            supervisor.request_find_text(document, generation, self.search.query().clone(), pages);
            return true;
        }
        // Nothing open that can answer. Notes and bookmarks have already been
        // searched in this process; saying "no page text here" once is better
        // than asking nobody again on every tick.
        self.search.fail(
            generation,
            pulpit_core::search::SearchProblem::Unsupported(
                "the page text of this document is not available".into(),
            ),
        );
        false
    }
}
