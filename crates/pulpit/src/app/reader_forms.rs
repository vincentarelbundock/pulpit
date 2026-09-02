//! Form fields in a read document (§79.4): the patch/commit round trip
//! with the document worker's own form engine, keyboard and clipboard
//! routing while a field holds the caret, and picker commits
//! (`App::commit_picker_value`) landing through `App::commit_to_document`
//! the same way any other edit does (SPEC-signing.md and §9.1).
//!
//! No fields of `App` move here: `form_flow`, `form_clipboard`,
//! `pending_form_goto` and `waits_for_form_commit` stay in app.rs, the
//! same shape as the other `app::*` extractions.

use std::time::Instant;

use iced::Task;

use pulpit_render::cache::FrameKey;

use super::{
    arrow_reaches_the_engine, describe_host_request, form_event_page, now_time, today,
    AfterFormCommit, App, FormClipboard, FormFocus, FormPointer, Message, PendingEdit,
};
use crate::reader::AppliedKind;

/// What of a clipboard's contents may be put into a form field.
///
/// Control characters are dropped — a field value is text, and the `\u{1b}`
/// that came out of a terminal is not text — except the newline, which a
/// multiline field really can hold and which PDFium discards itself in a
/// single-line one. Bounded because a clipboard is not: the protocol would
/// refuse an oversized request, and a refusal is a worse answer than a paste
/// of what the field could have held anyway.
fn sanitised_paste(text: &str) -> String {
    let limit = pulpit_render::document::limits::MAX_FIELD_VALUE_BYTES;
    let mut clean = String::new();
    for character in text.chars() {
        if character.is_control() && character != '\n' {
            continue;
        }
        if clean.len() + character.len_utf8() > limit {
            break;
        }
        clean.push(character);
    }
    clean
}

impl App {
    /// `Message::PasteFormText`: a clipboard read requested by a form paste
    /// shortcut has come back.
    pub(super) fn handle_paste_form_text(
        &mut self,
        focus: Option<FormFocus>,
        value: Option<String>,
    ) -> Task<Message> {
        // The read is asynchronous, so the caret may have left the
        // field — or the document — while it was in flight. A paste
        // with nowhere to land is dropped rather than sent: the worker
        // would answer an event with no focused field with a refusal,
        // and a refusal is not what "nothing was selected" means. And
        // one whose field has been left is dropped for a stronger
        // reason: the text belongs to the field it was asked for and
        // to no other (§8.6).
        let landing = self.reader.form_holds_the_caret() && self.form_focus() == focus;
        let Some(value) = value.filter(|_| landing) else {
            return Task::none();
        };
        let text = sanitised_paste(&value);
        if text.is_empty() {
            return Task::none();
        }
        self.ask_form_key(
            pulpit_render::document::protocol::FormInputEvent::ReplaceSelection { text },
        );
        Task::none()
    }

    /// Ask for one rectangle of one page, at the revision it should contain.
    ///
    /// Shared by the two things that dirty a rectangle without the render pool
    /// knowing: an applied annotation transaction, and a form field being
    /// typed into (§9.4). A keystroke reaches here through the same path a
    /// stroke does, because it is the same problem — the picture on screen was
    /// drawn from a snapshot that predates the edit.
    ///
    /// `uncommitted` says the rectangle shows form state PDFium has not yet
    /// committed into the document, which no snapshot can contain — the patch
    /// then outlives full frames at the same revision instead of being taken
    /// down by one that was drawn without the typed characters.
    ///
    /// The adapter half: [`crate::form_flow::FormFlow`] decides what to cover
    /// and whether a request goes out at all, this turns the one that does
    /// into a rectangle of pixels and reports back whether it actually went.
    pub(super) fn ask_patch_of(
        &mut self,
        page: pulpit_core::page::PageIndex,
        dirty: pulpit_core::page::PageRect,
        uncommitted: bool,
    ) {
        let placement = self.patch_placement(page);
        // The scope grows inside the machine whether or not the page can take
        // a crop, so a request that cannot go out now is still covered by the
        // one that does.
        let Some(ask) = self
            .form_flow
            .ask_patch(page, dirty, uncommitted, placement.is_some())
        else {
            return;
        };
        let Some((key, page_width, page_height)) = placement else {
            return;
        };
        // A margin, in page points, so the edge of a mark's antialiasing is
        // inside the patch rather than split down the middle of a pixel by it.
        const MARGIN: f32 = 2.0;
        let left = ((ask.dirty.left - MARGIN) / page_width).clamp(0.0, 1.0);
        let top = ((ask.dirty.top - MARGIN) / page_height).clamp(0.0, 1.0);
        let right = ((ask.dirty.right + MARGIN) / page_width).clamp(0.0, 1.0);
        let bottom = ((ask.dirty.bottom + MARGIN) / page_height).clamp(0.0, 1.0);
        let region = pulpit_core::notes::Region::new(left, top, right - left, bottom - top);
        if !region.is_valid() {
            return;
        }
        let width = (region.width * key.width as f32).round() as u32;
        let height = (region.height * key.height as f32).round() as u32;
        if width == 0 || height == 0 {
            return;
        }
        let sent = match self.reader_link.as_mut() {
            Some(link) => link.ask(crate::reader_link::Ask::RenderPatch {
                page,
                region,
                width,
                height,
                frame_width: key.width,
                frame_height: key.height,
            }),
            None => false,
        };
        // Outstanding only if it actually went out. Marking it before the send
        // latched the page shut when there was no link to send on: the entry
        // was never answered, and every later patch for that page was held
        // back behind it for ever.
        if sent {
            self.form_flow.ask_sent(&ask, (key.width, key.height));
        }
    }

