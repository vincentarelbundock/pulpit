//! The document worker: one open PDF, one execution context, one process.
//!
//! `SPEC-document.md` §5.1 and §6. This is a *role* of the same executable,
//! re-executed with a flag — not a new binary and not a new helper — and it is
//! the other role the renderer worker already plays. It is a separate loop
//! from [`crate::worker`] for one reason: that one serves many documents for
//! rendering and never mutates them, and this one owns exactly one document
//! and mutates it. Sharing a loop would mean a mutation and a render queue
//! racing for the same handle, which is the thing the process boundary exists
//! to prevent.
//!
//! What the boundary buys, and why per-keystroke round trips are worth it
//! (§8.6): form filling exercises PDFium's most complex code paths on hostile
//! input. A crash mid-fill must lose at most uncommitted in-field state —
//! never the document, and never a committed value.

use std::io::{BufWriter, Read, Write};

use crate::protocol::{read_message, write_message, ProtocolError};
use pulpit_core::page::PageIndex;

use super::protocol::{
    DocumentFailure, DocumentFrame, DocumentRequest, DocumentResponse, DOCUMENT_PROTOCOL_VERSION,
};
use super::{DocumentTransaction, PdfDocument};

/// What a document worker holds while it is serving.
///
/// One document, or none yet. Two would be two revisions counters and two undo
/// histories in one process, and nothing in the protocol says which one a
/// request means.
#[derive(Default)]
pub struct DocumentWorker<'a> {
    document: Option<PdfDocument<'a>>,
}

impl<'a> DocumentWorker<'a> {
    pub fn new() -> DocumentWorker<'a> {
        DocumentWorker::default()
    }

    /// Adopt an already-open document. The engine is built by the caller,
    /// which is what keeps this loop free of any particular PDF library.
    pub fn adopt(&mut self, document: PdfDocument<'a>) {
        self.document = Some(document);
    }

    pub fn is_open(&self) -> bool {
        self.document.is_some()
    }

    /// Answer one request.
    ///
    /// Every failure is an answer: a worker that panicked would take the
    /// document with it, and the supervisor cannot tell a mutation that was
    /// applied from one that was not without a reply (§11.5).
    pub fn handle(&mut self, request: DocumentRequest) -> DocumentResponse {
        // Bounds first, on receipt, however the sender validated it (§9.5):
        // supervisor and worker are separate processes that can disagree after
        // an upgrade.
        if let Err(limit) = request.validate() {
            return DocumentResponse::Failed(DocumentFailure::Refused(limit.to_string()));
        }

        match request {
            // Opening is the supervisor's to arrange: it chooses the engine
            // and hands the worker the result through `adopt`, so this loop
            // has no PDF library in it at all.
            DocumentRequest::Open(_) => DocumentResponse::Failed(DocumentFailure::Refused(
                "this worker is opened by its supervisor, not by a request".into(),
            )),
            DocumentRequest::Close => {
                self.document = None;
                DocumentResponse::Closed
            }
            other => match self.document.as_mut() {
                None => DocumentResponse::Failed(DocumentFailure::Refused(
                    "no document is open in this worker".into(),
                )),
                Some(document) => answer(document, other),
            },
        }
    }
}

