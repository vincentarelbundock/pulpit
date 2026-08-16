//! The document half of the worker protocol (§9.5).
//!
//! Versioned and length-bounded, because supervisor and worker are separate
//! processes that can disagree after an upgrade (§5.2) — not because two
//! projects consume it. Every field that will later size an allocation is
//! validated *before* anything is allocated for it, on both sides, against the
//! one set of constants in [`super::limits`].

use pulpit_core::annotate::AnnotationId;
use pulpit_core::page::{PageIndex, PagePoint, PageRect};
use pulpit_core::search::{HitChunk, Query};
use serde::{Deserialize, Serialize};

use super::limits::{self, LimitExceeded};
use super::model::{
    AnnotationSummary, Applied, DocumentRevision, DocumentTransaction, DocumentUndo, FormField,
    OpenDocumentInfo, SaveOptions, SavedDocument, TextSelection, TextSelectionResult,
};

/// Bumped whenever the document wire format changes. Carried alongside the
/// renderer's own [`crate::protocol::PROTOCOL_VERSION`]: a worker that does not
/// answer with the same version is shut down rather than trusted.
pub const DOCUMENT_PROTOCOL_VERSION: u32 = 1;

/// Open a document for reading and annotating.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenDocument {
    pub path: std::path::PathBuf,
    /// A password, when the document needs one.
    ///
    /// Carried in the request and nowhere else: it is never written to the
    /// recovery journal (§11.4) and never appears in a diagnostic.
    pub password: Option<String>,
    /// What the worker mixes into the `/NM` names it writes this session (A3).
    pub id_seed: u64,
}

impl std::fmt::Debug for OpenDocumentRedacted<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenDocument")
            .field("path", &self.0.path)
            .field("password", &self.0.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// A view of an [`OpenDocument`] safe to print. A password in a log is a
/// password on disk.
pub struct OpenDocumentRedacted<'a>(pub &'a OpenDocument);

impl OpenDocument {
    pub fn redacted(&self) -> OpenDocumentRedacted<'_> {
        OpenDocumentRedacted(self)
    }
}

/// Raw input forwarded to PDFium's form-fill environment (§8.6).
///
/// The application does not interpret these: it does not hit-test the field,
/// place the caret or decide what a keystroke means in a comb field. It
/// forwards what the pointer and the keyboard did, in page space, and the
/// engine draws the result. That is what makes one editing surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormInputEvent {
    PointerDown {
        at: PagePoint,
    },
    PointerUp {
        at: PagePoint,
    },
    PointerMove {
        at: PagePoint,
    },
    /// A *committed* character. In-progress IME composition is not forwarded
    /// and is not displayed in the field — a documented limitation (§3.4),
    /// stated rather than silently degraded.
    Char {
        character: char,
    },
    KeyDown {
        key: FormKey,
    },
    KeyUp {
        key: FormKey,
    },
    /// The page surface gained or lost keyboard focus. Losing it is what
    /// commits an in-progress field edit, so it is an event and not a hint.
    Focus {
        gained: bool,
    },
    /// Choose, or unchoose, one option of the focused choice field.
    ///
    /// The one event here that is not a raw input event, and it is worth
    /// saying why it does not break §8.6's rule. A combo box's list and a list
    /// box's rows are drawn by PDFium inside its own popup, and reaching an
    /// option by synthesising the clicks that would open that popup and land
    /// on the right row means knowing where PDFium decided to draw it — which
    /// is precisely the kind of imitation that rule exists to prevent.
    /// `FORM_SetIndexSelected` is PDFium's own answer: the engine performs the
    /// selection, generates the appearance and reports the change, exactly as
    /// it does for a keystroke. There is still one implementation, and it is
    /// still PDFium's.
    SelectOption {
        index: u32,
        selected: bool,
    },
    /// Put the caret in a named field, by name rather than by position.
    ///
    /// Not reachable by synthesising a click, and that is the reason it exists.
    /// A form can put two widgets on top of each other — an overlapping stack
    /// is one of the corpus's hazards, and a legitimate layout for a field that
    /// spans a printed line — and a click can only ever reach whichever one
    /// PDFium decides is on top. Naming the field is how a person reaches the
    /// other one. PDFium still does the focusing, the caret and the editing.
    ///
    /// No caller in the application sends this today: the field panel that
    /// would have was removed. It stays because reaching an occluded widget
    /// by name is an engine capability nothing else provides.
    FocusField {
        name: String,
    },
}