    /// What a page needs before a crop of it can be asked for: a frame on
    /// screen, at a size, and geometry to scale a rectangle of it by. The
    /// page's own width and height in points come back copied rather than
    /// borrowed, because the caller goes on to ask the machine for a request.
    fn patch_placement(&self, page: pulpit_core::page::PageIndex) -> Option<(FrameKey, f32, f32)> {
        let (surface_width, _) = self.page_surface_size()?;
        let key = self.ready_reader_frame_key(page, surface_width)?;
        let geometry = self.reader.page_geometry(page)?;
        if geometry.width <= 0.0 || geometry.height <= 0.0 {
            return None;
        }
        Some((key, geometry.width, geometry.height))
    }

    /// Ask again for any patch that was drawn against a frame size the page
    /// has since left.
    ///
    /// Zooming or resizing mid-edit used to take the typed characters off the
    /// screen: the request was sized from the cell and the answer was dropped
    /// when it did not match what was drawn. Nothing is dropped now — a patch
    /// is placed by its region and scaled — so the failure is a soft
    /// rectangle instead, and this is what ends it. The rectangle stays on
    /// screen for the round trip; only its sharpness is at stake.
    pub(super) fn reask_resized_patches(&mut self) {
        let stale = self.form_flow.resized_patches(|page| {
            self.patch_placement(page)
                .map(|(key, _, _)| (key.width, key.height))
        });
        for reask in stale {
            self.ask_patch_of(reask.page, reask.dirty, reask.uncommitted);
        }
    }

    /// Send one event to the document's own form and remember an answer is
    /// owed.
    ///
    /// The one route out for form events, so what is owed for them is counted
    /// where it is sent. Returns whether the worker took it, so a caller can
    /// tell "the form has this" from "there was no form to take it".
    pub(super) fn ask_form_event_on(
        &mut self,
        page: pulpit_core::page::PageIndex,
        event: pulpit_render::document::protocol::FormInputEvent,
    ) -> bool {
        // Whether this one *could* commit is known before it is sent; whether
        // it *did* is only known from the answer. See
        // [`App::form_commits_possible_in_flight`].
        let may_commit = event.can_change_the_document();
        // §31.3, A9, at the other choke point: a form event that could change
        // a value is refused in append-only mode, while the pointer and focus
        // events that only move the caret about go through. This also closes
        // what the command-level check never covered — a field reached with
        // Tab and typed into, which never sent a `ReadCommand` at all.
        if may_commit
            && self
                .append_only
                .is_some_and(crate::signing::AppendOnlyMode::blocks_mutation)
        {
            self.notify(crate::signing::append_only_refusal());
            return false;
        }
        let sent = match self.reader_link.as_mut() {
            Some(link) => link.ask(crate::reader_link::Ask::FormEvent { page, event }),
            None => false,
        };
        if sent {
            self.form_flow.form_event_sent(may_commit);
        }
        sent
    }

    /// One form event answered, whatever it answered with.
    pub(super) fn form_event_answered(&mut self) {
        self.form_flow.form_event_answered();
    }

