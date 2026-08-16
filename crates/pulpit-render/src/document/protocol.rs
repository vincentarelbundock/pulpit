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
pub const DOCUMENT_PROTOCOL_VERSION: u32 = 4;

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
    ///
    /// One event shape covers both kinds of choice field, because
    /// `FORM_SetIndexSelected` already does. On a single-select combo box or
    /// list box the engine clears whatever else was chosen, so
    /// `{ index, selected: true }` means "choose only this". On a
    /// *multi-select* list box it sets the state of that index and leaves the
    /// others alone, so the same event is a toggle: send `selected: true` to
    /// add a row and `selected: false` to take one away. Nothing here has to
    /// say which kind of field it is talking to — the field's own `/Ff` bit 22
    /// decides, inside PDFium, which is where that decision belongs.
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
    /// page is drawn at `full_width` across. Same renderer, same
    /// document, same appearance — which is what §9.4 requires of a partial
    /// composite, and why this is a crop rather than a second way to draw.
    #[serde(default = "full_region")]
    pub region: pulpit_core::notes::Region,
    /// The whole page's size in pixels, when the caller already has a frame of
    /// it. Zero means "work it out from the region", which rounds and so lands
    /// within a pixel of the frame's own scale instead of on it — a partial
    /// repaint composited at that scale shows a seam (§9.4).
    #[serde(default)]
    pub full_width: u32,
    #[serde(default)]
    pub full_height: u32,
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

    /// The full page size to draw at, or `None` to derive it from the region.
    pub fn full_size(&self) -> Option<(u32, u32)> {
        (self.full_width > 0 && self.full_height > 0).then_some((self.full_width, self.full_height))
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
        // A page smaller than the crop taken out of it is not a page.
        if let Some((full_width, full_height)) = self.full_size() {
            if full_width < self.width
                || full_height < self.height
                || full_width > Self::MAX_DIMENSION
                || full_height > Self::MAX_DIMENSION
            {
                return Err(LimitExceeded {
                    what: "a full page size that does not contain the region",
                    limit: Self::MAX_DIMENSION as usize,
                });
            }
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
    /// What the document's own JavaScript asked the host to do while this
    /// event was being handled. Empty for almost every event, because almost
    /// no field script calls out to the viewer.
    ///
    /// Reported rather than performed. See [`HostRequest`].
    ///
    /// No `skip_serializing_if` here, and that is load-bearing rather than an
    /// omission. This wire is bincode, which is not self-describing: a field
    /// the encoder skips is not a field the decoder knows to skip, so an empty
    /// vector would be written as nothing and read as whatever bytes came
    /// next. The symptom is a session that dies with "unexpected end of file"
    /// on the *common* path — an event with no host requests, which is almost
    /// every keystroke — while the rare path with an alert in it works.
    pub requests: Vec<HostRequest>,
    /// Whether a text field holds the caret now that this event has been
    /// handled.
    ///
    /// The application cannot work this out for itself, and getting it wrong
    /// is the difference between typing a name into a field and turning the
    /// page: every letter is also a shortcut. So it travels back with every
    /// event, including the ones that changed nothing — a click on bare page
    /// takes the caret *out* of a field, and that answer matters as much as
    /// the one that put it there.
    /// (No `serde` attribute, for the reason given on `requests` above: on a
    /// bincode wire, every field is positional and always present.)
    pub text_focus: bool,
    /// The closed combo box holding the focus, when one does.
    ///
    /// Populated for combo boxes only, and the asymmetry is measured rather
    /// than assumed. A *list* box responds to `FORM_OnKeyDown` with an arrow
    /// key by moving its selection, so it needs nothing here. A closed combo
    /// box ignores the same key entirely — the value does not change and
    /// nothing is invalidated — because in a real viewer that key would be
    /// travelling to a dropdown that is not open. `FORM_SetIndexSelected` is
    /// PDFium's own way in, and choosing the index to pass it needs to know
    /// what is selected now and how many options there are.
    pub focused_choice: Option<FocusedChoice>,
    /// This event pressed a non-editable choice widget, and the engine focused
    /// it *without* opening its own list.
    ///
    /// The press is answered by focus alone because the list a click would
    /// open is transient viewer chrome: PDFium draws it into the page bitmap,
    /// reports slivers of it as invalidated, and asks for a round trip per
    /// hovered row. The application draws the list instead, from
    /// [`Self::focused_choice`], and commits what is chosen through
    /// [`FormInputEvent::SelectOption`] — so the value and its appearance are
    /// still PDFium's, and only the open list is not (§8.6).
    /// (No `serde` attribute: bincode is positional. See `requests`.)
    pub opened_choice: bool,
    /// What the field that just took the caret expects, when it expects
    /// something in particular — "date, as dd mmmm yyyy".
    ///
    /// A date field in a PDF looks exactly like a text field: same box, same
    /// caret, and in Acrobat a calendar that pulpit does not have. Without
    /// this the only way to learn what to type is to type something wrong and
    /// watch the format script rewrite it. Carried on the event that gives the
    /// field focus so it can be said at the moment it is useful.
    pub focused_hint: Option<String>,
    /// The date field holding the caret, when one does.
    ///
    /// A PDF says a field is a date and says the pattern; it does not offer a
    /// calendar, because a calendar is a *viewer's* answer to that — Acrobat
    /// and PDF Studio each draw their own. pulpit draws one too, and this is
    /// what it needs: which field, what shape the value takes, and where the
    /// widget is so the calendar can open beside it rather than somewhere
    /// else on the page.
    pub focused_date: Option<FocusedDate>,
    /// Where the widget holding the focus is, whatever kind of field it
    /// belongs to.
    ///
    /// PDFium draws its own focus decoration into the bitmap it patches, and
    /// that decoration is in the *patch* only: a full page frame comes from
    /// the render pool's own form environment, which has no focus in it, so
    /// the ring blinks out the moment a fresh frame supersedes a patch (A2).
    /// Reported here so the application can draw the indicator itself, where
    /// it survives every frame swap by construction.
    ///
    /// `None` when nothing is focused, and when the focused widget is not on
    /// the page the event was sent to — a ring cannot be drawn on a page the
    /// event does not name.
    pub focused_widget: Option<FocusedWidget>,
}

/// The widget with the focus, wherever it is (§8.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusedWidget {
    pub field: String,
    pub page: PageIndex,
    /// Where the widget is, in canonical page space (A4), so the caller can
    /// place an indicator over it without knowing anything about PDF
    /// coordinates.
    pub bounds: PageRect,
}