/// The keys a form field responds to that are not characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormKey {
    Backspace,
    Delete,
    Enter,
    Escape,
    Tab,
    ShiftTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// What the worker is asked to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentRequest {
    Open(OpenDocument),
    /// What the worker knows about the document it holds: page count,
    /// compatibility level and the warnings a user is told before they start
    /// editing (§3.4, A9).
    Info,
    /// Canonical geometry for a run of pages.
    ///
    /// A run rather than one page, because a reader laying out a scrolled
    /// column needs every page's size to place any of them (§8.7), and a
    /// round trip per page would be one per page at open. A run rather than
    /// the whole document, because the answer is bounded like everything else
    /// that crosses this wire.
    PageGeometries {
        from: PageIndex,
        count: usize,
    },
    /// Render one page at a size the caller chose, from the document the
    /// worker holds.
    ///
    /// The frame contains every committed annotation, because it is drawn
    /// from the mutated document itself. The application no longer draws
    /// reader pages this way — it renders them in the worker pool from a
    /// revision snapshot the `SaveAs` machinery writes, which gets it the
    /// pool's caching, cancellation and shared-memory transport — but the
    /// request stays: it is the ground truth a test can hold a snapshot
    /// against, and the natural carrier for a §9.4 partial repaint.
    Render(DocumentRenderRequest),
    ListAnnotations {
        page: PageIndex,
    },
    GetAnnotation {
        id: AnnotationId,
    },
    SelectText {
        page: PageIndex,
        selection: TextSelection,
    },
    /// Find a string in the text layer of a run of pages.
    ///
    /// A run rather than the whole document because a five-hundred-page deck
    /// must not be scanned inside one round trip: the caller walks the
    /// document a chunk at a time, sees hits as they are found, and stops
    /// asking when the query changes (§9.5).
    FindText {
        query: Query,
        /// Half-open range of physical pages, bounded by
        /// [`limits::MAX_PAGES_PER_SEARCH`].
        from_page: usize,
        to_page: usize,
    },
    ListFields,
    /// The document's bookmark tree, for the outline rail.
    Outline,
    /// Raw input forwarded to the form-fill environment (§8.6).
    FormEvent {
        page: PageIndex,
        event: FormInputEvent,
    },
    Apply {
        expected_revision: DocumentRevision,
        transaction: DocumentTransaction,
    },
    Undo {
        expected_revision: DocumentRevision,
        operation: DocumentUndo,
    },
    SaveAs(SaveRequest),
    Close,
}

/// The most pages one [`DocumentRequest::PageGeometries`] answer may cover.
///
/// A page's geometry is six floats; a thousand of them is a message of a few
/// tens of kilobytes, which is nothing beside a frame and plenty for the
/// longest document a person scrolls by hand. A longer one asks again.
pub const MAX_PAGE_GEOMETRIES: usize = 1_024;

/// Render one page of the open document.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DocumentRenderRequest {
    pub page: PageIndex,
    /// Target size in physical pixels.
    pub width: u32,
    pub height: u32,
    /// The revision the caller believes the document is at.
    ///
    /// Not a precondition — a render is not a mutation and never fails over a
    /// revision — but the answer carries the revision it actually contains,
    /// which is how a preview knows when it may be dropped (A7, §9.2).
    pub expected_revision: DocumentRevision,
    /// Which part of the page to draw, as a fraction of it.
    ///
    /// [`Region::FULL`] is the whole page, and `width` × `height` is then the
    /// page's size in pixels. A smaller region is the §9.4 partial repaint:
    /// the caller has a frame of the page already and needs only the rectangle
    /// an edit changed, so `width` × `height` is that rectangle's size and the
    /// page is drawn at `width / region.width` across. Same renderer, same
    /// document, same appearance — which is what §9.4 requires of a partial
    /// composite, and why this is a crop rather than a second way to draw.
    #[serde(default = "full_region")]
    pub region: pulpit_core::notes::Region,
}

