//! The thread that talks to the document worker.
//!
//! The document session is a synchronous request-and-answer channel over a
//! pipe (`pulpit_render::document::session`), and the application draws on the
//! event-loop thread. Calling one from the other would put a PDF render — and,
//! once §8.6 lands, a keystroke round trip — inside a view pass.
//!
//! So the session lives on a thread of its own and the application talks to it
//! the way it talks to every other out-of-process thing here: it posts work and
//! collects answers on its existing tick. Two rules make that safe rather than
//! merely asynchronous:
//!
//! * **Nothing is assumed.** An answer says what happened; a request that got
//!   no answer is not a mutation that succeeded (§11.5).
//! * **Answers name their revision.** A frame carries the revision it
//!   contains, so a preview is removed when a frame at or beyond the
//!   mutation's revision arrives and not before (A7).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;

use pulpit_core::page::{PageGeometry, PageIndex};
use pulpit_render::document::protocol::{DocumentRequest, DocumentResponse};
use pulpit_render::document::session::{DocumentSession, DocumentWorkerCommand, SessionError};
use pulpit_render::document::{
    Applied, DocumentRevision, DocumentTransaction, OpenDocumentInfo, SaveOptions,
};

/// Something the application wants the document worker to do.
///
/// Save As is here and unreached: the reader's toolbar posts the intent and
/// the file chooser that turns it into a destination is the next step of
/// §14.3. It is written now because the worker already answers it, and a
/// request the worker can answer but nothing can send is the kind of gap that
/// goes unnoticed.
///
/// A small vocabulary on purpose: everything the reader needs and nothing it
/// does not, so a request that cannot be answered is a compile error rather
#[allow(dead_code)] // Save As: see the note above
/// than a message that goes nowhere.
#[derive(Debug)]
pub enum Ask {
    /// Everything a freshly opened document needs, in one exchange: what it
    /// is, and how big its pages are. One message rather than three because
    /// the reader can do nothing with any of them alone.
    Describe { pages: usize },
    /// Write the document as it now stands to a scratch path, so the render
    /// worker pool can draw pages that contain every committed edit (A7).
    ///
    /// The same save the worker already performs for Save As, aimed at a
    /// file pulpit owns; A6 still holds because the destination is never the
    /// source. Verification is skipped: the snapshot is consumed by pulpit's
    /// own renderer moments later, which is a better check than reopening it
    /// here, and the reader is waiting on the round trip.
    Snapshot { destination: PathBuf },
    /// What is on a page, for hit-testing. The eraser and the selection tool
    /// need to know what is under the pointer, and only the document does.
    ListAnnotations { page: pulpit_core::page::PageIndex },
    /// Every AcroForm field, in the order the file lists them (§6.4).
    ///
    /// Read-only: it never moves the revision. Asked once when the document is
    /// described and again after a commit, because PDFium is the sole author
    /// of a value and a list this process patched would be a second opinion.
    ListFields,
    /// What the document says about itself, for the properties dialog.
    ///
    /// Asked when the dialog is opened rather than when the document is
    /// described: it is a question a presenter putting a deck on a projector
    /// never asks, and the worker answers one request at a time.
    Properties,
    /// Resolve a text selection. Read-only: it never moves the revision
    /// (§6.3). `finalising` marks the query a release is waiting on, so the
    /// answer that commits a highlight is told apart from the ones that only
    /// keep the live selection drawn.
    SelectText {
        page: pulpit_core::page::PageIndex,
        selection: pulpit_render::document::TextSelection,
        finalising: bool,
    },
    /// Read the text inside a rectangle on one page. Read-only: it never
    /// moves the revision (§6.3). Not coalesced the way a selection is —
    /// there is one of these per band, asked when the pointer comes up.
    AreaText {
        page: pulpit_core::page::PageIndex,
        rect: pulpit_core::page::PageRect,
    },
    /// One page's whole text layer, so speech can read it (issue #20).
    ///
    /// Read-only, and it never moves the revision. The page travels back with
    /// the answer because speech may have turned the page while this was in
    /// flight, and the reading cursor drops text for a page it has left.
    PageText { page: pulpit_core::page::PageIndex },
    /// Find a string in a run of pages. Read-only, and carried with the
    /// generation it belongs to: the answer to a query the user has already
    /// typed past has to be recognisable as stale on arrival.
    FindText {
        generation: pulpit_core::search::SearchGeneration,
        query: pulpit_core::search::Query,
        from_page: usize,
        to_page: usize,
    },
    /// One raw input event for the document's own form fields (§8.6).
    ///
    /// Deliberately *not* a "set this field to that value" request. Field
    /// values are edited in place by PDFium's form-fill environment, under the
    /// field's own `/DA`, so what travels is the press or the keystroke and
    /// never a value pulpit composed. That is what keeps one editing surface
    /// rather than two.
    ///
    /// No optimistic revision check rides along, unlike [`Ask::Apply`]. Most
    /// of these events commit nothing — a caret move, a key the field ignores
    /// — and the ones that do commit are answered with the revision they
    /// produced. A keystroke that had to name the revision it expected would
    /// be a keystroke that could be refused for arriving in the wrong order,
    /// which is not how typing behaves anywhere.
    FormEvent {
        page: pulpit_core::page::PageIndex,
        event: pulpit_render::document::protocol::FormInputEvent,
    },
    Apply {
        expected_revision: DocumentRevision,
        transaction: DocumentTransaction,
    },
    Undo {
        expected_revision: DocumentRevision,
        operation: pulpit_render::document::DocumentUndo,
    },
    SaveAs {
        destination: PathBuf,
        options: SaveOptions,
    },
    /// Draw the rectangle an edit changed, from the document the worker
    /// holds, so the page can show that edit without waiting for a snapshot.
    ///
    /// The §9.4 partial repaint. It is the same renderer drawing the same
    /// document that the snapshot would have been taken from, so what comes
    /// back is what the full render would have put in that rectangle — a crop,
    /// not a second opinion. A full frame from a snapshot remains the correct
    /// baseline and supersedes every patch on its page; a patch that fails or
    /// arrives late costs nothing but the wait it was trying to save.
    RenderPatch {
        page: pulpit_core::page::PageIndex,
        region: pulpit_core::notes::Region,
        width: u32,
        height: u32,
        /// The full-page frame the crop is going to be drawn over. Sent
        /// rather than left to the worker to reconstruct from the region: the
        /// two roundings disagree by up to a pixel, and a patch drawn at a
        /// scale the frame beneath it was not drawn at shimmers as the
        /// rectangle grows with each keystroke (§9.4).
        ///
        /// The size the crop is *rendered* at, not a size the answer must
        /// match: the patch is placed by its region in page space and scaled
        /// by the layout, so an answer that comes back against a frame size
        /// the page has since left is drawn very slightly soft rather than
        /// dropped — which would take the typed characters off the screen.
        frame_width: u32,
        frame_height: u32,
    },
}

