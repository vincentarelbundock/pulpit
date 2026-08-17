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
    /// Resolve a text selection. Read-only: it never moves the revision
    /// (§6.3). `finalising` marks the query a release is waiting on, so the
    /// answer that commits a highlight is told apart from the ones that only
    /// keep the live selection drawn.
    SelectText {
        page: pulpit_core::page::PageIndex,
        selection: pulpit_render::document::TextSelection,
        finalising: bool,
    },
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
        /// The full-page frame the crop is going to be composited into. Sent
        /// rather than left to the worker to reconstruct from the region: the
        /// two roundings disagree by up to a pixel, and a patch drawn at a
        /// scale the frame beneath it was not drawn at shimmers as the
        /// rectangle grows with each keystroke (§9.4).
        frame_width: u32,
        frame_height: u32,
        expected_revision: DocumentRevision,
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
    Selection {
        result: pulpit_render::document::TextSelectionResult,
        finalising: bool,
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
    /// form, or a page that would not load. Nothing changed, and saying so is
    /// what keeps the caller's one-in-flight guard from latching shut.
    FormRefused {
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

/// The application's end of the conversation.
pub struct ReaderLink {
    asks: Sender<Ask>,
    told: Receiver<Told>,
    source: PathBuf,
    /// Set when the worker is gone. The link is not restarted here: reopening
    /// is a decision about the journal, not about a channel (§11.5).
    lost: bool,
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
        let session = DocumentSession::start(&DocumentWorkerCommand::default(), source)?;
        let (ask_sender, ask_receiver) = std::sync::mpsc::channel::<Ask>();
        let (told_sender, told_receiver) = std::sync::mpsc::channel::<Told>();

        let source_for_thread = source.to_path_buf();
        std::thread::Builder::new()
            .name("document-session".into())
            .spawn(move || {
                serve(session, ask_receiver, told_sender);
                tracing::debug!(source = %source_for_thread.display(), "document session ended");
            })
            .map_err(SessionError::Spawn)?;

        Ok(ReaderLink {
            asks: ask_sender,
            told: told_receiver,
            source: source.to_path_buf(),
            lost: false,
        })
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
            return false;
        }
        true
    }

    /// Everything the worker has said since the last time it was asked.
    ///
    /// Drains rather than taking one, because the application collects on a
    /// tick and a queue that grows by more than one per tick would never empty.
    pub fn collect(&mut self) -> Vec<Told> {
        let mut answers = Vec::new();
        loop {
            match self.told.try_recv() {
                Ok(told) => {
                    if matches!(&told, Told::Failed { fatal: true, .. }) {
                        self.lost = true;
                    }
                    answers.push(told);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.lost = true;
                    break;
                }
            }
        }
        answers
    }
}

/// The thread body: one request at a time, in the order they were posted.
///
/// Deliberately serial. The worker holds one document and answers one thing at
/// a time; a queue in front of it would only reorder the wait, and ordering is
/// what makes the optimistic revision check mean anything (§9.5).
fn serve(mut session: DocumentSession, asks: Receiver<Ask>, told: Sender<Told>) {
    while let Ok(ask) = asks.recv() {
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
        }
        if fatal {
            return;
        }
    }
    session.close();
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
            expected_revision,
        } => vec![match session.request(DocumentRequest::Render(
            pulpit_render::document::protocol::DocumentRenderRequest {
                page,
                width,
                height,
                expected_revision,
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
                return Vec::new();
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
                    vec![Told::FormRefused { moved }]
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