fn full_region() -> pulpit_core::notes::Region {
    pulpit_core::notes::Region::FULL
}

impl DocumentRenderRequest {
    /// The largest page pulpit will rasterise, in each direction.
    ///
    /// The same bound the render path uses: 16k × 16k RGBA is already a
    /// gigabyte, and anything beyond it is a bug or an attack.
    pub const MAX_DIMENSION: u32 = 16_384;

    pub fn rgba_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4
    }

    pub fn validate(&self) -> Result<(), LimitExceeded> {
        if self.width == 0 || self.height == 0 {
            return Err(LimitExceeded {
                what: "a zero-sized render",
                limit: 1,
            });
        }
        if self.width > Self::MAX_DIMENSION || self.height > Self::MAX_DIMENSION {
            return Err(LimitExceeded {
                what: "render dimensions",
                limit: Self::MAX_DIMENSION as usize,
            });
        }
        Ok(())
    }
}

/// One rendered page, and the revision it contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentFrame {
    pub page: PageIndex,
    pub width: u32,
    pub height: u32,
    /// The revision this frame was rendered from. A preview is removed only
    /// when a frame at or beyond the mutation's revision arrives (A7).
    pub revision: DocumentRevision,
    /// Tightly packed RGBA8.
    pub pixels: Vec<u8>,
    /// The part of the page this covers, so a partial repaint knows where on
    /// the page it belongs. [`Region::FULL`] for a whole page.
    #[serde(default = "full_region")]
    pub region: pulpit_core::notes::Region,
}

impl DocumentFrame {
    pub fn is_consistent(&self) -> bool {
        self.pixels.len() == self.width as usize * self.height as usize * 4
    }
}

/// Write the open document somewhere else. Never over its source (A6), which
/// the worker checks again on receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveRequest {
    pub destination: std::path::PathBuf,
    pub options: SaveOptions,
}

/// What a form event changed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FormEventResult {
    /// The page rectangles the engine invalidated, to be re-composited
    /// (§9.4). Empty when the event changed nothing on screen.
    pub invalidated: Vec<PageRect>,
    /// A field whose value was *committed* by this event — a toggle, a
    /// selection change, a focus loss. One committed change is one revision
    /// and one undo entry, in the same history as the annotations (§8.6).
    pub committed: Option<CommittedField>,
}

/// A field value the engine committed, and the revision that carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommittedField {
    pub name: String,
    pub value: String,
    pub revision: DocumentRevision,
}

/// What the worker answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentResponse {
    Opened(Box<OpenDocumentInfo>),
    /// Canonical geometry for the run that was asked for, in page order,
    /// starting at the requested page.
    PageGeometries(Vec<pulpit_core::page::PageGeometry>),
    Frame(Box<DocumentFrame>),
    Annotations(Vec<AnnotationSummary>),
    Annotation(Box<AnnotationSummary>),
    Selection(TextSelectionResult),
    /// The hits in one run of pages. A run with none answers with an empty
    /// chunk, which is how the caller knows to move its frontier along.
    Found(HitChunk),
    Fields(Vec<FormField>),
    Outline(pulpit_core::navigation::Outline),
    Form(FormEventResult),
    Applied(Box<Applied>),
    Saved(SavedDocument),
    Closed,
    Failed(DocumentFailure),
}