/// Something the document worker had to say.
#[derive(Debug)]
pub enum Told {
    /// The worker is up and this is the document it holds.
    Described {
        info: Box<OpenDocumentInfo>,
        geometry: Vec<PageGeometry>,
        outline: pulpit_core::navigation::Outline,
    },
    /// A snapshot landed at the destination, at this revision.
    Snapshotted(pulpit_render::document::SavedDocument),
    /// …or it did not, and rendering carries on from the previous one.
    SnapshotFailed {
        message: String,
    },
    Annotations {
        page: pulpit_core::page::PageIndex,
        summaries: Vec<pulpit_render::document::AnnotationSummary>,
    },
    /// The document's fields as the engine now holds them.
    Fields(Vec<pulpit_render::document::FormField>),
    /// What the document says about itself.
    Properties(Box<pulpit_render::document::DocumentProperties>),
    /// …or the reason it would not say. Its own case, like a refused snapshot,
    /// because a dialog is waiting on this particular answer.
    PropertiesFailed {
        message: String,
    },
    Selection {
        result: pulpit_render::document::TextSelectionResult,
        finalising: bool,
    },
    /// The text a band covered, on its way to the clipboard.
    AreaText {
        text: String,
        truncated: bool,
    },
    /// One page's text, or the empty string when the page has none.
    PageText {
        page: pulpit_core::page::PageIndex,
        text: String,
    },
    /// This document has no text layer to read aloud at all — an image
    /// directory, a scan. Said once, rather than as an empty page every time.
    CannotSpeak {
        reason: String,
    },
    /// Hits for one run of pages, or the reason there will not be any.
    ///
    /// The generation travels back so the model can drop what belongs to a
    /// superseded query; the worker is not asked to cancel, because a chunk is
    /// a handful of pages and finishing it is cheaper than coordinating.
    Found {
        generation: pulpit_core::search::SearchGeneration,
        chunk: pulpit_core::search::HitChunk,
    },
    /// The document cannot be searched at all — no text layer the backend can
    /// read. Not the same as finding nothing, and said differently.
    CannotSearch {
        generation: pulpit_core::search::SearchGeneration,
        message: String,
    },
    /// What a form event changed: rectangles to redraw, a value it committed,
    /// and anything the document's own JavaScript asked the host to do.
    FormChanged {
        page: pulpit_core::page::PageIndex,
        result: Box<pulpit_render::document::protocol::FormEventResult>,
        /// Whether this answers a pointer *move*.
        ///
        /// The application keeps at most one move in flight, and the slot it
        /// waits on is a slot for moves: a key, a copy or a paste answering
        /// while a move is still out would otherwise release it and let a
        /// second move go out behind the first. Read from the event that was
        /// sent rather than carried by the caller, because the kind of a
        /// request is a property of the request.
        moved: bool,
    },
    /// A form event the worker would not take — a document with no fillable
    /// form, a page that would not load, or permissions that forbid the change.
    /// Nothing changed, and saying so is what keeps the caller's one-in-flight
    /// guard from latching shut.
    ///
    /// `refusal` carries the document's own reason when there is one — the
    /// permissions case, which the reader must be *told* about rather than see
    /// as a field that quietly ignores typing. `None` is the ordinary "nothing
    /// here to take it", which is not worth a word.
    FormRefused {
        refusal: Option<String>,
        /// Whether this answers a pointer move — see [`Told::FormChanged`].
        moved: bool,
    },
    Applied(Box<Applied>),
    /// A mutation — a transaction, an undo or a redo — was not applied.
    ///
    /// Its own case rather than a bare `Failed`, for the same reason a refused
    /// snapshot has one: the reader is keeping a record per mutation in flight,
    /// and it must know *which* request a failure answered rather than guess.
    /// A refused undo also has an operation to put back on its stack.
    EditFailed {
        message: String,
        fatal: bool,
    },
    /// The rectangle an edit changed, drawn from the worker's document. Held
    /// over the page's frame until a full frame containing the same revision
    /// arrives (§9.2, §9.4).
    Patched(Box<pulpit_render::document::protocol::DocumentFrame>),
    /// …or the worker would not draw it. Nothing is reported to the reader — a
    /// patch is an optimisation over the snapshot that is coming anyway — but
    /// the request stops being outstanding, which is what the page needs to
    /// know before it asks for the next one.
    PatchRefused {
        page: pulpit_core::page::PageIndex,
    },
    Saved {
        saved: pulpit_render::document::SavedDocument,
        /// The required fields (`/Ff` bit 2) still holding nothing when the
        /// copy was written. Told, never enforced: pulpit is not the form's
        /// submit button, but a copy quietly missing what the document says it
        /// needs is worth one sentence.
        unfilled_required: Vec<String>,
    },
    /// Something was refused, or the worker went. `fatal` is the difference
    /// that matters: a refusal is an answer and the session carries on; a lost
    /// worker means nothing more will be answered until the document is
    /// reopened, and no mutation in flight may be assumed committed.
    Failed {
        message: String,
        fatal: bool,
    },
}