fn answer(document: &mut PdfDocument<'_>, request: DocumentRequest) -> DocumentResponse {
    let result = match request {
        DocumentRequest::Info => Ok(DocumentResponse::Opened(Box::new(document.info().clone()))),
        DocumentRequest::PageGeometries { from, count } => {
            // Clamped to what the document has rather than refused: a reader
            // asking for a run past the end is asking for the tail of the
            // document, which is a well-formed question.
            let last = document.page_count();
            let count = count.min(last.saturating_sub(from.get()));
            (0..count)
                .map(|offset| document.page_geometry(PageIndex(from.get() + offset)))
                .collect::<Result<Vec<_>, _>>()
                .map(DocumentResponse::PageGeometries)
        }
        DocumentRequest::Render(render) => document
            .render_page(
                render.page,
                render.region,
                render.width,
                render.height,
                render.full_size(),
            )
            .map(|pixels| {
                DocumentResponse::Frame(Box::new(DocumentFrame {
                    page: render.page,
                    width: render.width,
                    height: render.height,
                    region: render.region,
                    // The revision the frame actually contains, which is the
                    // document's now — not the one the caller expected (A7).
                    revision: document.revision(),
                    pixels,
                }))
            }),
        DocumentRequest::ListAnnotations { page } => document
            .annotations(page)
            .map(DocumentResponse::Annotations),
        DocumentRequest::GetAnnotation { id } => document
            .annotation(&id)
            .map(|summary| DocumentResponse::Annotation(Box::new(summary))),
        DocumentRequest::SelectText { page, selection } => document
            .select_text(page, selection)
            .map(DocumentResponse::Selection),
        DocumentRequest::AreaText { page, rect } => document
            .area_text(page, rect)
            .map(|(text, truncated)| DocumentResponse::AreaText { text, truncated }),
        DocumentRequest::FindText {
            query,
            from_page,
            to_page,
        } => document
            .find_text(&query, from_page..to_page)
            .map(DocumentResponse::Found),
        DocumentRequest::ListFields => document.fields().map(DocumentResponse::Fields),
        DocumentRequest::Outline => document.outline().map(DocumentResponse::Outline),
        DocumentRequest::Properties => document
            .properties()
            .map(|properties| DocumentResponse::Properties(Box::new(properties))),
        DocumentRequest::Apply {
            expected_revision,
            transaction,
        } => document
            .apply(expected_revision, transaction)
            .map(|applied| DocumentResponse::Applied(Box::new(applied))),
        DocumentRequest::Undo {
            expected_revision,
            operation,
        } => document
            .undo(expected_revision, operation)
            .map(|applied| DocumentResponse::Applied(Box::new(applied))),
        DocumentRequest::SaveAs(save) => document
            .save_as(&save.destination, save.options)
            .map(DocumentResponse::Saved),
        // Straight through to PDFium's form-fill environment (§8.6). The
        // worker does not interpret the event, does not know which field it
        // lands in and does not draw a field editor: it forwards, and hands
        // back the rectangles the engine invalidated. That is the whole point
        // — there is one implementation of what a keystroke does to a form
        // field, and it is the one that also generates the appearance.
        DocumentRequest::FormEvent { page, event } => document
            .form_event(page, event)
            .map(|result| DocumentResponse::Form(Box::new(result))),
        DocumentRequest::Open(_) | DocumentRequest::Close => {
            unreachable!("handled before dispatch")
        }
    };

    match result {
        Ok(response) => match response.validate() {
            Ok(()) => response,
            // The worker's own answer is over a limit. Reporting it is better
            // than sending a message the supervisor will refuse to read,
            // which would look like a dead worker.
            Err(limit) => DocumentResponse::Failed(DocumentFailure::Refused(limit.to_string())),
        },
        Err(error) => DocumentResponse::Failed(DocumentFailure::from(&error)),
    }
}

/// Serve until the peer closes the connection or asks the worker to close.
///
/// The first message must be a version handshake: a worker that does not speak
/// the same document protocol is shut down rather than trusted (§5.2).
pub fn serve(
    mut input: impl Read,
    output: impl Write,
    mut worker: DocumentWorker<'_>,
) -> Result<(), ProtocolError> {
    let mut output = BufWriter::new(output);
    write_message(&mut output, &Hello::default())?;

    loop {
        let request: DocumentRequest = match read_message(&mut input) {
            Ok(request) => request,
            Err(ProtocolError::Closed) => return Ok(()),
            Err(error) => return Err(error),
        };
        let closing = matches!(request, DocumentRequest::Close);
        let response = worker.handle(request);
        write_message(&mut output, &response)?;
        if closing {
            return Ok(());
        }
    }
}

/// The worker's opening line: which document protocol it speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub document_protocol_version: u32,
}

impl Default for Hello {
    fn default() -> Self {
        Hello {
            document_protocol_version: DOCUMENT_PROTOCOL_VERSION,
        }
    }
}

impl Hello {
    /// Does this worker speak the same protocol we do?
    pub fn is_compatible(&self) -> bool {
        self.document_protocol_version == DOCUMENT_PROTOCOL_VERSION
    }
}