/// Why a request failed, in a form that survives the wire.
///
/// [`super::DocumentError`] carries an `io::Error` and a `DraftError` and is
/// not serialisable; this is what crosses the pipe. The distinction that
/// matters to a caller is whether the request can be retried, so it is a
/// variant rather than a string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentFailure {
    /// The document moved underneath the request. The caller re-reads the
    /// revision and decides; it MUST NOT simply resend (A7).
    RevisionConflict {
        expected: DocumentRevision,
        actual: DocumentRevision,
    },
    /// The request named something that is not there.
    NotFound(String),
    /// The request was refused before anything was touched.
    Refused(String),
    /// The backend cannot do this at all.
    ///
    /// Distinct from a failure and from an empty answer: a document that
    /// cannot be searched and a document with no matches must not look the
    /// same to the person who typed the query.
    Unsupported(String),
    /// The engine failed. Read-only requests may be retried; a mutation is
    /// not assumed committed without a response (§11.5).
    Engine(String),
}

impl std::fmt::Display for DocumentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl DocumentFailure {
    /// May a read-only request that failed this way simply be sent again?
    pub fn is_retryable(&self) -> bool {
        matches!(self, DocumentFailure::Engine(_))
    }

    pub fn message(&self) -> String {
        match self {
            DocumentFailure::RevisionConflict { expected, actual } => {
                format!("the document changed: expected {expected}, it is at {actual}")
            }
            DocumentFailure::NotFound(what) => format!("no {what} in this document"),
            DocumentFailure::Refused(why) => why.clone(),
            DocumentFailure::Engine(why) => why.clone(),
            DocumentFailure::Unsupported(what) => format!("this document cannot {what}"),
        }
    }
}

impl From<&super::DocumentError> for DocumentFailure {
    fn from(error: &super::DocumentError) -> DocumentFailure {
        use super::DocumentError as E;
        match error {
            E::RevisionConflict { expected, actual } => DocumentFailure::RevisionConflict {
                expected: *expected,
                actual: *actual,
            },
            E::NoSuchPage { page, count } => {
                DocumentFailure::NotFound(format!("page {page} ({count} pages)"))
            }
            E::NoSuchAnnotation(id) => DocumentFailure::NotFound(format!("annotation {id}")),
            E::NoSuchField(name) => DocumentFailure::NotFound(format!("field {name}")),
            E::NotEditable(_)
            | E::Rejected(_)
            | E::Limit(_)
            | E::MutationForbidden
            | E::SourceIsDestination => DocumentFailure::Refused(error.to_string()),
            E::Unsupported(what) => DocumentFailure::Unsupported(what.clone()),
            E::Backend(_) | E::Save(_) | E::Io(_) => DocumentFailure::Engine(error.to_string()),
        }
    }
}

impl DocumentRequest {
    /// Would this change the document?
    ///
    /// The supervisor needs to know: a read-only request may be retried after
    /// a worker crash, and a mutation may not be assumed committed without a
    /// response (§11.5).
    pub fn is_mutation(&self) -> bool {
        match self {
            DocumentRequest::Apply { .. } | DocumentRequest::Undo { .. } => true,
            // A form event may commit a field value, so it is treated as a
            // mutation even though most of them only move a caret. Guessing
            // the other way would replay a keystroke into a document that
            // already had it.
            DocumentRequest::FormEvent { .. } => true,
            DocumentRequest::Open(_)
            | DocumentRequest::Info
            | DocumentRequest::PageGeometries { .. }
            | DocumentRequest::Render(_)
            | DocumentRequest::ListAnnotations { .. }
            | DocumentRequest::GetAnnotation { .. }
            | DocumentRequest::SelectText { .. }
            | DocumentRequest::FindText { .. }
            | DocumentRequest::ListFields
            | DocumentRequest::Outline
            | DocumentRequest::SaveAs(_)
            | DocumentRequest::Close => false,
        }
    }