/// A document answer is waiting on [`ReaderLink`].
///
/// This is deliberately a one-slot doorbell rather than another delivery
/// channel. Answers remain ordered on `ReaderLink::told`; a burst merely asks
/// the event loop to drain that channel once.
pub use pulpit_core::ipc::Doorbell as ReaderWakeup;
use pulpit_core::ipc::Sink as WakeupSink;

/// The application's end of the conversation.
pub struct ReaderLink {
    asks: Sender<Ask>,
    told: Receiver<Told>,
    source: PathBuf,
    /// Set when the worker is gone. The link is not restarted here: reopening
    /// is a decision about the journal, not about a channel (§11.5).
    lost: bool,
    /// How many requests have been sent and not yet answered.
    ///
    /// One number rather than a flag per kind of request, because every [`Ask`]
    /// is answered exactly once — [`handle`] returns one [`Told`] per ask, and
    /// [`describe`] folds its several round trips into one answer too. So
    /// "something is in flight" is arithmetic here rather than a list of
    /// conditions somewhere else that a new kind of request can be forgotten
    /// from; that omission is what once made typing in a form settle to the
    /// slow tick between keystrokes.
    ///
    /// It cannot be forced to zero and it cannot latch: it is incremented only
    /// where a send succeeded and decremented only where an answer was taken,
    /// and when the worker is lost the count goes with the link.
    outstanding: usize,
    wakeup_inbox: Option<Arc<ReaderWakeup>>,
}

impl std::fmt::Debug for ReaderLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderLink")
            .field("source", &self.source)
            .field("lost", &self.lost)
            .finish()
    }
}

