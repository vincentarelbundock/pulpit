//! Printing (§79.4): the dialog, the optional scratch copy a marked-up print
//! is spooled from, and the job handed to the platform.
//!
//! `App` used to carry this as three independent fields —
//! `print_scratch: Option<PathBuf>`, `print_pending: Option<PrintPlan>` and
//! `print_in_flight: bool` — whose *combination* was the actual state: a
//! reviewer had to read all three together to know whether nothing was
//! printing, a copy was still being written, or a job was already handed to
//! the platform. [`PrintJob`] makes that one field with three states.
//! `print_dialog` stays a separate field: it is the Print sheet's own UI
//! state (open while a reader is choosing pages and a destination), and it
//! is guarded against a job already in flight rather than folded into one —
//! see [`App::open_print_dialog`].

use std::path::PathBuf;

use iced::Task;

use super::{AfterFormCommit, App, Message, PrintMsg};

/// The print job in flight, if any.
///
/// There is no "ready to spool" state here: the moment a scratch copy lands,
/// its plan is handed straight to a deferred [`Message::SpoolPrint`] (§79.1)
/// rather than held in a field, so the job is either still being written,
/// or already with the platform.
#[derive(Debug)]
pub enum PrintJob {
    /// Nothing is printing.
    Idle,
    /// A marked-up copy is being written to scratch, picked up again at
    /// `Told::Saved`, before it is spooled.
    WritingScratch {
        scratch: PathBuf,
        pending: crate::printing::PrintPlan,
    },
    /// Handed to the platform: a system dialog is up, or a spooler is
    /// reading the file. Nothing else may print while this holds.
    InFlight,
}

impl PrintJob {
    pub fn is_idle(&self) -> bool {
        matches!(self, PrintJob::Idle)
    }
}

impl App {
    /// Ctrl+P, and everything the dialog it opens can be asked afterwards.
    pub(super) fn handle_print(&mut self, message: PrintMsg) -> Task<Message> {
        use crate::printing::PageChoice;

        match message {
            PrintMsg::Open => {
                self.close_menu_dropdowns();
                // A field holding the caret holds characters PDFium has not
                // committed yet, and the whole point of printing "as it is on
                // screen" is that those characters are on the paper. The
                // dialog opens from the tick once the commit has come back.
                if self
                    .ask_form_commit_first(AfterFormCommit::Print)
                    .succeeded()
                {
                    return Task::none();
                }
                self.open_print_dialog()
            }
            PrintMsg::Close => {
                self.print_dialog = None;
                Task::none()
            }
            PrintMsg::ChoosePages(choice) => {
                if let Some(dialog) = self.print_dialog.as_mut() {
                    dialog.choice = choice;
                }
                Task::none()
            }
            PrintMsg::TypeRange(text) => {
                if let Some(dialog) = self.print_dialog.as_mut() {
                    dialog.custom = text;
                    // Typing in the box is choosing it: a reader who types a
                    // range and presses Print meant the range, not "all".
                    dialog.choice = PageChoice::Custom;
                }
                Task::none()
            }
            PrintMsg::ChooseMarks(marks) => {
                if let Some(dialog) = self.print_dialog.as_mut() {
                    dialog.marks = marks;
                }
                Task::none()
            }
            PrintMsg::TypeCopies(text) => {
                if let Some(dialog) = self.print_dialog.as_mut() {
                    dialog.set_copies(&text);
                }
                Task::none()
            }
            PrintMsg::ChooseDestination(destination) => {
                if let Some(dialog) = self.print_dialog.as_mut() {
                    dialog.destination = destination;
                }
                Task::none()
            }
            PrintMsg::Destinations(names) => {
                // Only into the dialog that asked. A list that arrives after
                // the reader has closed the dialog, or printed, is nothing to
                // put anywhere.
                if let Some(dialog) = self.print_dialog.as_mut() {
                    if dialog.asks_particulars {
                        dialog.destinations = names;
                    }
                }
                Task::none()
            }
            PrintMsg::AcceptPermission => {
                if let Some(dialog) = self.print_dialog.as_mut() {
                    dialog.permission_answered = true;
                }
                Task::none()
            }
            PrintMsg::Spooled {
                outcome,
                title,
                destination,
                scratch,
            } => {
                self.print_spooled(outcome, title, destination, scratch);
                Task::none()
            }
            PrintMsg::Send => {
                let Some(dialog) = self.print_dialog.take() else {
                    return Task::none();
                };
                let Some(source) = self
                    .documents
                    .active()
                    .map(|document| document.path.clone())
                else {
                    self.notify("There is no document open to print.".into());
                    return Task::none();
                };
                let page_count = self.reader.page_count();
                let current = self.reader.current_page();
                // Asked once more here rather than trusted from the view: the
                // button is drawn from the same answer, but the document can
                // have closed between the draw and the press.
                if let Some(reason) = dialog.blocked(current, page_count) {
                    // Put the dialog back: the reader has something to
                    // correct, and taking it away would take the correction
                    // with it.
                    self.print_dialog = Some(dialog);
                    self.notify(reason);
                    return Task::none();
                }
                let Ok(plan) = dialog.plan(&source, current, page_count) else {
                    self.print_dialog = Some(dialog);
                    return Task::none();
                };
                if plan.needs_a_copy {
                    self.print_scratch_copy(&source, plan);
                    Task::none()
                } else {
                    self.spool(&source, &plan, false)
                }
            }
        }
    }