    /// Follow the pointer over a form, at the rate the worker can answer.
    ///
    /// Coalesced rather than throttled on a clock: the newest position always
    /// goes out next, so the rollover follows the hand as closely as the round
    /// trip allows and never lags by a queue of stale samples.
    pub(super) fn ask_form_move(&mut self) {
        use pulpit_render::document::protocol::FormInputEvent;

        if !self.reader.press_belongs_to_the_form() {
            return;
        }
        let Some((page, at)) = self.reader.cursor_position() else {
            return;
        };
        let Some((page, at)) = self.form_move.offer((page, at)) else {
            return;
        };
        if self.ask_form_event_on(page, FormInputEvent::PointerMove { at }) {
            self.form_move.sent();
        }
    }

    /// A move was answered: the newest waiting position, if any, goes out now.
    pub(super) fn form_move_answered(&mut self) {
        let Some((page, at)) = self.form_move.answered() else {
            return;
        };
        if !self.reader.press_belongs_to_the_form() {
            return;
        }
        if self.ask_form_event_on(
            page,
            pulpit_render::document::protocol::FormInputEvent::PointerMove { at },
        ) {
            self.form_move.sent();
        }
    }

    /// Send one pointer event to the document's own form, if it has one.
    ///
    /// Returns whether it was sent, so the caller can tell "the form took this"
    /// from "there was no form to take it".
    ///
    /// Only the two ends of the gesture, never the moves between. PDFium wants
    /// `FORM_OnMouseMove` for hover effects on buttons; the worker is serial,
    /// and a round trip per pointer sample would queue in front of the page
    /// renders the reader is waiting on. A caret that does not change shape
    /// over a field is a smaller loss than a page that stutters while the hand
    /// moves across it.
    pub(super) fn ask_form_pointer(&mut self, which: FormPointer) -> bool {
        use pulpit_render::document::protocol::FormInputEvent;

        if !self.reader.press_belongs_to_the_form() {
            return false;
        }
        let Some((page, at)) = self.reader.cursor_position() else {
            return false;
        };
        let event = match which {
            FormPointer::Down => FormInputEvent::PointerDown { at },
            FormPointer::Up => FormInputEvent::PointerUp { at },
        };
        self.ask_form_event_on(page, event)
    }

    /// Move a focused combo box's selection by one, if there is one to move to.
    ///
    /// The key is consumed either way. An arrow at the end of the list does
    /// nothing rather than falling through to scroll the page, which is what a
    /// combo box does everywhere else — and a page that jumped because someone
    /// held the down arrow at the bottom of a list would be worse than a key
    /// that did nothing.
    fn form_choice_step(&mut self, forward: bool) -> Option<Task<Message>> {
        use pulpit_render::document::protocol::FormInputEvent;

        if let Some(index) = self.reader.choice_step(forward) {
            self.ask_form_key(FormInputEvent::SelectOption {
                index,
                selected: true,
            });
        }
        Some(Task::none())
    }

    /// Commit the row an open list is on, and close it.
    ///
    /// The choice crosses as a `SelectOption`, which is `FORM_SetIndexSelected`
    /// on the other side: the engine performs the selection, generates the
    /// appearance and runs the field's scripts, exactly as it would for a
    /// click on a list it had drawn itself (§8.6).
    fn choose_highlighted_option(&mut self) -> Task<Message> {
        use pulpit_render::document::protocol::FormInputEvent;

        if let Some(index) = self.reader.take_highlighted_option() {
            self.ask_form_key(FormInputEvent::SelectOption {
                index,
                selected: true,
            });
        }
        Task::none()
    }

    /// Turn the highlighted row of an open multi-select list on or off,
    /// leaving the list open. The keyboard's half of a click on a row.
    fn toggle_highlighted_option(&mut self) -> Task<Message> {
        use pulpit_render::document::protocol::FormInputEvent;

        if let Some((index, selected)) = self.reader.toggle_highlighted_option() {
            self.ask_form_key(FormInputEvent::SelectOption { index, selected });
        }
        Task::none()
    }