#[allow(dead_code)] // `source` and `is_lost` are read by the recovery path
impl ReaderLink {
    /// Start a worker for `source` and the thread that talks to it.
    ///
    /// Returns as soon as the worker has answered its handshake, so a document
    /// that cannot be opened is reported here rather than as silence.
    pub fn open(source: &Path) -> Result<ReaderLink, SessionError> {
        let (ask_sender, ask_receiver) = std::sync::mpsc::channel::<Ask>();
        let (told_sender, told_receiver) = std::sync::mpsc::channel::<Told>();
        let (wakeup, wakeup_receiver) = pulpit_core::ipc::doorbell();

        // The worker is started on the link's own thread, not here. Starting
        // it means spawning the process and waiting out its handshake — a
        // third of a second of dynamic linking and PDFium coming up — and
        // every caller of `open` is on a path that has better things to do
        // with that time. A start that fails becomes a fatal `Told`, which
        // is the same shape any later loss of the worker takes.
        let source_for_thread = source.to_path_buf();
        std::thread::Builder::new()
            .name("document-session".into())
            .spawn(move || {
                let session = match DocumentSession::start(
                    &DocumentWorkerCommand::default(),
                    &source_for_thread,
                ) {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = told_sender.send(Told::Failed {
                            message: format!("the document worker did not start: {error}"),
                            fatal: true,
                        });
                        wakeup.ring();
                        return;
                    }
                };
                serve(session, ask_receiver, told_sender, wakeup);
                tracing::debug!(source = %source_for_thread.display(), "document session ended");
            })
            .map_err(SessionError::Spawn)?;

        Ok(ReaderLink {
            asks: ask_sender,
            told: told_receiver,
            source: source.to_path_buf(),
            lost: false,
            outstanding: 0,
            wakeup_inbox: Some(Arc::new(wakeup_receiver)),
        })
    }

    /// Take the single event-loop listener for this document session.
    pub fn take_wakeup(&mut self) -> Option<Arc<ReaderWakeup>> {
        self.wakeup_inbox.take()
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Post work. Returns `false` when the session is gone, so a caller can
    /// tell "sent" from "there was nobody to send it to".
    pub fn ask(&mut self, ask: Ask) -> bool {
        if self.lost {
            return false;
        }
        if self.asks.send(ask).is_err() {
            self.lost = true;
            self.outstanding = 0;
            return false;
        }
        self.outstanding += 1;
        true
    }

    /// Is the worker owed an answer? What [`crate::app::App::is_live`] asks
    /// instead of enumerating every kind of request that might be out.
    pub fn is_busy(&self) -> bool {
        self.outstanding > 0
    }

    /// Everything the worker has said since the last time it was asked.
    ///
    /// Drains rather than taking one, because the application collects on a
    /// tick and a queue that grows by more than one per tick would never empty.
    pub fn collect(&mut self) -> Vec<Told> {
        self.collect_bounded(usize::MAX).0
    }

    /// Collect at most `limit` answers, returning whether another event-loop
    /// turn should continue the drain.
    pub fn collect_bounded(&mut self, limit: usize) -> (Vec<Told>, bool) {
        let mut answers = Vec::new();
        while answers.len() < limit {
            match self.told.try_recv() {
                Ok(told) => {
                    // One answer per ask, so one answer is one request no
                    // longer outstanding.
                    self.outstanding = self.outstanding.saturating_sub(1);
                    if matches!(&told, Told::Failed { fatal: true, .. }) {
                        self.lost = true;
                        self.outstanding = 0;
                    }
                    answers.push(told);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Nothing that was out will ever be answered now.
                    self.lost = true;
                    self.outstanding = 0;
                    break;
                }
            }
        }
        // Reaching the budget is enough to request one continuation. It may
        // be a harmless empty pass when the queue contained exactly `limit`
        // answers, and avoids pulling an answer out merely to peek.
        let more = answers.len() == limit;
        (answers, more)
    }
}

/// The thread body: one request at a time, in the order they were posted.
///
/// Deliberately serial. The worker holds one document and answers one thing at
/// a time; a queue in front of it would only reorder the wait, and ordering is
/// what makes the optimistic revision check mean anything (§9.5).
fn serve(
    mut session: DocumentSession,
    asks: Receiver<Ask>,
    told: Sender<Told>,
    wakeup: WakeupSink,
) {
    // Work already posted, read ahead of time so a search can see whether it
    // has been superseded before it is run. Order is otherwise preserved
    // exactly: this queue is drained before the channel is read again.
    let mut queued: std::collections::VecDeque<Ask> = std::collections::VecDeque::new();
    loop {
        let ask = match queued.pop_front() {
            Some(ask) => ask,
            None => match asks.recv() {
                Ok(ask) => ask,
                Err(_) => break,
            },
        };
        // A scan whose query the reader has already typed past is thirty-two
        // pages of PDF the newer query then waits behind. Its answer would be
        // dropped on arrival for its generation; drop it here instead, and
        // answer cheaply so that one answer still comes back for one ask.
        let ask = match superseded(ask, &asks, &mut queued) {
            Ok(stale) => {
                if told.send(stale).is_err() {
                    return;
                }
                wakeup.ring();
                continue;
            }
            Err(ask) => ask,
        };
        let answers = handle(&mut session, ask);
        let fatal = answers
            .iter()
            .any(|answer| matches!(answer, Told::Failed { fatal: true, .. }));
        for answer in answers {
            // The application has gone: so should this thread, and the session
            // with it — the worker exits when its pipe closes.
            if told.send(answer).is_err() {
                return;
            }
            wakeup.ring();
        }
        if fatal {
            return;
        }
    }
    session.close();
}