    /// Put the dialog up, with what this session can actually offer in it.
    pub(super) fn open_print_dialog(&mut self) -> Task<Message> {
        if !self.print_job.is_idle() {
            // A system print dialog is up, a spooler is reading, or the copy
            // one of them is about to be handed is still being written. A
            // second dialog behind any of those is two prints the reader
            // only asked for one of, and refusing at the Print button after
            // letting the dialog open is a worse way to say so.
            self.notify("A print is already on its way. Wait for it, then try again.".into());
            return Task::none();
        }
        if !self.platform.capabilities.printing {
            // Said, and said visibly, rather than a dialog that ends in
            // nothing. This is the whole reason the capability exists.
            self.notify(
                "Nothing in this session can print: pulpit found no spooler to hand the \
                 document to."
                    .into(),
            );
            return Task::none();
        }
        if self.documents.active().is_none() {
            self.notify("There is no document open to print.".into());
            return Task::none();
        }
        // A deck open for presenting has no document worker behind it, and
        // it is the worker that would write the copy and answer for the
        // permission bits. Said here rather than in a dialog whose Print
        // button could only fail.
        if self.reader_link.is_none() {
            self.notify(
                "This document is open for presenting only. Read it, and Ctrl+P prints from \
                 there."
                    .into(),
            );
            return Task::none();
        }
        // Who asks which pages, how many copies and which queue. The
        // desktop's own dialog asks all three where there is one, so pulpit
        // asks none of them and this one is left with the single question no
        // system dialog can ask.
        let asks_particulars = self.platform.capabilities.print_options
            && !self.platform.capabilities.system_print_dialog;
        // The dialog opens now and the queue list catches up. Asking the
        // spooler is a subprocess that talks to the network — `lpstat`
        // enumerates destinations it has only heard about — so on the event
        // loop it is a freeze of both windows for as long as one unreachable
        // print server takes to time out. The picker appears when the answer
        // does; until then the dialog means what an empty list has always
        // meant here, which is the default queue.
        let ask_destinations = if asks_particulars {
            let services = self.platform.services.clone();
            Task::perform(
                async move {
                    // Its own thread rather than the executor's blocking
                    // pool, for the reason spooling uses one: how long the
                    // spooler takes is not this process's to know, and an
                    // executor thread parked on it is one the rest of the
                    // application does not get back.
                    let (send, receive) = std::sync::mpsc::channel();
                    let spawned = std::thread::Builder::new()
                        .name("pulpit-printers".into())
                        .spawn(move || {
                            let _ = send.send(services.printers());
                        });
                    match spawned {
                        Ok(_) => receive.recv().unwrap_or_default(),
                        Err(error) => {
                            tracing::warn!(%error, "could not ask for the print queues");
                            Vec::new()
                        }
                    }
                },
                |names| Message::Print(PrintMsg::Destinations(names)),
            )
        } else {
            // A picker that cannot pick — or that the system dialog is about
            // to offer properly — is worse than no picker.
            Task::none()
        };
        let mut dialog = crate::printing::PrintDialog::open(
            Vec::new(),
            self.reader.can_undo(),
            asks_particulars,
        );
        // What the document itself asks for. Already answered for a document
        // whose properties have been read; asked for otherwise, and the
        // dialog shows the caution as soon as it lands. Not knowing is not
        // the same as being forbidden, so nothing waits on it.
        dialog.permission = self
            .document_properties
            .as_ref()
            .map(|properties| crate::printing::Permission::read(&properties.permissions));
        if dialog.permission.is_none() {
            self.ask_document_properties();
        }
        self.print_dialog = Some(dialog);
        ask_destinations
    }