/// The date field with the caret in it (§8.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusedDate {
    pub field: String,
    /// The Acrobat pattern the field's own format script names — `dd mmmm
    /// yyyy`. A numbered preset — `AFDate_Format(2)` — arrives translated
    /// through Acrobat's fixed preset table; empty only for a preset that
    /// table does not know.
    pub pattern: String,
    pub page: PageIndex,
    /// Where the widget is, in canonical page space (A4), so the caller can
    /// place the calendar without knowing anything about PDF coordinates.
    pub bounds: PageRect,
}

/// What the focused choice field currently holds (§8.6).
///
/// Reported for a combo box and for a list box alike. It carries the labels as
/// well as the count because a *non-editable* choice field's open list is
/// drawn by the application rather than by PDFium: a list that is only ever
/// viewer chrome — it appears in no saved file — so drawing it app-side costs
/// §8.6 nothing, while the value it chooses still goes back through
/// [`FormInputEvent::SelectOption`] and is still PDFium's to commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusedChoice {
    /// The field's name, so the application can tell the caret moving *within*
    /// a field from its moving to another one.
    pub field: String,
    /// Which option is chosen, if any. `None` for a combo box holding a value
    /// that is not in its own `/Opt` list, which is a case the corpus carries.
    ///
    /// The *first* of [`Self::selections`] when several are chosen. A single
    /// index cannot describe a multi-select list box, which is what the field
    /// below is for; this one stays because everything that steps a combo box
    /// asks "which one is on", and for a combo box there is only ever one.
    pub selected: Option<u32>,
    /// Every chosen option, by index, in ascending order. One entry at most
    /// for a combo box or a single-select list box; any number for a
    /// multi-select list box, which is the whole reason it exists — the drawn
    /// list ticks its rows from this, and `selected` alone would tick one row
    /// of three. (No `serde` attribute: bincode is positional, so a default
    /// would silently mis-frame the rest of the struct.)
    pub selections: Vec<u32>,
    /// How many options there are, so a caller can step within them without
    /// asking for the list.
    pub options: u32,
    /// The option labels, in the order PDFium indexes them — which is the
    /// order a [`FormInputEvent::SelectOption`] index names. Bounded by
    /// [`limits::MAX_FIELD_OPTIONS`] and each label by
    /// [`limits::MAX_FIELD_VALUE_BYTES`], like every other list on this wire.
    pub labels: Vec<String>,
    /// Whether the field takes a value of its own — `/Ff` bit 19, an editable
    /// combo box. An editable combo is a text box with a list attached, and
    /// its list is PDFium's: drawing one over a caret PDFium is also drawing
    /// would be two editing surfaces for one field, which §8.6 forbids.
    pub editable: bool,
    /// Whether the field takes several options at once. Reported so the
    /// application can say what it does not offer rather than quietly choosing
    /// one option where the field allows three.
    pub multiple_selection: bool,
    /// Whether the field is a list box rather than a combo box. A list box is
    /// always showing its rows; a combo box shows one.
    pub list_box: bool,
    /// Where the widget is, in canonical page space (A4), so an
    /// application-drawn list can be anchored to the field it belongs to.
    pub page: PageIndex,
    pub bounds: PageRect,
}