    /// Route one key press to the field that holds the caret (§8.6).
    ///
    /// Once a form owns the keyboard, every press is consumed here. A key the
    /// field does not understand is inert rather than falling through to the
    /// application keymap and firing an unrelated action.
    pub(super) fn form_key(
        &mut self,
        key: Option<&str>,
        text: Option<&str>,
        primary: bool,
        shift: bool,
    ) -> Option<Task<Message>> {
        use pulpit_render::document::protocol::{FormInputEvent, FormKey, KeyModifiers};

        // What the field is told was held down. Shift is what turns an arrow
        // into a selection, which is what a copy out of the field then reads.
        // The engine's control flag means "the commanding modifier", which
        // is the primary one: ⌘C in a macOS field is a copy.
        let modifiers = KeyModifiers::new(shift, primary);

        // Ctrl-anything belongs to the field while it owns the keyboard. The
        // clipboard combinations have local meanings; every other modified
        // press is consumed without consulting the global keymap. PDFium's
        // form environment has no
        // clipboard of its own — it has `FORM_GetSelectedText`,
        // `FORM_ReplaceSelection` and `FORM_SelectAllText`, and the host is
        // expected to be the clipboard — so this is where the two are joined.
        if primary {
            if self.reader.form_holds_the_caret() {
                if let Some(task) = self.form_clipboard_key(key) {
                    return Some(task);
                }
            }
            return Some(Task::none());
        }

        // An open option list takes the keys a list takes, and it takes them
        // before the field does: while it is open the arrows move a highlight
        // rather than the field's value, and nothing is committed until Enter
        // (§8.6). Escape puts the list away and leaves the field focused,
        // which is what closing a dropdown means everywhere else — so it is
        // consumed here rather than falling through to abandon the field edit.
        if self.reader.choice_list().is_some() {
            match key {
                Some("ArrowUp") | Some("Up") => {
                    self.reader.step_choice_list(false);
                    return Some(Task::none());
                }
                Some("ArrowDown") | Some("Down") => {
                    self.reader.step_choice_list(true);
                    return Some(Task::none());
                }
                // Space ticks the highlighted row of a multi-select list, and
                // is the keyboard's half of what a click does there. On a
                // single-select list it is nothing: Enter already chooses, and
                // a second key that also chose would only be a way to choose
                // by accident.
                Some("Space") if self.reader.choice_list_is_multiple() => {
                    return Some(self.toggle_highlighted_option());
                }
                // Enter is "done" on a multi-select list rather than "choose":
                // each tick was committed as it was made, so there is nothing
                // held back for Enter to commit, and treating it as a choice
                // would silently toggle whichever row the highlight happened
                // to be resting on.
                Some("Enter") if self.reader.choice_list_is_multiple() => {
                    self.reader.close_choice_list();
                    return Some(Task::none());
                }
                Some("Enter") => {
                    return Some(self.choose_highlighted_option());
                }
                Some("Escape") => {
                    self.reader.close_choice_list();
                    return Some(Task::none());
                }
                _ => {}
            }
        }

        // The named keys a field uses.
        let named = match key {
            Some("Backspace") => Some(FormKey::Backspace),
            Some("Delete") => Some(FormKey::Delete),
            Some("Enter") => Some(FormKey::Enter),
            // Tab is not here: field traversal is `document_key`'s, above,
            // because it has to turn the page as well as move the caret.
            Some("ArrowLeft") | Some("Left") => Some(FormKey::Left),
            Some("ArrowRight") | Some("Right") => Some(FormKey::Right),
            // A *list* box moves its own selection on an arrow key, so those
            // go straight through, and so does an editable combo box, whose
            // list is PDFium's own. A closed, non-editable combo box ignores
            // them — in a real viewer the key would be travelling to a
            // dropdown that is not open — so for one of those, and only one
            // of those, the arrow becomes the selection change PDFium does
            // answer to (§8.6).
            Some("ArrowUp") | Some("Up")
                if !arrow_reaches_the_engine(self.reader.focused_choice()) =>
            {
                return self.form_choice_step(false)
            }
            Some("ArrowDown") | Some("Down")
                if !arrow_reaches_the_engine(self.reader.focused_choice()) =>
            {
                return self.form_choice_step(true)
            }
            Some("ArrowUp") | Some("Up") => Some(FormKey::Up),
            Some("ArrowDown") | Some("Down") => Some(FormKey::Down),
            Some("Home") => Some(FormKey::Home),
            Some("End") => Some(FormKey::End),
            _ => None,
        };
        if let Some(named) = named {
            self.ask_form_key(FormInputEvent::KeyDown {
                key: named,
                modifiers,
            });
            return Some(Task::none());
        }
        if key == Some("Escape") {
            self.ask_form_key(FormInputEvent::KeyDown {
                key: FormKey::Escape,
                modifiers,
            });
            return Some(Task::none());
        }

        // Anything that produced text is text. Taken from the toolkit's own
        // `text` rather than from the key name, because that is what has been
        // through the keyboard layout and the dead keys: the key named "2" on
        // one layout is the character that layout puts there, and a field that
        // read the key name would spell a French keyboard wrong.
        let Some(text) = text else {
            return Some(Task::none());
        };
        for character in text.chars() {
            // Control characters are not text. The named keys above already
            // carry the ones a field acts on, and forwarding the rest as
            // characters is how a stray \u{7f} ends up in someone's name.
            if character.is_control() {
                continue;
            }
            self.ask_form_key(FormInputEvent::Char { character });
        }
        Some(Task::none())
    }