/// Answer a search that a newer one has already replaced, without running it.
///
/// Returns `Ok(answer)` when `ask` is a [`Ask::FindText`] and a *later*
/// generation of search is already posted behind it, and `Err(ask)` — the
/// work, to be done — otherwise. Only a later generation supersedes: several
/// chunks of the same scan are in flight at once by design, and dropping one
/// of those would leave a run of pages unread.
///
/// Everything read out of the channel to decide this is left in `queued`, in
/// the order it was posted. Nothing is reordered and nothing is lost.
fn superseded(
    ask: Ask,
    asks: &Receiver<Ask>,
    queued: &mut std::collections::VecDeque<Ask>,
) -> Result<Told, Ask> {
    let Ask::FindText { generation, .. } = &ask else {
        return Err(ask);
    };
    let generation = *generation;
    queued.extend(asks.try_iter());
    let replaced = queued.iter().any(|later| match later {
        Ask::FindText {
            generation: newer, ..
        } => *newer > generation,
        _ => false,
    });
    if !replaced {
        return Err(ask);
    }
    let Ask::FindText {
        from_page, to_page, ..
    } = ask
    else {
        unreachable!("the ask was matched as a search above");
    };
    // An empty chunk under a generation the application has already left
    // behind: it is dropped by the model on arrival, and the ask is answered.
    Ok(Told::Found {
        generation,
        chunk: pulpit_core::search::HitChunk {
            from_page,
            to_page,
            hits: Vec::new(),
            truncated: false,
        },
    })
}