    /// Ask the worker for the copy a marked-up print is spooled from.
    ///
    /// The same write Save As makes, to a scratch directory rather than
    /// somewhere the reader chose. It is picked up again at `Told::Saved`.
    fn print_scratch_copy(
        &mut self,
        source: &std::path::Path,
        pending: crate::printing::PrintPlan,
    ) {
        let directory = self.platform.services.directories().cache.join("print");
        // One at a time. The scratch name is this process's, so a second
        // print started while the first is still being written would target
        // the same bytes — and the answer that came back would be matched to
        // whichever plan was set last.
        if !self.print_job.is_idle() {
            self.notify("A print is already on its way. Wait for it, then try again.".into());
            return;
        }
        if let Err(error) = std::fs::create_dir_all(&directory) {
            self.notify(format!("pulpit could not make room for the print: {error}"));
            return;
        }
        let scratch = directory.join(crate::printing::spool_name(source, std::process::id()));
        // A6 all the same: the scratch name is derived from the source, and a
        // document already living in the cache directory could collide with
        // it. Printing must never write over what the reader opened.
        if Self::same_path(source, &scratch) {
            self.notify("pulpit cannot print this document from where it is.".into());
            return;
        }
        let Some(link) = self.reader_link.as_mut() else {
            self.notify("There is no document open to print.".into());
            return;
        };
        link.ask(crate::reader_link::Ask::SaveAs {
            destination: scratch.clone(),
            options: pulpit_render::document::SaveOptions::verified(),
        });
        self.print_job = PrintJob::WritingScratch { scratch, pending };
    }

    /// The scratch copy exists. Spool it, then take it away again.
    ///
    /// Called from [`App::print_scratch_matches`], the only place this is
    /// reached: the pending `Told::Saved` answer matcher, shared with
    /// signing's own scratch copy, lives in `app.rs` and goes through that
    /// method instead of calling this one directly.
    fn print_scratch_landed(&mut self, path: PathBuf) {
        let PrintJob::WritingScratch { pending, .. } =
            std::mem::replace(&mut self.print_job, PrintJob::Idle)
        else {
            // Nothing asked for this copy, which should not happen; deleting
            // it is still the right thing to do with it.
            let _ = std::fs::remove_file(&path);
            return;
        };
        // Queued rather than spooled directly: the pump this lands on has no
        // way to return the `Task` that spooling is now (§79.1).
        self.deferred.push(Message::SpoolPrint(path, pending));
    }

    /// Hand a file to the platform, on a thread, and say what came of it.
    ///
    /// Two paths, chosen by what the session can do rather than by what it is
    /// running on:
    ///
    /// - a desktop with a print dialog of its own gets the file and the
    ///   title, and asks the reader everything else itself;
    /// - a desktop with only a spooler gets the pages, the copies and the
    ///   queue that pulpit's own dialog asked for, because nothing else was
    ///   going to ask.
    ///
    /// Either way the call blocks — the first for as long as a person looks
    /// at a dialog — so neither is made here. Both go to a thread, and the
    /// answer arrives as [`PrintMsg::Spooled`].
    pub(super) fn spool(
        &mut self,
        file: &std::path::Path,
        pending: &crate::printing::PrintPlan,
        scratch: bool,
    ) -> Task<Message> {
        let job = crate::platform::services::PrintJob {
            file: file.to_path_buf(),
            title: pending.title.clone(),
            pages: pending.pages.ranges().to_vec(),
            copies: pending.copies,
            destination: pending.destination.clone(),
        };
        let system_dialog = self.platform.capabilities.system_print_dialog;
        let services = std::sync::Arc::clone(&self.platform.services);
        let title = pending.title.clone();
        // Not named where the system dialog chose the queue: pulpit did not
        // pick it and cannot read it back, and "Sent to HP_LaserJet" that
        // names the queue pulpit *would* have used is a lie about where the
        // paper is.
        let destination = if system_dialog {
            None
        } else {
            pending.destination.clone()
        };
        let scratch = scratch.then(|| file.to_path_buf());
        // The dialog is up, or the spooler is reading; a second print started
        // over the top of it would be a second dialog and, for a marked-up
        // print, a second write to the same scratch name.
        self.print_job = PrintJob::InFlight;

        // A panel AppKit will only run on the thread that owns the event
        // loop. Called in place, and pulpit's own drawing stops until the
        // reader closes it — which is the cost of that platform having no
        // other way to show its print dialog. The audience window keeps the
        // last complete frame it had throughout, so the third standing rule
        // is not what this spends.
        if system_dialog && self.platform.services.print_dialog_wants_main_thread() {
            let outcome = self.platform.services.print_with_dialog(&job);
            return Task::done(Message::Print(PrintMsg::Spooled {
                outcome,
                title,
                destination,
                scratch,
            }));
        }

        Task::perform(
            async move {
                // A thread rather than the async runtime's blocking pool
                // because the wait is unbounded: it ends when a person
                // decides, and an executor thread parked on that is one the
                // rest of the application does not get back.
                let (send, receive) = std::sync::mpsc::channel();
                std::thread::Builder::new()
                    .name("pulpit-print".into())
                    .spawn(move || {
                        let outcome = if system_dialog {
                            services.print_with_dialog(&job)
                        } else {
                            services.print(&job)
                        };
                        let _ = send.send(outcome);
                    })
                    .map_err(|e| format!("the print could not be started: {e}"))
                    .and_then(|_| {
                        receive
                            .recv()
                            .map_err(|_| "the print ended without saying how".to_string())
                    })
                    .unwrap_or_else(crate::platform::Outcome::failed)
            },
            move |outcome| {
                Message::Print(PrintMsg::Spooled {
                    outcome,
                    title: title.clone(),
                    destination: destination.clone(),
                    scratch: scratch.clone(),
                })
            },
        )
    }