    /// Check the request against the declared limits, on the sending side and
    /// again on receipt (A8).
    pub fn validate(&self) -> Result<(), LimitExceeded> {
        match self {
            DocumentRequest::Apply { transaction, .. } => transaction.validate(),
            DocumentRequest::Undo { operation, .. } => limits::within(
                "undo operations",
                operation.operations.len(),
                limits::MAX_OPERATIONS_PER_TRANSACTION,
            ),
            DocumentRequest::Render(render) => render.validate(),
            DocumentRequest::FindText {
                query,
                from_page,
                to_page,
            } => {
                limits::within(
                    "query length",
                    query.text().chars().count(),
                    pulpit_core::search::MAX_QUERY_CHARS,
                )?;
                // A backwards range is not a small allocation, it is a caller
                // bug; refuse it here rather than let it become an empty scan
                // that silently reports "no matches".
                limits::within(
                    "pages in one search",
                    to_page.saturating_sub(*from_page),
                    limits::MAX_PAGES_PER_SEARCH,
                )?;
                if to_page < from_page {
                    return Err(LimitExceeded {
                        what: "a backwards page range",
                        limit: 0,
                    });
                }
                Ok(())
            }
            DocumentRequest::PageGeometries { count, .. } => {
                limits::within("page geometries", *count, MAX_PAGE_GEOMETRIES)
            }
            DocumentRequest::Open(open) => limits::within(
                "password length",
                open.password.as_ref().map(String::len).unwrap_or(0),
                limits::MAX_TEXT_BYTES,
            ),
            _ => Ok(()),
        }
    }
}