fn handle(session: &mut DocumentSession, ask: Ask) -> Vec<Told> {
    match ask {
        Ask::Describe { pages } => describe(session, pages),
        Ask::Snapshot { destination } => vec![match session.request(DocumentRequest::SaveAs(
            pulpit_render::document::protocol::SaveRequest {
                destination,
                // Incremental: PDFium appends the changed objects to the
                // original byte stream instead of re-serialising the whole
                // document, which is the cheap way to produce a copy that is
                // read back by pulpit's own renderer moments later.
                options: SaveOptions {
                    incremental: true,
                    verify: false,
                },
            },
        )) {
            Ok(DocumentResponse::Saved(saved)) => Told::Snapshotted(saved),
            // A snapshot that failed is not a lost worker and not a lost
            // edit: the document still holds the commit, and the reader
            // keeps showing the previous picture. Reported as its own case
            // so the caller can clear its in-flight mark rather than guess
            // which request the failure answered.
            Err(error) if !error.is_worker_loss() => Told::SnapshotFailed {
                message: error.to_string(),
            },
            other => unexpected(other, "a snapshot"),
        }],
        Ask::RenderPatch {
            page,
            region,
            width,
            height,
            frame_width,
            frame_height,
        } => vec![match session.request(DocumentRequest::Render(
            pulpit_render::document::protocol::DocumentRenderRequest {
                page,
                width,
                height,
                region,
                full_width: frame_width,
                full_height: frame_height,
            },
        )) {
            Ok(DocumentResponse::Frame(frame)) => Told::Patched(frame),
            // A patch is an optimisation over waiting for the snapshot
            // that is coming anyway, so a failure is not reported to the
            // reader: the page keeps its previews and the snapshot lands
            // as it would have.
            Err(error) if !error.is_worker_loss() => {
                tracing::debug!(%error, "a partial repaint was refused");
                // Said rather than swallowed. The application keeps one patch
                // per page in flight and matches answers to requests in the
                // order they were asked for; a refusal that said nothing would
                // leave the request outstanding for ever, and every later
                // answer for that page would be read as the answer to it.
                Told::PatchRefused { page }
            }
            other => unexpected(other, "a page patch"),
        }],
        Ask::ListAnnotations { page } => vec![match session
            .request(DocumentRequest::ListAnnotations { page })
        {
            Ok(DocumentResponse::Annotations(summaries)) => Told::Annotations { page, summaries },
            other => unexpected(other, "an annotation list"),
        }],
        Ask::ListFields => vec![match session.request(DocumentRequest::ListFields) {
            Ok(DocumentResponse::Fields(fields)) => Told::Fields(fields),
            other => unexpected(other, "a field list"),
        }],
        Ask::Properties => vec![match session.request(DocumentRequest::Properties) {
            Ok(DocumentResponse::Properties(properties)) => Told::Properties(properties),
            // A refusal is an answer: the dialog is waiting on this one, and a
            // generic failure would leave it reading "Reading…" for as long as
            // it stayed open.
            Err(error) if !error.is_worker_loss() => Told::PropertiesFailed {
                message: error.to_string(),
            },
            other => unexpected(other, "document properties"),
        }],
        Ask::SelectText {
            page,
            selection,
            finalising,
        } => vec![
            match session.request(DocumentRequest::SelectText { page, selection }) {
                Ok(DocumentResponse::Selection(result)) => Told::Selection { result, finalising },
                other => unexpected(other, "a text selection"),
            },
        ],
        Ask::AreaText { page, rect } => {
            vec![
                match session.request(DocumentRequest::AreaText { page, rect }) {
                    Ok(DocumentResponse::AreaText { text, truncated }) => {
                        Told::AreaText { text, truncated }
                    }
                    // A backend or a page with no text layer answers this way,
                    // and it is a fact about the region rather than a lost
                    // worker: the band gets told there was no text in it.
                    Ok(DocumentResponse::Failed(
                        pulpit_render::document::protocol::DocumentFailure::Unsupported(_),
                    )) => Told::AreaText {
                        text: String::new(),
                        truncated: false,
                    },
                    other => unexpected(other, "the text in an area"),
                },
            ]
        }
        Ask::PageText { page } => vec![match session.request(DocumentRequest::PageText { page }) {
            Ok(DocumentResponse::PageText(text)) => Told::PageText { page, text },
            // A backend with no text layer is a standing fact about this
            // document, not a failed request — the same distinction search
            // makes. Speech says it once and stops, rather than treating
            // every page as empty.
            Ok(DocumentResponse::Failed(
                pulpit_render::document::protocol::DocumentFailure::Unsupported(reason),
            )) => Told::CannotSpeak { reason },
            other => unexpected(other, "a page's text"),
        }],
        Ask::FindText {
            generation,
            query,
            from_page,
            to_page,
        } => vec![match session.request(DocumentRequest::FindText {
            query,
            from_page,
            to_page,
        }) {
            Ok(DocumentResponse::Found(chunk)) => Told::Found { generation, chunk },
            // A backend with no text layer is a standing fact about this
            // document, not a failed request: it is reported once, in the
            // search box, rather than as a diagnostic per chunk.
            Ok(DocumentResponse::Failed(
                pulpit_render::document::protocol::DocumentFailure::Unsupported(message),
            )) => Told::CannotSearch {
                generation,
                message,
            },
            other => unexpected(other, "search results"),
        }],
        Ask::FormEvent { page, event } => {
            let moved = matches!(
                &event,
                pulpit_render::document::protocol::FormInputEvent::PointerMove { .. }
            );
            match session.request(DocumentRequest::FormEvent { page, event }) {
                Ok(DocumentResponse::Form(result)) => vec![Told::FormChanged {
                    page,
                    result,
                    moved,
                }],
                // A refused keystroke is not a lost worker and not a lost
                // edit: the field simply did not take it. Reported at debug
                // level rather than as a failure banner, because a document
                // with no fillable form answers this way for every stray
                // click on the page, and a banner per click would be noise.
                Err(error) if !error.is_worker_loss() => {
                    // Answered even though nothing happened. The application
                    // keeps at most one pointer move in flight and waits for
                    // an answer before sending the next; a refusal that said
                    // nothing would latch that guard shut and the form would
                    // stop following the pointer for the rest of the session.
                    tracing::debug!(%error, "a form event was refused");
                    // A document that refused the change on its own terms —
                    // permissions, a read-only field — said so, and that
                    // sentence belongs on screen. Everything else is the
                    // ordinary "no form here" and stays at debug level.
                    let refusal = match &error {
                        pulpit_render::document::session::SessionError::Refused(
                            pulpit_render::document::protocol::DocumentFailure::Refused(why),
                        ) => Some(why.clone()),
                        _ => None,
                    };
                    vec![Told::FormRefused { refusal, moved }]
                }
                other => vec![unexpected(other, "a form event result")],
            }
        }
        Ask::Apply {
            expected_revision,
            transaction,
        } => vec![match session.request(DocumentRequest::Apply {
            expected_revision,
            transaction,
        }) {
            Ok(DocumentResponse::Applied(applied)) => Told::Applied(applied),
            other => mutation_failed(unexpected(other, "an applied transaction")),
        }],
        Ask::Undo {
            expected_revision,
            operation,
        } => vec![match session.request(DocumentRequest::Undo {
            expected_revision,
            operation,
        }) {
            Ok(DocumentResponse::Applied(applied)) => Told::Applied(applied),
            other => mutation_failed(unexpected(other, "an undone action")),
        }],
        Ask::SaveAs {
            destination,
            options,
        } => vec![match session.request(DocumentRequest::SaveAs(
            pulpit_render::document::protocol::SaveRequest {
                destination,
                options,
            },
        )) {
            Ok(DocumentResponse::Saved(saved)) => {
                // What the document says it still needs, read after the write
                // so the answer describes the copy that was actually made. A
                // form that cannot be listed simply reports nothing missing.
                let unfilled_required = match session.request(DocumentRequest::ListFields) {
                    Ok(DocumentResponse::Fields(fields)) => fields
                        .into_iter()
                        .filter(|field| {
                            field.required && field.value.is_empty() && field.selected.is_empty()
                        })
                        .map(|field| field.name)
                        .collect(),
                    _ => Vec::new(),
                };
                Told::Saved {
                    saved,
                    unfilled_required,
                }
            }
            other => unexpected(other, "a saved document"),
        }],
    }
}

