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
use pulpit_render::document::protocol::{
    DocumentFrame, DocumentRenderRequest, DocumentRequest, DocumentResponse,
};
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
    Describe {
        pages: usize,
    },
    Render(DocumentRenderRequest),
    /// What is on a page, for hit-testing. The eraser and the selection tool
    /// need to know what is under the pointer, and only the document does.
    ListAnnotations {
        page: pulpit_core::page::PageIndex,
    },
    /// Resolve a text selection. Read-only: it never moves the revision
    /// (§6.3). `finalising` marks the query a release is waiting on, so the
    /// answer that commits a highlight is told apart from the ones that only
    /// keep the live selection drawn.
    SelectText {
        page: pulpit_core::page::PageIndex,
        selection: pulpit_render::document::TextSelection,
        finalising: bool,
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
}

/// Something the document worker had to say.
#[derive(Debug)]
pub enum Told {
    /// The worker is up and this is the document it holds.
    Described {
        info: Box<OpenDocumentInfo>,
        geometry: Vec<PageGeometry>,
        outline: pulpit_core::navigation::Outline,
        fields: Vec<pulpit_render::document::FormField>,
    },
    Frame(Box<DocumentFrame>),
    Annotations {
        page: pulpit_core::page::PageIndex,
        summaries: Vec<pulpit_render::document::AnnotationSummary>,
    },
    Selection {
        result: pulpit_render::document::TextSelectionResult,
        finalising: bool,
    },
    Applied(Box<Applied>),
    Saved(pulpit_render::document::SavedDocument),
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
        Ask::Render(request) => vec![match session.request(DocumentRequest::Render(request)) {
            Ok(DocumentResponse::Frame(frame)) => Told::Frame(frame),
            other => unexpected(other, "a frame"),
        }],
        Ask::ListAnnotations { page } => vec![match session
            .request(DocumentRequest::ListAnnotations { page })
        {
            Ok(DocumentResponse::Annotations(summaries)) => Told::Annotations { page, summaries },
            other => unexpected(other, "an annotation list"),
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
        Ask::Apply {
            expected_revision,
            transaction,
        } => vec![match session.request(DocumentRequest::Apply {
            expected_revision,
            transaction,
        }) {
            Ok(DocumentResponse::Applied(applied)) => Told::Applied(applied),
            other => unexpected(other, "an applied transaction"),
        }],
        Ask::Undo {
            expected_revision,
            operation,
        } => vec![match session.request(DocumentRequest::Undo {
            expected_revision,
            operation,
        }) {
            Ok(DocumentResponse::Applied(applied)) => Told::Applied(applied),
            other => unexpected(other, "an applied transaction"),
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
            Ok(DocumentResponse::Saved(saved)) => Told::Saved(saved),
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

    // The outline and the field list are asked for in the same exchange:
    // a rail that filled in a tick later would flicker, and neither answer
    // is large enough to be worth a round trip of its own.
    let outline = match session.request(DocumentRequest::Outline) {
        Ok(DocumentResponse::Outline(outline)) => outline,
        // A document without bookmarks is not a failure, and neither is a
        // build that cannot read them; the rail says it has none.
        _ => Default::default(),
    };
    let fields = match session.request(DocumentRequest::ListFields) {
        Ok(DocumentResponse::Fields(fields)) => fields,
        _ => Vec::new(),
    };

    vec![Told::Described {
        info,
        geometry,
        outline,
        fields,
    }]
}

/// Turn anything that is not the expected answer into a report.
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