    /// The clipboard shortcuts, with the caret in a text field (§8.6).
    ///
    /// `None` for a Ctrl-press that is not one of them; the caller still
    /// consumes it because the focused form owns the keyboard.
    ///
    /// Copy and cut cannot answer here: what is selected is PDFium's to
    /// report, so the event goes out and the clipboard is written when the
    /// answer arrives — see [`Self::form_changed`]. Cut is a copy that
    /// remembers to remove what it took.
    fn form_clipboard_key(&mut self, key: Option<&str>) -> Option<Task<Message>> {
        use pulpit_render::document::protocol::FormInputEvent;

        let key = key?;
        if key.eq_ignore_ascii_case("c") || key.eq_ignore_ascii_case("x") {
            let intent = if key.eq_ignore_ascii_case("x") {
                FormClipboard::Cut
            } else {
                FormClipboard::Copy
            };
            // Named before the event goes out, because the answer comes back
            // a turn later and the caret is free to move in between.
            let focus = self.form_focus();
            if !self.ask_form_key(FormInputEvent::CopySelection) {
                // Nothing will answer, so nothing is left waiting for one.
                return None;
            }
            self.form_clipboard = Some((intent, focus));
            return Some(Task::none());
        }
        if key.eq_ignore_ascii_case("v") {
            // Read now, sent when it arrives: the clipboard is the toolkit's
            // and answers asynchronously. The field is named now too, so what
            // arrives can be matched against where it was asked for.
            let focus = self.form_focus();
            return Some(
                iced::clipboard::read().map(move |value| Message::PasteFormText {
                    focus: focus.clone(),
                    value,
                }),
            );
        }
        if key.eq_ignore_ascii_case("a") {
            self.ask_form_key(FormInputEvent::SelectAll);
            return Some(Task::none());
        }
        None
    }

    /// Send one event to the page the *focus* is on, not the page the pointer
    /// is over.
    ///
    /// The clipboard events all reach the focused field through its own page
    /// handle, and the two pages need not be the same: a caret can sit in a
    /// field at the foot of one page with the pointer resting over the next,
    /// and a copy addressed to the wrong page reads an empty selection. Falls
    /// back to the ordinary route when nothing is focused, where the answer is
    /// "nothing is selected" either way.
    /// The field that holds the caret, named so an answer that arrives a turn
    /// later can be matched against it — see [`FormFocus`].
    pub(super) fn form_focus(&self) -> Option<FormFocus> {
        self.reader.focused_widget().map(|widget| FormFocus {
            page: widget.page,
            field: widget.field.clone(),
        })
    }

    /// Send one keyboard event to the field that holds the caret.
    ///
    /// The one route for every focus-owned event — a key, a copy, a select-all
    /// — because they all answer the same question about where the event
    /// belongs: the focused widget's page, and the pointer's only when nothing
    /// is focused.
    ///
    /// Only reached when the worker has said a field has focus, so an ordinary
    /// reader of an ordinary deck never takes this path and every letter still
    /// means what the keymap says it means.
    pub(super) fn ask_form_key(
        &mut self,
        event: pulpit_render::document::protocol::FormInputEvent,
    ) -> bool {
        let Some(page) = form_event_page(
            self.reader.focused_widget().map(|widget| widget.page),
            self.reader.cursor_position().map(|(page, _)| page),
            self.reader.current_page(),
        ) else {
            return false;
        };
        self.ask_form_event_on(page, event)
    }