/// Ask for the document's shape, in as few round trips as it takes.
fn describe(session: &mut DocumentSession, pages: usize) -> Vec<Told> {
    tracing::debug!("describe begins");
    let info = match session.request(DocumentRequest::Info) {
        Ok(DocumentResponse::Opened(info)) => info,
        other => return vec![unexpected(other, "document info")],
    };

    // The geometry answer is bounded, so a long document takes several
    // exchanges. Asking for the whole thing and getting a truncated answer
    // without noticing is exactly the bug the bound exists to prevent.
    let wanted = if pages == 0 { info.page_count } else { pages };
    let wanted = wanted.min(info.page_count);
    let mut geometry = Vec::with_capacity(wanted);
    while geometry.len() < wanted {
        let request = DocumentRequest::PageGeometries {
            from: PageIndex(geometry.len()),
            count: (wanted - geometry.len())
                .min(pulpit_render::document::protocol::MAX_PAGE_GEOMETRIES),
        };
        match session.request(request) {
            Ok(DocumentResponse::PageGeometries(run)) if !run.is_empty() => {
                geometry.extend(run);
            }
            // An empty run would loop forever; a document that will not
            // measure its own pages is one the reader cannot lay out.
            Ok(DocumentResponse::PageGeometries(_)) => {
                return vec![Told::Failed {
                    message: format!(
                        "the document stopped measuring its pages after {}",
                        geometry.len()
                    ),
                    fatal: false,
                }]
            }
            other => return vec![unexpected(other, "page geometries")],
        }
    }

    let outline = match session.request(DocumentRequest::Outline) {
        Ok(DocumentResponse::Outline(outline)) => outline,
        // A document without bookmarks is not a failure, and neither is a
        // build that cannot read them; the rail says it has none.
        _ => Default::default(),
    };
    tracing::debug!(pages = geometry.len(), "describe measured");
    vec![Told::Described {
        info,
        geometry,
        outline,
    }]
}

/// Turn anything that is not the expected answer into a report.
/// Say that a failure answered a mutation, so the reader can match it to the
/// request it left waiting instead of clearing everything in flight.
fn mutation_failed(told: Told) -> Told {
    match told {
        Told::Failed { message, fatal } => Told::EditFailed { message, fatal },
        other => other,
    }
}