    /// The platform is done with the job.
    fn print_spooled(
        &mut self,
        outcome: crate::platform::Outcome,
        title: String,
        destination: Option<String>,
        scratch: Option<PathBuf>,
    ) {
        self.print_job = PrintJob::Idle;
        // The copy is pulpit's, and it goes whether the job was taken or not:
        // a scratch file left behind after a refused print is a copy of a
        // document the reader never asked for, sitting in a cache directory.
        // Both paths have finished reading it by the time the answer is here
        // — `lp` reads before it queues, and the portal dups the descriptor —
        // which is why both of them wait rather than spawn.
        if let Some(scratch) = scratch {
            if let Err(error) = std::fs::remove_file(&scratch) {
                tracing::warn!(%error, path = %scratch.display(), "the print copy could not be removed");
            }
        }
        match outcome {
            // Cancelling a print dialog is a thing the reader did on purpose,
            // and reporting it as a failure would tell them their own
            // decision went wrong. Nothing is said at all.
            crate::platform::Outcome::Refused { .. } => {}
            crate::platform::Outcome::Done => {
                let where_to = match destination.as_deref() {
                    Some(queue) => format!(" to {queue}"),
                    None => String::new(),
                };
                self.notify_done(format!("Sent “{title}”{where_to} to print."));
            }
            other => {
                if let Some(problem) = other.describe() {
                    self.notify_error(format!("The print did not go: {problem}"), None);
                }
            }
        }
    }

    /// Whether `saved.path` is the scratch copy this module is waiting for;
    /// if it is, land it and report `true` so the caller — the shared
    /// `Told::Saved` answer matcher in `app.rs`, which also watches for
    /// signing's own scratch copy — knows not to treat it as an ordinary
    /// save.
    pub(super) fn print_scratch_matches(&mut self, path: &std::path::Path) -> bool {
        let matches = matches!(
            &self.print_job,
            PrintJob::WritingScratch { scratch, .. } if scratch.as_path() == path
        );
        if matches {
            self.print_scratch_landed(path.to_path_buf());
        }
        matches
    }

    /// Drop a scratch copy still being written, and the plan that named it.
    /// Does nothing when the job is `Idle` or already `InFlight`: a job
    /// already handed to the platform is out of reach, and pretending
    /// otherwise would let a second dialog open behind the first one.
    pub(super) fn abandon_print_scratch(&mut self) {
        if matches!(self.print_job, PrintJob::WritingScratch { .. }) {
            if let PrintJob::WritingScratch { scratch, .. } =
                std::mem::replace(&mut self.print_job, PrintJob::Idle)
            {
                let _ = std::fs::remove_file(scratch);
            }
        }
    }

    /// Drop whatever print job was in progress because the document it was
    /// for is going away, and close the dialog with it.
    pub(super) fn abandon_print_job(&mut self) {
        self.print_dialog = None;
        self.abandon_print_scratch();
    }
}