    /// Ask the worker what the document's fields now hold (§6.4).
    ///
    /// Read-only, so it can be sent whenever the answer would be stale without
    /// any of the revision bookkeeping a mutation needs.
    pub(super) fn ask_field_list(&mut self) {
        if let Some(link) = self.reader_link.as_mut() {
            link.ask(crate::reader_link::Ask::ListFields);
        }
    }

    /// A form event came back from the worker (§8.6).
    ///
    /// Three separable things arrive together, because one keystroke produces
    /// all three and splitting them across messages would let the caret and
    /// the picture disagree:
    ///
    /// * **Where the caret is.** Taken as fact, never inferred. Until this
    ///   says a field has it, letters are shortcuts.
    /// * **What to redraw.** PDFium holds the typed characters in its own
    ///   environment; the frame on screen was drawn from a snapshot that
    ///   predates them. Without the patch the reader types and sees nothing
    ///   until the snapshot behind it lands.
    /// * **What was committed.** A committed field value is a document change
    ///   like any other — one revision, one undo entry, in the same history as
    ///   the annotations (§9.1).
    pub(super) fn form_changed(
        &mut self,
        page: pulpit_core::page::PageIndex,
        mut result: pulpit_render::document::protocol::FormEventResult,
    ) {
        // What a copy asked for, on its way out. Held rather than written:
        // this runs inside the worker pump, which has no `Task` to hand back,
        // so the write goes out on the next tick alongside everything else the
        // pump left behind.
        //
        // An empty selection is left alone. Copy with nothing selected is a
        // no-op in every text box there is, and emptying the clipboard instead
        // would lose whatever the reader had put there to paste.
        if let Some(text) = result.selected_text.take() {
            if let Some((intent, focus)) = self.form_clipboard.take() {
                // The copy itself is unconditional: the text came back, and
                // whatever the caret has done since, the reader asked for it
                // and it goes to the clipboard.
                //
                // The *removal* is not. A cut deletes, and it must delete out
                // of the field it was typed in: the answer arrives a turn
                // later, and a click queued behind it can have moved the caret
                // by the time the second half goes out. Checked against the
                // focus this very answer reports, and against what this layer
                // believes now, and skipped unless both still name the field
                // the cut began in. It cannot be made airtight from here —
                // only a worker-side "copy and replace in one event" would be,
                // and that is a protocol change for a race this closes in
                // practice — so what is left is the case where the click's own
                // answer has not landed yet, and the loss it risks is a
                // deletion the reader asked for rather than one they did not.
                let answered_the_same_field =
                    result.focused_widget.as_ref().is_some_and(|widget| {
                        focus.as_ref().is_some_and(|focus| {
                            focus.page == widget.page && focus.field == widget.field
                        })
                    });
                let still_there =
                    focus.is_none() || (answered_the_same_field && self.form_focus() == focus);
                if !text.is_empty() {
                    self.deferred.push(Message::WriteFormClipboard(text));
                    if intent == FormClipboard::Cut && still_there {
                        // The other half of a cut: what was taken is removed
                        // through the engine's own replacement, in one edit,
                        // rather than by a run of synthesised backspaces.
                        self.ask_form_key(
                            pulpit_render::document::protocol::FormInputEvent::ReplaceSelection {
                                text: String::new(),
                            },
                        );
                    }
                }
            }
        }
        self.reader.set_form_typing(result.text_focus);
        let opened_choice = result.opened_choice;
        // Read before the choice moves into the reader: the uncommitted rule
        // at the bottom of this function still needs to know one is held.
        let editing_choice = result.focused_choice.is_some();
        self.reader.set_focused_choice(result.focused_choice.take());
        // A press landed on a choice field whose list PDFium deliberately did
        // not open, because this is the layer that draws it (§8.6).
        if opened_choice {
            self.reader.open_choice_list();
        }
        // Where to draw the focus ring, taken as fact like the caret itself.
        // The answer that says nothing is focused matters as much as the one
        // that names a widget: it is what takes the ring off the last field.
        self.reader
            .set_focused_widget(result.focused_widget.clone());
        // Open the calendar when the caret lands in a date field, and put it
        // away when it leaves. The clock is read here, at the edge, because
        // the reader's state deliberately reads none.
        self.reader.set_date_language(self.date_language);
        self.reader
            .set_focused_date(result.focused_date.as_ref(), today());
        // And the steppers when it lands in a time field, on the same terms:
        // the wall clock is read here, at the edge, and handed in as the
        // starting time for a field that holds nothing readable.
        self.reader
            .set_focused_time(result.focused_time.as_ref(), now_time());
        // What this field wants, said once as the caret arrives in it rather
        // than once per keystroke — every event carries the hint, including
        // the ones that changed nothing, so the dedup is what keeps it a hint
        // instead of a stream. It is shown beside the field, which is where
        // the reader is looking; the diagnostics line stays only for the case
        // the tooltip cannot cover, which is a focus the worker reported
        // without a widget to hang the tooltip on.
        if self.reader.take_form_hint(result.focused_hint.as_deref()) {
            if let (Some(hint), None) = (&result.focused_hint, &result.focused_widget) {
                self.diagnostics.note(format!("this field takes a {hint}"));
            }
        }

        // Anything the document's own JavaScript asked for. Refused in the
        // worker already; said out loud here, because a refusal nobody is told
        // about looks exactly like a document that asked for nothing.
        for request in &result.requests {
            self.diagnostics.note(describe_host_request(request));
        }
        // …and the two kinds a diagnostics line is not enough for.
        for request in &result.requests {
            self.host_request_needs_the_reader(request);
        }

        if let Some(committed) = &result.committed {
            // The same bookkeeping an applied transaction gets: the document
            // has moved, nothing on screen or on disk reflects it yet, and the
            // snapshot the render pool reads from is now stale.
            self.reader.field_committed(committed);
            // …including the journal, which is the half this used to miss: a
            // form commit does not come back as an `Applied`, so it never
            // reached the only place that recorded anything, and a recovery
            // put back the ink and dropped every field the reader had filled
            // in. What one is written down as lives in
            // [`crate::reader_journal::entry_for_committed_field`].
            if let Some(entry) = crate::reader_journal::entry_for_committed_field(committed) {
                self.journal(entry);
            }
            // The navigator's fill marks are only as true as the list they
            // were drawn from, and a commit has just made that list wrong.
            // Re-asked rather than patched here: PDFium is the sole author of
            // a value, and the value it committed is not always the one that
            // was typed — a format script may have rewritten it.
            self.ask_field_list();
            self.reader_render.edited_at = Some(Instant::now());
            self.reader_render.urgency = self
                .reader_render
                .urgency
                .max(crate::reader::RasterUrgency::Prompt);
        }

        // One patch, covering everything the event dirtied. The worker already
        // coalesced its rectangles; this unions what is left, because two
        // round trips for one keystroke is worse than one slightly larger one.
        let dirty = result
            .invalidated
            .iter()
            .copied()
            .reduce(|all, one| all.union(&one));
        if let Some(dirty) = dirty {
            // A keystroke's pixels are uncommitted until the field commits:
            // they live in PDFium's form environment and in no snapshot, so
            // that patch must survive full frames at the same revision. Only
            // that patch, though — a rollover the pointer drew is state a
            // full frame reproduces exactly, and marking it uncommitted made
            // it immortal, growing over the page with every pointer move. The
            // rule: uncommitted means an interaction is *being held open* —
            // a caret in a text field, or an open choice list.
            let editing = result.text_focus || editing_choice;
            self.ask_patch_of(page, dirty, result.committed.is_none() && editing);
        }

        // A Save As is waiting on this answer. Everything above has run — the
        // commit is in the revision, in the undo history and in the field
        // list — so the save can go on; queued because this is the pump and
        // there is no `Task` to hand back from here (§79.1).
        if let Some(after) = self.waits_for_form_commit.take() {
            self.deferred.push(Message::ResumeAfterFormCommit(after));
        }
    }