impl DocumentResponse {
    /// Check an answer before anything is built from it. The worker is
    /// supervised, not trusted: it has just parsed a hostile document.
    pub fn validate(&self) -> Result<(), LimitExceeded> {
        match self {
            DocumentResponse::Annotations(annotations) => limits::within(
                "annotations on a page",
                annotations.len(),
                limits::MAX_ANNOTATIONS_PER_PAGE,
            ),
            DocumentResponse::Fields(fields) => {
                limits::within("form fields", fields.len(), limits::MAX_FORM_FIELDS)
            }
            DocumentResponse::Selection(selection) => limits::within(
                "quadrilaterals in a selection",
                selection.quads.len(),
                limits::MAX_QUADS_PER_SELECTION,
            ),
            DocumentResponse::PageGeometries(pages) => {
                limits::within("page geometries", pages.len(), MAX_PAGE_GEOMETRIES)
            }
            DocumentResponse::Frame(frame) => {
                if frame.is_consistent() {
                    Ok(())
                } else {
                    Err(LimitExceeded {
                        what: "a frame whose pixels do not match its dimensions",
                        limit: 0,
                    })
                }
            }
            DocumentResponse::Form(result) => limits::within(
                "invalidated rectangles",
                result.invalidated.len(),
                limits::MAX_ANNOTATIONS_PER_PAGE,
            ),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotate::{
        AnnotationCommand, AnnotationDraft, IdGenerator, InkDraft, InkPoint, MarkStyle,
    };
    use pulpit_core::page::PageIndex;

    fn ink_transaction(points: usize) -> DocumentTransaction {
        DocumentTransaction::from_annotations([AnnotationCommand::Create(AnnotationDraft::Ink(
            InkDraft {
                page: PageIndex(0),
                points: vec![InkPoint::new(1.0, 1.0); points],
                style: MarkStyle::default(),
            },
        ))])
    }

    #[test]
    fn a_request_says_whether_it_changes_the_document() {
        assert!(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: ink_transaction(2),
        }
        .is_mutation());
        assert!(DocumentRequest::FormEvent {
            page: PageIndex(0),
            event: FormInputEvent::Char { character: 'a' },
        }
        .is_mutation());
        assert!(!DocumentRequest::ListFields.is_mutation());
        assert!(!DocumentRequest::ListAnnotations { page: PageIndex(0) }.is_mutation());
        assert!(!DocumentRequest::Close.is_mutation());
    }

    #[test]
    fn an_over_large_request_is_refused_before_it_is_sent() {
        let request = DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: ink_transaction(limits::MAX_POINTS_PER_INK + 1),
        };
        assert!(request.validate().is_err());
        assert!(DocumentRequest::Apply {
            expected_revision: DocumentRevision::INITIAL,
            transaction: ink_transaction(8),
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn an_over_long_password_is_refused_rather_than_sent() {
        let request = DocumentRequest::Open(OpenDocument {
            path: "/tmp/x.pdf".into(),
            password: Some("x".repeat(limits::MAX_TEXT_BYTES + 1)),
            id_seed: 1,
        });
        assert!(request.validate().is_err());
    }

    #[test]
    fn a_password_is_never_printed() {
        let open = OpenDocument {
            path: "/tmp/x.pdf".into(),
            password: Some("hunter2".into()),
            id_seed: 1,
        };
        let printed = format!("{:?}", open.redacted());
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("redacted"));
        assert!(printed.contains("x.pdf"), "the path is still useful");
    }

    #[test]
    fn an_answer_from_the_worker_is_checked_before_it_is_believed() {
        let too_many = DocumentResponse::Fields(vec![
            FormField {
                name: "f".into(),
                kind: super::super::model::FieldKind::Text,
                value: String::new(),
                read_only: false,
                options: Vec::new(),
                allows_custom_value: true,
                multiple_selection: false,
                selected: Vec::new(),
                widgets: Vec::new(),
            };
            limits::MAX_FORM_FIELDS + 1
        ]);
        assert!(too_many.validate().is_err());
        assert!(DocumentResponse::Closed.validate().is_ok());
    }

    #[test]
    fn a_failure_says_whether_it_can_be_retried() {
        // A read-only request that died with the worker can be sent again; a
        // refusal and a conflict are answers, and resending them is a loop.
        assert!(DocumentFailure::Engine("worker died".into()).is_retryable());
        assert!(!DocumentFailure::Refused("off the page".into()).is_retryable());
        assert!(!DocumentFailure::RevisionConflict {
            expected: DocumentRevision(1),
            actual: DocumentRevision(2),
        }
        .is_retryable());
    }

    #[test]
    fn every_engine_error_crosses_the_wire_as_something_a_caller_can_act_on() {
        use super::super::DocumentError as E;
        let id = IdGenerator::new(0).next_id();
        let cases = [
            (
                E::RevisionConflict {
                    expected: DocumentRevision(1),
                    actual: DocumentRevision(2),
                },
                false,
            ),
            (E::NoSuchPage { page: 9, count: 3 }, false),
            (E::NoSuchAnnotation(id.clone()), false),
            (E::NoSuchField("total".into()), false),
            (E::NotEditable(id), false),
            (E::MutationForbidden, false),
            (E::SourceIsDestination, false),
            (E::Backend("boom".into()), true),
            (E::Save("disk full".into()), true),
        ];
        for (error, retryable) in cases {
            let failure = DocumentFailure::from(&error);
            assert_eq!(failure.is_retryable(), retryable, "{error}");
            assert!(!failure.message().is_empty());
        }
    }

    #[test]
    fn requests_and_answers_survive_the_wire() {
        let request = DocumentRequest::FormEvent {
            page: PageIndex(2),
            event: FormInputEvent::PointerDown {
                at: PagePoint::new(10.0, 20.0),
            },
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<DocumentRequest>(&encoded).unwrap(),
            request
        );

        let answer = DocumentResponse::Form(FormEventResult {
            invalidated: vec![PageRect::new(0.0, 0.0, 10.0, 10.0)],
            committed: Some(CommittedField {
                name: "name".into(),
                value: "Ada".into(),
                revision: DocumentRevision(4),
            }),
        });
        let encoded = serde_json::to_string(&answer).unwrap();
        assert_eq!(
            serde_json::from_str::<DocumentResponse>(&encoded).unwrap(),
            answer
        );
    }

    #[test]
    fn a_form_event_that_changed_nothing_answers_with_nothing() {
        let result = FormEventResult::default();
        assert!(result.invalidated.is_empty());
        assert!(result.committed.is_none());
        assert!(DocumentResponse::Form(result).validate().is_ok());
    }
}