fn unexpected(response: Result<DocumentResponse, SessionError>, wanted: &str) -> Told {
    match response {
        Err(error) => Told::Failed {
            message: error.to_string(),
            fatal: error.is_worker_loss(),
        },
        Ok(other) => Told::Failed {
            // A worker answering the wrong question is a protocol bug, not a
            // document problem, and is not something to retry.
            message: format!("the document worker answered {other:?} when asked for {wanted}"),
            fatal: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_to_nothing_reports_rather_than_waiting() {
        // There is no document at this path, so the worker exits before its
        // handshake and `open` fails immediately. A reader learns now instead
        // of watching an empty page.
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("absent.pdf");
        match ReaderLink::open(&missing) {
            Err(error) => assert!(
                error.is_worker_loss() || matches!(error, SessionError::Spawn(_)),
                "{error}"
            ),
            // A test binary is not the pulpit executable, so `current_exe`
            // spawns something that is not a document worker. Either outcome
            // is a failure to open, which is what this asserts.
            Ok(mut link) => {
                assert!(!link.ask(Ask::Describe { pages: 0 }) || link.collect().is_empty());
            }
        }
    }

    /// A link with nobody on the other end, so the counting can be exercised
    /// without a worker: the channels are the whole mechanism.
    fn detached_link() -> (ReaderLink, Sender<Told>, Receiver<Ask>) {
        let (ask_sender, ask_receiver) = std::sync::mpsc::channel::<Ask>();
        let (told_sender, told_receiver) = std::sync::mpsc::channel::<Told>();
        (
            ReaderLink {
                asks: ask_sender,
                told: told_receiver,
                source: PathBuf::from("nowhere.pdf"),
                lost: false,
                outstanding: 0,
                wakeup_inbox: None,
            },
            told_sender,
            ask_receiver,
        )
    }

    fn find(generation: u64, from_page: usize) -> Ask {
        Ask::FindText {
            generation: pulpit_core::search::SearchGeneration(generation),
            query: pulpit_core::search::Query::new("pdf", false, false),
            from_page,
            to_page: from_page + 4,
        }
    }

    #[test]
    fn a_search_the_reader_has_typed_past_is_answered_without_being_run() {
        let (sender, receiver) = std::sync::mpsc::channel::<Ask>();
        let mut queued = std::collections::VecDeque::new();
        // A newer query is already posted behind this one.
        sender.send(find(8, 0)).unwrap();
        sender.send(Ask::ListFields).unwrap();

        let answer = superseded(find(7, 0), &receiver, &mut queued)
            .expect("a search behind a newer one is not worth running");
        match answer {
            Told::Found { generation, chunk } => {
                assert_eq!(generation, pulpit_core::search::SearchGeneration(7));
                assert!(chunk.hits.is_empty(), "a dropped scan found nothing");
            }
            other => panic!("expected an empty answer, got {other:?}"),
        }
        // Everything read ahead to decide that is still there, in order.
        assert!(matches!(queued.pop_front(), Some(Ask::FindText { .. })));
        assert!(matches!(queued.pop_front(), Some(Ask::ListFields)));
        assert!(queued.is_empty());
    }

    #[test]
    fn the_other_chunks_of_the_same_scan_are_not_dropped_as_stale() {
        let (sender, receiver) = std::sync::mpsc::channel::<Ask>();
        let mut queued = std::collections::VecDeque::new();
        // Three chunks of one query are in flight together by design.
        sender.send(find(7, 4)).unwrap();
        sender.send(find(7, 8)).unwrap();

        assert!(
            superseded(find(7, 0), &receiver, &mut queued).is_err(),
            "a chunk of the current scan is work, not a stale answer"
        );
        assert_eq!(queued.len(), 2, "and the ones behind it are still queued");
    }

    #[test]
    fn the_link_is_busy_until_every_ask_is_answered() {
        let (mut link, told, _asks) = detached_link();
        assert!(!link.is_busy());
        assert!(link.ask(Ask::ListFields));
        assert!(link.ask(Ask::ListFields));
        assert!(link.is_busy());

        told.send(Told::Fields(Vec::new())).unwrap();
        assert_eq!(link.collect().len(), 1);
        // One answered, one still owed.
        assert!(link.is_busy());

        told.send(Told::Fields(Vec::new())).unwrap();
        assert_eq!(link.collect().len(), 1);
        assert!(!link.is_busy());
    }

    #[test]
    fn a_bounded_drain_yields_and_preserves_the_remaining_answers() {
        let (mut link, told, _asks) = detached_link();
        for _ in 0..3 {
            link.outstanding += 1;
            told.send(Told::Failed {
                message: "test".into(),
                fatal: false,
            })
            .unwrap();
        }

        let (first, more) = link.collect_bounded(2);
        assert_eq!(first.len(), 2);
        assert!(more);
        assert_eq!(link.outstanding, 1);

        let (last, more) = link.collect_bounded(2);
        assert_eq!(last.len(), 1);
        assert!(!more);
        assert_eq!(link.outstanding, 0);
    }

    #[test]
    fn a_lost_worker_owes_nothing() {
        let (mut link, told, asks) = detached_link();
        assert!(link.ask(Ask::ListFields));
        told.send(Told::Failed {
            message: "gone".into(),
            fatal: true,
        })
        .unwrap();
        assert_eq!(link.collect().len(), 1);
        assert!(link.is_lost());
        // The count died with the link: nothing is owed by a worker that is
        // not there, and a count left standing would hold the fast tick for
        // the rest of the session.
        assert!(!link.is_busy());

        // …and a send with nobody listening is not a request in flight.
        drop(asks);
        drop(told);
        let (mut link, told, asks) = detached_link();
        drop(asks);
        drop(told);
        assert!(!link.ask(Ask::ListFields));
        assert!(!link.is_busy());
    }

    #[test]
    fn an_unexpected_answer_is_fatal_and_a_refusal_is_not() {
        // The distinction §11.5 turns on, at the point it is decided.
        let refusal = unexpected(
            Err(SessionError::Refused(
                pulpit_render::document::protocol::DocumentFailure::Refused("off the page".into()),
            )),
            "a frame",
        );
        assert!(matches!(refusal, Told::Failed { fatal: false, .. }));

        let loss = unexpected(Err(SessionError::WorkerGone), "a frame");
        assert!(matches!(loss, Told::Failed { fatal: true, .. }));

        let confused = unexpected(Ok(DocumentResponse::Closed), "a frame");
        assert!(matches!(confused, Told::Failed { fatal: true, .. }));
    }
}