/// Something a document's JavaScript asked the host to do (§8.6).
///
/// PDFium's JS platform is a set of callbacks a viewer is expected to answer
/// *synchronously*, from inside the form event that triggered them —
/// `app.alert` blocks until its dialog is dismissed. pulpit cannot do that: the
/// callback runs on the worker process, which has no UI and must not block the
/// application waiting for one.
///
/// So a request is recorded and returned with the event. The script sees the
/// answer a dismissed dialog would have given and runs to completion; the
/// application, which is the layer with a user in front of it, decides what to
/// show and what to allow.
///
/// Nothing in the worker performs any of these. Egress especially is a decision
/// for a user, not for a sandboxed process holding a hostile document (A8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostRequest {
    /// `app.alert(…)`.
    Alert { message: String, title: String },
    /// `app.beep(…)`.
    Beep,
    /// `app.response(…)` — a question with an entry field. Answered with
    /// nothing, because there is no one on the worker process to ask.
    Response { question: String, title: String },
    /// The document asked for its own path. Refused: a form does not need to
    /// know where on disk it is, and telling it puts the user's home directory
    /// into a string a script could then try to submit somewhere.
    FilePath,
    /// `doc.mailDoc(…)`, recorded with its recipients so the application can
    /// say what was attempted. Never sent from the worker.
    Mail { to: String, subject: String },
    /// `doc.print(…)`.
    Print,
    /// `doc.submitForm(url)`. The field data is deliberately not carried — only
    /// its size and the destination, which is what a user needs in order to
    /// decide. The application can re-serialise the form itself if the answer
    /// is yes.
    SubmitForm { url: String, bytes: usize },
    /// The document asked to jump to a page. The application may honour this:
    /// it is navigation inside the document and reaches nothing outside it.
    GotoPage { page: usize },
    /// A file-picker for a file-selection field. Refused, as `FFI_OpenFile` is.
    Browse,
    /// A named viewer action — `NextPage`, `Print`, `SaveAs` and friends.
    NamedAction { name: String },
}