    /// Put the time the helper is showing into the field it is open over.
    ///
    /// Word for word the calendar's path: the chosen value becomes the same
    /// `SetField` an undo uses, so PDFium's own editor takes it, the field's
    /// format script runs over it, and it lands in the shared undo history
    /// like any other change (§9.1). pulpit chooses the *text* and nothing
    /// more.
    pub(super) fn commit_focused_time(&mut self) -> Task<Message> {
        let Some(picker) = self.reader.time_picker() else {
            return Task::none();
        };
        let value = picker.time.format(&picker.pattern, self.date_language);
        self.commit_picker_value(picker.field.clone(), value)
    }

    /// A date or a time chosen from a picker, committed as an ordinary field
    /// edit: the same `SetField` an undo uses, so it goes through PDFium's
    /// own editor, gets the field's format script run over it, and lands in
    /// the shared undo history like any other change (§9.1). pulpit chooses
    /// the *text*; PDFium still decides what it looks like in the field.
    ///
    /// §79.5: `PickDate` and `commit_focused_time` built this transaction by
    /// hand, identically but for the field and the formatted value.
    ///
    /// The engine kills the focus as it takes the value — `SetField` runs
    /// through PDFium's own editor, which force-kills the focus and lets the
    /// form page go — and the answer comes back as an `Applied`, which
    /// carries no `FormEventResult` and so refreshes nothing. Said here
    /// rather than plumbed through a form-event reply: a commit is a document
    /// mutation, and turning it into a form event to get a focus report back
    /// would be a second editing path for one value (§9.1).
    pub(super) fn commit_picker_value(&mut self, field: String, value: String) -> Task<Message> {
        let transaction = pulpit_render::document::DocumentTransaction::one(
            pulpit_render::document::DocumentCommand::SetField {
                name: field,
                value,
                // A date and a time are single values; nothing to select.
                selected: Vec::new(),
            },
        );
        self.reader.form_focus_dropped();
        self.commit_to_document(transaction);
        Task::none()
    }