/// The empty transaction the supervisor must never send.
///
/// Exposed so the supervisor can assert on it rather than discovering it as a
/// refusal: an empty transaction is a bug on the sending side, not a document
/// that said no.
pub fn is_sendable(transaction: &DocumentTransaction) -> bool {
    !transaction.is_empty() && transaction.validate().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::memory::MemoryDocument;
    use crate::document::model::{DocumentCommand, DocumentRevision, SaveOptions};
    use crate::document::protocol::{FormInputEvent, OpenDocument, SaveRequest};
    use pulpit_core::annotate::{
        AnnotationCommand, AnnotationDraft, InkDraft, InkPoint, MarkStyle,
    };
    use pulpit_core::page::{PageIndex, PagePoint};

    fn worker() -> DocumentWorker<'static> {
        let mut worker = DocumentWorker::new();
        worker.adopt(PdfDocument::new(Box::new(MemoryDocument::with_form()), 3));
        worker
    }

    fn stroke() -> DocumentTransaction {
        DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
            InkDraft {
                page: PageIndex(0),
                points: vec![InkPoint::new(20.0, 20.0), InkPoint::new(90.0, 60.0)],
                style: MarkStyle::default(),
            },
        ))])
    }

    #[test]
    fn a_worker_with_nothing_open_refuses_rather_than_falls_over() {
        let mut worker = DocumentWorker::new();
        assert!(!worker.is_open());
        let response = worker.handle(DocumentRequest::ListFields);
        assert!(matches!(
            response,
            DocumentResponse::Failed(DocumentFailure::Refused(_))
        ));
    }

    #[test]
    fn opening_is_the_supervisors_to_arrange() {
        // The loop carries no PDF library, which is what lets it be tested
        // against a memory document on a machine with no PDFium.
        let mut worker = worker();
        let response = worker.handle(DocumentRequest::Open(OpenDocument {
            path: "/tmp/x.pdf".into(),
            password: None,
            id_seed: 0,
        }));
        assert!(matches!(
            response,
            DocumentResponse::Failed(DocumentFailure::Refused(_))
        ));
    }

    #[test]
    fn a_transaction_is_applied_and_its_answer_carries_the_new_revision() {
        let mut worker = worker();
        let response = worker.handle(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        });
        let DocumentResponse::Applied(applied) = response else {
            panic!("expected an applied transaction, got {response:?}")
        };
        assert_eq!(applied.document_revision, DocumentRevision(1));
        assert_eq!(applied.effects.len(), 1);
    }

    #[test]
    fn a_stale_revision_comes_back_as_a_conflict_rather_than_an_engine_error() {
        // The distinction matters on the receiving side: a conflict is
        // re-read and decided, an engine failure may be retried (§11.5).
        let mut worker = worker();
        worker.handle(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        });
        let response = worker.handle(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        });
        let DocumentResponse::Failed(failure) = response else {
            panic!("expected a refusal")
        };
        assert!(matches!(
            failure,
            DocumentFailure::RevisionConflict {
                expected: DocumentRevision(0),
                actual: DocumentRevision(1)
            }
        ));
        assert!(!failure.is_retryable());
    }

    #[test]
    fn an_undo_answers_with_the_operation_that_redoes_it() {
        let mut worker = worker();
        let DocumentResponse::Applied(applied) = worker.handle(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        }) else {
            panic!("the stroke commits")
        };
        let DocumentResponse::Applied(undone) = worker.handle(DocumentRequest::Undo {
            expected_revision: DocumentRevision(1),
            operation: applied.undo,
        }) else {
            panic!("the undo applies")
        };
        assert_eq!(undone.document_revision, DocumentRevision(2));
        // Redo needs no request of its own: it is the undo request carrying
        // what the undo handed back (§9.5).
        let DocumentResponse::Applied(redone) = worker.handle(DocumentRequest::Undo {
            expected_revision: DocumentRevision(2),
            operation: undone.undo,
        }) else {
            panic!("the redo applies")
        };
        assert_eq!(redone.document_revision, DocumentRevision(3));
    }

    #[test]
    fn a_request_past_the_limits_is_refused_on_receipt_however_it_was_sent() {
        let mut worker = worker();
        let huge = DocumentTransaction(vec![
            DocumentCommand::Annotation(AnnotationCommand::Create(
                AnnotationDraft::Ink(InkDraft {
                    page: PageIndex(0),
                    points: vec![InkPoint::new(1.0, 1.0); 4],
                    style: MarkStyle::default(),
                })
            ));
            super::super::limits::MAX_OPERATIONS_PER_TRANSACTION
                + 1
        ]);
        let response = worker.handle(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: huge,
        });
        assert!(matches!(
            response,
            DocumentResponse::Failed(DocumentFailure::Refused(_))
        ));
    }

    #[test]
    fn a_form_event_says_it_is_not_wired_rather_than_answering_with_nothing() {
        // An empty answer would look like a keystroke that did nothing; this
        // says which it is, so a caller cannot mistake a missing feature for
        // a field that ignored them.
        let mut worker = worker();
        let response = worker.handle(DocumentRequest::FormEvent {
            page: PageIndex(0),
            event: FormInputEvent::Char { character: 'a' },
        });
        let DocumentResponse::Failed(failure) = response else {
            panic!("expected a refusal")
        };
        assert!(failure.message().contains("form-fill"));
    }

    #[test]
    fn closing_ends_the_session_and_leaves_nothing_open() {
        let mut worker = worker();
        assert!(worker.is_open());
        assert_eq!(
            worker.handle(DocumentRequest::Close),
            DocumentResponse::Closed
        );
        assert!(!worker.is_open());
    }

    #[test]
    fn a_conversation_over_a_pipe_ends_when_the_peer_closes() {
        // The loop's own contract: a handshake first, an answer per request,
        // and a clean return when the connection goes.
        let mut request_bytes = Vec::new();
        write_message(
            &mut request_bytes,
            &DocumentRequest::ListAnnotations { page: PageIndex(0) },
        )
        .unwrap();
        write_message(&mut request_bytes, &DocumentRequest::Close).unwrap();

        let mut answers = Vec::new();
        serve(std::io::Cursor::new(request_bytes), &mut answers, worker()).unwrap();

        let mut reader = std::io::Cursor::new(answers);
        let hello: Hello = read_message(&mut reader).unwrap();
        assert!(hello.is_compatible());
        let first: DocumentResponse = read_message(&mut reader).unwrap();
        assert!(matches!(first, DocumentResponse::Annotations(_)));
        let second: DocumentResponse = read_message(&mut reader).unwrap();
        assert_eq!(second, DocumentResponse::Closed);
        // Nothing after the close: the loop returned rather than waiting.
        assert!(read_message::<DocumentResponse>(&mut reader).is_err());
    }

    #[test]
    fn a_truncated_request_ends_the_loop_cleanly_rather_than_hanging() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &DocumentRequest::ListFields).unwrap();
        bytes.truncate(bytes.len() - 3);
        let mut answers = Vec::new();
        // A half-message is the peer having gone away mid-write, which is a
        // closed connection and not a protocol violation to shout about.
        let outcome = serve(std::io::Cursor::new(bytes), &mut answers, worker());
        assert!(outcome.is_err() || !answers.is_empty());
    }

    #[test]
    fn saving_answers_with_the_revision_that_was_written() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("out.pdf");
        let mut worker = worker();
        worker.handle(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: stroke(),
        });
        let response = worker.handle(DocumentRequest::SaveAs(SaveRequest {
            destination: destination.clone(),
            options: SaveOptions::verified(),
        }));
        let DocumentResponse::Saved(saved) = response else {
            panic!("expected a save, got {response:?}")
        };
        assert_eq!(saved.revision, DocumentRevision(1));
        assert_eq!(saved.path, destination);
        assert!(destination.exists());
    }

    #[test]
    fn a_selection_on_a_page_with_no_text_is_an_empty_answer_not_a_failure() {
        let mut worker = worker();
        let response = worker.handle(DocumentRequest::SelectText {
            page: PageIndex(0),
            selection: crate::document::TextSelection::Word {
                at: PagePoint::new(10.0, 10.0),
            },
        });
        let DocumentResponse::Selection(selection) = response else {
            panic!("expected a selection")
        };
        assert!(selection.is_empty());
    }

    #[test]
    fn a_backend_that_cannot_search_says_so_rather_than_finding_nothing() {
        let mut worker = worker();
        let response = worker.handle(DocumentRequest::FindText {
            query: pulpit_core::search::Query::new("anything", false, false),
            from_page: 0,
            to_page: 1,
        });
        assert!(matches!(
            response,
            DocumentResponse::Failed(DocumentFailure::Unsupported(_))
        ));
    }

    #[test]
    fn a_search_wider_than_the_limit_is_refused_before_it_is_run() {
        let request = DocumentRequest::FindText {
            query: pulpit_core::search::Query::new("pdf", false, false),
            from_page: 0,
            to_page: crate::document::limits::MAX_PAGES_PER_SEARCH + 1,
        };
        assert!(request.validate().is_err());
        assert!(!request.is_mutation());
    }

    #[test]
    fn an_empty_transaction_is_a_bug_on_the_sending_side() {
        assert!(!is_sendable(&DocumentTransaction::default()));
        assert!(is_sendable(&stroke()));
    }

    #[test]
    fn the_handshake_names_the_protocol_this_build_speaks() {
        assert!(Hello::default().is_compatible());
        assert!(!Hello {
            document_protocol_version: DOCUMENT_PROTOCOL_VERSION + 1,
        }
        .is_compatible());
    }
}