/// A field value the engine committed, and the revision that carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommittedField {
    pub name: String,
    pub value: String,
    /// What the field held before this commit.
    ///
    /// The inverse of the edit, and the only thing that lets a filled field
    /// join the same undo history as the annotations (§9.1). Captured before
    /// the event was dispatched, because afterwards the old value is gone.
    pub previous: String,
    pub revision: DocumentRevision,
    /// Which options are chosen now, by index, for a choice field. Empty for
    /// every other kind. (No `serde` attribute for the reason given on
    /// [`FormEventResult::requests`]: bincode is positional.)
    pub selected: Vec<u32>,
    /// Which options were chosen before this commit — the selection half of
    /// `previous`, and the only faithful before-image a multi-select list box
    /// has: three selections cannot be named by one string.
    pub previous_selected: Vec<u32>,
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
    Form(Box<FormEventResult>),
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
            DocumentResponse::Form(result) => {
                limits::within(
                    "invalidated rectangles",
                    result.invalidated.len(),
                    limits::MAX_ANNOTATIONS_PER_PAGE,
                )?;
                // The option list the application is about to draw comes from
                // a document it does not trust, so it is bounded here like
                // every other list that crosses this wire (A8).
                if let Some(choice) = &result.focused_choice {
                    limits::within(
                        "options in a focused choice field",
                        choice.labels.len(),
                        limits::MAX_FIELD_OPTIONS,
                    )?;
                    for label in &choice.labels {
                        limits::within(
                            "option label length",
                            label.len(),
                            limits::MAX_FIELD_VALUE_BYTES,
                        )?;
                    }
                    // A multi-select list box can have as many selections as
                    // it has options and no more, so this is bounded by the
                    // same limit rather than by a second one.
                    limits::within(
                        "chosen options in a focused choice field",
                        choice.selections.len(),
                        limits::MAX_FIELD_OPTIONS,
                    )?;
                }
                Ok(())
            }
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
    fn a_patch_carries_the_frame_it_is_to_be_composited_into() {
        let patch = DocumentRenderRequest {
            page: PageIndex(0),
            width: 100,
            height: 50,
            expected_revision: DocumentRevision::INITIAL,
            region: pulpit_core::notes::Region::new(0.25, 0.5, 0.25, 0.25),
            full_width: 401,
            full_height: 201,
        };
        assert!(patch.validate().is_ok());
        // The size the caller gave, not the one the region rounds to: 100/0.25
        // is 400, and a patch drawn on that page is a pixel off the frame.
        assert_eq!(patch.full_size(), Some((401, 201)));

        // A whole-page render says nothing and is drawn at its own size.
        let full = DocumentRenderRequest {
            region: pulpit_core::notes::Region::FULL,
            full_width: 0,
            full_height: 0,
            ..patch
        };
        assert_eq!(full.full_size(), None);
        assert!(full.validate().is_ok());

        // A page smaller than the crop taken out of it is not a page.
        assert!(DocumentRenderRequest {
            full_width: 10,
            ..patch
        }
        .validate()
        .is_err());

        // Absent on the wire — an older peer's request — reads as "derive it".
        let wire = serde_json::to_string(&patch).expect("the request serialises");
        assert_eq!(
            serde_json::from_str::<DocumentRenderRequest>(&wire).unwrap(),
            patch
        );
        let older: DocumentRenderRequest =
            serde_json::from_str(r#"{"page":0,"width":100,"height":50,"expected_revision":0}"#)
                .expect("a request without the fields still parses");
        assert_eq!(older.full_size(), None);
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
                format: crate::document::model::FieldFormat::Plain,
                options: Vec::new(),
                allows_custom_value: true,
                multiple_selection: false,
                required: false,
                password: false,
                file_select: false,
                rich_text: false,
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

        let answer = DocumentResponse::Form(Box::new(FormEventResult {
            invalidated: vec![PageRect::new(0.0, 0.0, 10.0, 10.0)],
            committed: Some(CommittedField {
                name: "name".into(),
                value: "Ada".into(),
                previous: String::new(),
                revision: DocumentRevision(4),
                selected: Vec::new(),
                previous_selected: Vec::new(),
            }),
            requests: vec![HostRequest::Alert {
                message: "filled".into(),
                title: "pulpit".into(),
            }],
            text_focus: true,
            focused_choice: Some(FocusedChoice {
                field: "country".into(),
                selected: Some(1),
                selections: vec![1],
                options: 2,
                labels: vec!["France".into(), "Japan".into()],
                editable: false,
                multiple_selection: false,
                list_box: false,
                page: PageIndex(2),
                bounds: PageRect::new(10.0, 20.0, 120.0, 36.0),
            }),
            opened_choice: true,
            focused_hint: None,
            focused_date: None,
            focused_widget: Some(FocusedWidget {
                field: "name".into(),
                page: PageIndex(2),
                bounds: PageRect::new(10.0, 20.0, 120.0, 36.0),
            }),
        }));
        let encoded = serde_json::to_string(&answer).unwrap();
        assert_eq!(
            serde_json::from_str::<DocumentResponse>(&encoded).unwrap(),
            answer
        );
    }

    #[test]
    fn an_option_list_the_application_would_draw_is_bounded() {
        let choice = |labels: Vec<String>| {
            DocumentResponse::Form(Box::new(FormEventResult {
                focused_choice: Some(FocusedChoice {
                    field: "country".into(),
                    selected: None,
                    selections: Vec::new(),
                    options: labels.len() as u32,
                    labels,
                    editable: false,
                    multiple_selection: false,
                    list_box: true,
                    page: PageIndex(0),
                    bounds: PageRect::new(0.0, 0.0, 10.0, 10.0),
                }),
                ..FormEventResult::default()
            }))
        };
        assert!(choice(vec!["one".into(), "two".into()]).validate().is_ok());
        assert!(choice(vec!["x".into(); limits::MAX_FIELD_OPTIONS + 1])
            .validate()
            .is_err());
        assert!(choice(vec!["x".repeat(limits::MAX_FIELD_VALUE_BYTES + 1)])
            .validate()
            .is_err());
    }

    #[test]
    fn a_form_event_that_changed_nothing_answers_with_nothing() {
        let result = FormEventResult::default();
        assert!(result.invalidated.is_empty());
        assert!(result.committed.is_none());
        assert!(result.focused_widget.is_none());
        assert!(DocumentResponse::Form(Box::new(result)).validate().is_ok());
    }
}