    /// Ask the worker to take focus off the field the caret is in, so the
    /// edit in progress is committed before the file is written — or before
    /// the print dialog reads what is in the form.
    ///
    /// `Done` means the operation is now waiting on that answer, and will be
    /// resumed from the tick; `Refused` that no field holds the caret, which
    /// is every save of a deck without a form; `Failed` that there is no
    /// worker to ask, in which case there is also no document and the
    /// caller.s own check will say so.
    pub(super) fn ask_form_commit_first(
        &mut self,
        after: AfterFormCommit,
    ) -> crate::platform::Outcome {
        use pulpit_render::document::protocol::FormInputEvent;

        let Some(page) = self.reader.focused_widget().map(|widget| widget.page) else {
            return crate::platform::Outcome::refused("no field holds the caret");
        };
        let sent = self.ask_form_event_on(page, FormInputEvent::Focus { gained: false });
        if !sent {
            return crate::platform::Outcome::failed("the document worker took no event");
        }
        self.waits_for_form_commit = Some(after);
        crate::platform::Outcome::Done
    }

    /// Post one atomic user action to the document worker.
    ///
    /// One transaction is one revision and one undo entry, whatever it
    /// contains (§9.1) — an eraser sweep that took eleven marks included.
    pub(super) fn commit_to_document(
        &mut self,
        transaction: pulpit_render::document::DocumentTransaction,
    ) -> bool {
        if transaction.is_empty() {
            return false;
        }
        // §31.3, A9: the one place every annotation transaction passes
        // through, whatever gesture made it. Guarding here rather than at the
        // gesture is what lets the hand keep panning a signed document while
        // still refusing the one thing the hand can change — picking a mark
        // up and putting it down somewhere else.
        if self
            .append_only
            .is_some_and(crate::signing::AppendOnlyMode::blocks_mutation)
        {
            self.notify(crate::signing::append_only_refusal());
            return false;
        }
        // A form event in flight may commit a value, and a commit is a
        // revision. Whether it will is only known from its answer, so a
        // mutation sent now would name a revision it cannot know — and be
        // refused for a conflict that is nobody's mistake. It waits for the
        // form to answer instead, which is one round trip and only ever
        // happens when a mark is made in the same instant a field is edited.
        let Some(transaction) = self.form_flow.commit_requested(transaction) else {
            return true;
        };
        let expected = self.expected_revision();
        // Keep drawing what this commit creates until a frame containing it
        // arrives (§9.2): the stroke must not vanish at release and reappear
        // a snapshot round trip later. The answer also says whether that frame
        // is owed *soon* or merely eventually.
        let urgency = self.reader.retain_commit(&transaction);
        self.reader_pending.push_back(PendingEdit {
            kind: AppliedKind::Edit,
            names: None,
            transaction: Some(transaction.clone()),
            urgency,
            reversal: None,
        });
        let sent = match self.reader_link.as_mut() {
            Some(link) => link.ask(crate::reader_link::Ask::Apply {
                expected_revision: expected,
                transaction,
            }),
            None => false,
        };
        if !sent {
            self.reader_pending.pop_back();
            self.reader.commit_refused();
        }
        sent
    }
}
