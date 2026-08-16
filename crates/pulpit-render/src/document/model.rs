//! The document API's data types: what an annotation looks like from outside
//! the engine, what a form field is, what a mutation asks for and what it
//! reports back.
//!
//! None of these carries a PDFium handle or an indirect object number (A3),
//! which is what lets every one of them cross the worker protocol, be written
//! to the recovery journal and be replayed against a freshly opened document.

use pulpit_core::annotate::{
    AnnotationCommand, AnnotationDraft, AnnotationId, AnnotationKind, MarkStyle,
};
use pulpit_core::page::{PageGeometry, PageIndex, PagePoint, PageQuad, PageRect};
use serde::{Deserialize, Serialize};

use super::limits;

/// A session-local monotonic counter, incremented by every successful
/// mutation (§9.1).
///
/// Not PDF metadata and not a persistent version identifier: it exists so a
/// render result can name exactly which state it contains, and so a delayed
/// message cannot overwrite a later change (A7).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct DocumentRevision(pub u64);

impl DocumentRevision {
    /// What an open document starts at.
    pub const INITIAL: DocumentRevision = DocumentRevision(0);

    pub fn next(self) -> DocumentRevision {
        DocumentRevision(self.0 + 1)
    }
}

impl std::fmt::Display for DocumentRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// How much of a document pulpit can honour (§3.4). Displayed to the user, so
/// a limitation is a stated one rather than a surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityLevel {
    /// Opens, renders, fields recognised, no unsupported required actions.
    Native,
    /// Fields editable; some JavaScript, validation, formatting, submission or
    /// appearance behaviour unavailable.
    NativeWithLimitations,
    /// No usable form semantics, but the page renders and every annotation
    /// tool works.
    ///
    /// The default, because it is the *least* pulpit promises about a document
    /// it has not surveyed yet. Promising more and withdrawing it is worse
    /// than promising less and finding more.
    #[default]
    AnnotateOnly,
    /// Does not render, or cannot be opened safely.
    Unsupported,
}

impl CompatibilityLevel {
    pub fn label(self) -> &'static str {
        match self {
            CompatibilityLevel::Native => "Native",
            CompatibilityLevel::NativeWithLimitations => "Native with limitations",
            CompatibilityLevel::AnnotateOnly => "Annotate only",
            CompatibilityLevel::Unsupported => "Unsupported",
        }
    }

    /// May this document be annotated at all?
    pub fn allows_annotation(self) -> bool {
        !matches!(self, CompatibilityLevel::Unsupported)
    }

    /// May its form fields be filled?
    pub fn allows_form_filling(self) -> bool {
        matches!(
            self,
            CompatibilityLevel::Native | CompatibilityLevel::NativeWithLimitations
        )
    }
}

/// Something about a document the user is told before they start editing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentWarning {
    /// The document is encrypted. Permissions are honoured and mutation may be
    /// refused outright.
    Encrypted,
    /// The document carries an XFA form, whose dynamic behaviour is deferred
    /// (§3.3). The AcroForm shadow, where there is one, still fills.
    XfaForm,
    /// The document carries JavaScript, which pulpit never executes (§12).
    JavaScript,
    /// The document is cryptographically signed. Editing it can invalidate
    /// that signature (A9), and the user is told *before* the first mutation.
    Signed,
    /// The document sets permissions that forbid annotation or form filling.
    MutationForbidden,
    /// The document has a form, and PDFium would not give pulpit an
    /// environment to fill it through. The pages still render and every
    /// annotation tool still works; the fields are read-only.
    FormUnavailable,
    /// A field script calls out of the document — submitting, mailing or
    /// opening a URL. pulpit refuses every one of those, and says so here
    /// rather than refusing silently.
    ScriptReachesOut,
}

impl DocumentWarning {
    pub fn message(&self) -> &'static str {
        match self {
            DocumentWarning::Encrypted => {
                "This document is encrypted. Some edits may be refused by its own permissions."
            }
            DocumentWarning::XfaForm => {
                "This document uses an XFA form. Its dynamic behaviour is not available; \
                 any ordinary AcroForm fields it also carries can still be filled."
            }
            DocumentWarning::JavaScript => {
                "This document contains JavaScript. Its field formatting, validation \
                 and calculations run; anything it asks to send, open or print does not."
            }
            DocumentWarning::Signed => {
                "This document is signed. Saving a modified copy will not carry the \
                 signature's validity with it."
            }
            DocumentWarning::MutationForbidden => {
                "This document's permissions do not allow it to be changed."
            }
            DocumentWarning::FormUnavailable => {
                "This document's form fields cannot be filled. The pages still render \
                 and every annotation tool still works."
            }
            DocumentWarning::ScriptReachesOut => {
                "This form tries to send itself somewhere. pulpit fills it and saves it \
                 locally; nothing is submitted, mailed or opened over the network."
            }
        }
    }
}

/// What the engine knows about an open document up front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenDocumentInfo {
    pub page_count: usize,
    pub level: CompatibilityLevel,
    pub warnings: Vec<DocumentWarning>,
    /// The page pulpit measured geometry for, up to the tracked bound; pages
    /// beyond it are measured on demand.
    pub first_page: PageGeometry,
    pub has_form: bool,
}

/// How well pulpit understands one imported annotation (§10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationSupport {
    /// Round-trips through the model without known loss.
    Editable,
    /// Understood and selectable, but editing it would lose data.
    ReadOnlySupported,
    /// Preserved and rendered, with bounded summary metadata only.
    Unsupported,
    /// Ignored or rendered only as far as is safe, with a diagnostic.
    Malformed,
}

impl AnnotationSupport {
    pub fn is_editable(self) -> bool {
        matches!(self, AnnotationSupport::Editable)
    }
}

/// The contents of an annotation, as much as fits in a summary.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnnotationContents {
    /// `/Contents`, bounded by [`limits::MAX_TEXT_BYTES`].
    pub text: String,
    /// True when the text was cut to fit the bound, so an inspector can say
    /// so rather than showing a silently truncated sentence.
    pub truncated: bool,
    /// pulpit's own namespaced metadata — Typst source, principally — when
    /// the annotation carries it (§7.4).
    pub pulpit_source: Option<String>,
}

/// One annotation, as the application sees it (§6.1).
///
/// Carries enough geometry for hit-testing and inspectors, and no raw object
/// reference. Large ink arrays and quad lists may be absent from an
/// enumeration and fetched by id, which is what keeps a page of ten thousand
/// strokes inside the protocol's message bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationSummary {
    pub id: AnnotationId,
    pub page: PageIndex,
    pub kind: AnnotationKind,
    pub bounds: PageRect,
    pub style: MarkStyle,
    pub contents: AnnotationContents,
    pub support: AnnotationSupport,
    pub revision: DocumentRevision,
    /// The stroke's centre line, when it was cheap enough to include.
    pub path: Vec<PagePoint>,
    /// The marked text runs, when they were cheap enough to include.
    pub quads: Vec<PageQuad>,
    /// True when `path` or `quads` was left out of this summary and must be
    /// fetched by id. An empty vector and an omitted one are different things,
    /// and a hit-test that confused them would silently stop selecting.
    pub geometry_elided: bool,
}

impl AnnotationSummary {
    pub fn editable(&self) -> bool {
        self.support.is_editable()
    }

    /// The modelled content, when pulpit understands this annotation well
    /// enough to describe it.
    ///
    /// `None` for a kind that is preserved rather than modelled (§10.2) —
    /// which is also a kind the editor never offers to change, so nothing can
    /// build a replacement out of one by accident.
    pub fn to_draft(&self) -> Option<AnnotationDraft> {
        use pulpit_core::annotate::{
            FreeTextDraft, HighlightDraft, InkDraft, InkPoint, NoteDraft, StampDraft, StampMark,
            TextSource,
        };

        let style = self.style;
        match self.kind {
            AnnotationKind::Ink => Some(AnnotationDraft::Ink(InkDraft {
                page: self.page,
                points: self.path.iter().map(|at| InkPoint { at: *at }).collect(),
                style,
            })),
            AnnotationKind::Highlight => Some(AnnotationDraft::Highlight(HighlightDraft {
                page: self.page,
                quads: self.quads.clone(),
                text: self.contents.text.clone(),
                style,
            })),
            AnnotationKind::FreeText => Some(AnnotationDraft::FreeText(FreeTextDraft {
                page: self.page,
                rect: self.bounds,
                text: self.contents.text.clone(),
                source: if self.contents.pulpit_source.is_some() {
                    TextSource::Typst
                } else {
                    TextSource::Plain
                },
                style,
            })),
            AnnotationKind::Note => Some(AnnotationDraft::Note(NoteDraft {
                page: self.page,
                at: PagePoint::new(self.bounds.left, self.bounds.top),
                text: self.contents.text.clone(),
                style,
            })),
            AnnotationKind::Stamp => Some(AnnotationDraft::Stamp(StampDraft {
                page: self.page,
                rect: self.bounds,
                mark: StampMark::Check,
                style,
                // Whatever markup generated this mark, so reopening it for
                // editing shows the source rather than the picture (§7.4).
                source: self.contents.pulpit_source.clone(),
            })),
            AnnotationKind::Other => None,
        }
    }

    /// The shape [`pulpit_core::annotate::hit`] tests against.
    pub fn to_hit(&self) -> pulpit_core::annotate::AnnotationHit {
        pulpit_core::annotate::AnnotationHit {
            id: self.id.clone(),
            kind: self.kind,
            bounds: self.bounds,
            path: self.path.clone(),
            quads: self.quads.clone(),
            editable: self.editable(),
            width: self.style.width,
        }
    }
}

/// What kind of control a form field is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldKind {
    Text,
    Checkbox,
    RadioGroup,
    ComboBox,
    ListBox,
    PushButton,
    /// A signature field. Displayed, never filled — signing is
    /// `SPEC-signing.md`'s subject, not this one's.
    Signature,
    /// A field whose type pulpit does not recognise. Displayed read-only so
    /// the user can see it exists rather than wondering where it went.
    Unknown,
}

impl FieldKind {
    /// Can a value be typed or chosen for this kind?
    pub fn is_fillable(self) -> bool {
        matches!(
            self,
            FieldKind::Text
                | FieldKind::Checkbox
                | FieldKind::RadioGroup
                | FieldKind::ComboBox
                | FieldKind::ListBox
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            FieldKind::Text => "Text",
            FieldKind::Checkbox => "Checkbox",
            FieldKind::RadioGroup => "Radio group",
            FieldKind::ComboBox => "Dropdown",
            FieldKind::ListBox => "List",
            FieldKind::PushButton => "Button",
            FieldKind::Signature => "Signature",
            FieldKind::Unknown => "Field",
        }
    }
}

/// Where one field is drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldWidget {
    pub page: PageIndex,
    pub bounds: PageRect,
    /// The value this widget stands for when a field's widgets mean different
    /// things — a radio group's options. `None` when pressing the widget means
    /// the field rather than one of its values.
    pub option: Option<String>,
}

/// One AcroForm field (§6.4).
///
/// This is pdfform's `FormValue`/`WidgetRect` with `NormalizedRect` replaced
/// by canonical [`PageRect`] per A4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    pub name: String,
    pub kind: FieldKind,
    pub value: String,
    /// Which of [`Self::options`] are chosen, by index, in the order the file
    /// lists them.
    ///
    /// A choice field that takes several selections has no single value, and
    /// [`Self::value`] can only ever name one of them — so what was chosen is
    /// said here, where a list of three things can be a list of three things.
    /// Empty for every other kind, and for a choice field with nothing chosen.
    #[serde(default)]
    pub selected: Vec<u32>,
    pub read_only: bool,
    pub options: Vec<String>,
    pub allows_custom_value: bool,
    pub multiple_selection: bool,
    /// Where the field is drawn. Empty when neither the producer nor the
    /// reader of the document could say — the inspector is still a way in.
    pub widgets: Vec<FieldWidget>,
}

impl FormField {
    /// Where this field's editor goes on `page`: at its first widget there.
    ///
    /// A field with more than one rectangle on one page is a radio group's
    /// options or mirrored copies of one value, and neither wants a second
    /// editor.
    pub fn anchor_on(&self, page: PageIndex) -> Option<PageRect> {
        self.widgets
            .iter()
            .find(|widget| widget.page == page)
            .map(|widget| widget.bounds)
    }

    /// Every page this field appears on, in order, without repeats.
    pub fn pages(&self) -> Vec<PageIndex> {
        let mut pages: Vec<PageIndex> = Vec::new();
        for widget in &self.widgets {
            if !pages.contains(&widget.page) {
                pages.push(widget.page);
            }
        }
        pages
    }

    /// Can the user change this one?
    pub fn is_editable(&self) -> bool {
        !self.read_only && self.kind.is_fillable()
    }
}

/// How a text selection was asked for (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextSelection {
    Range { anchor: PagePoint, head: PagePoint },
    Word { at: PagePoint },
    Line { at: PagePoint },
}

/// What the page's text layer said (§6.3).
///
/// A page with no extractable text returns an empty result rather than an
/// error, and the UI reports that the highlighter is unavailable there.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TextSelectionResult {
    /// One quadrilateral per contiguous run, in reading order.
    pub quads: Vec<PageQuad>,
    pub text: String,
    /// True when the selection was cut to fit a protocol bound.
    pub truncated: bool,
}

impl TextSelectionResult {
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
}

/// One thing to do to the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentCommand {
    Annotation(AnnotationCommand),
    SetField { name: String, value: String },
}

impl DocumentCommand {
    pub fn label(&self) -> String {
        match self {
            DocumentCommand::Annotation(command) => command.label(),
            DocumentCommand::SetField { name, .. } => format!("Fill {name}"),
        }
    }
}

/// One atomic user action: a single command for an ordinary edit, several for
/// an eraser sweep or a compound replacement.
///
/// One transaction is one revision increment and one undo entry (§9.1), and it
/// is applied whole or not at all (§9.5).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DocumentTransaction(pub Vec<DocumentCommand>);

impl DocumentTransaction {
    pub fn one(command: DocumentCommand) -> DocumentTransaction {
        DocumentTransaction(vec![command])
    }

    /// Every annotation command a gesture produced, as one transaction.
    pub fn from_annotations(
        commands: impl IntoIterator<Item = AnnotationCommand>,
    ) -> DocumentTransaction {
        DocumentTransaction(
            commands
                .into_iter()
                .map(DocumentCommand::Annotation)
                .collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// What the undo menu calls this action.
    ///
    /// A sweep that erased eleven marks is "Erase", not eleven entries: the
    /// user made one gesture.
    pub fn label(&self) -> String {
        match self.0.as_slice() {
            [] => "Nothing".to_string(),
            [single] => single.label(),
            [first, ..] => first.label(),
        }
    }

    /// Check the transaction against the declared limits before anything is
    /// allocated for it (A8).
    pub fn validate(&self) -> Result<(), limits::LimitExceeded> {
        limits::within(
            "operations per transaction",
            self.0.len(),
            limits::MAX_OPERATIONS_PER_TRANSACTION,
        )?;
        let mut points = 0usize;
        for command in &self.0 {
            match command {
                DocumentCommand::Annotation(annotation) => {
                    if let Some(AnnotationDraft::Ink(ink)) = annotation.draft() {
                        points = points.saturating_add(ink.points.len());
                        limits::within("ink points", ink.points.len(), limits::MAX_POINTS_PER_INK)?;
                    }
                    if let Some(AnnotationDraft::Highlight(highlight)) = annotation.draft() {
                        limits::within(
                            "quadrilaterals",
                            highlight.quads.len(),
                            limits::MAX_QUADS_PER_ANNOTATION,
                        )?;
                    }
                }
                DocumentCommand::SetField { value, .. } => {
                    limits::within("field value", value.len(), limits::MAX_FIELD_VALUE_BYTES)?;
                }
            }
        }
        limits::within(
            "ink points per transaction",
            points,
            limits::MAX_POINTS_PER_TRANSACTION,
        )
    }
}

/// What one command in a transaction did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppliedEffect {
    /// An annotation was created or replaced; this is what it is now.
    Annotation(Box<AnnotationSummary>),
    /// An annotation was deleted.
    Deleted(AnnotationId),
    /// A field now holds this value. The engine's value, not the requested
    /// one: a checkbox asked for "yes" reports the export value it actually
    /// took.
    Field { name: String, value: String },
}

/// What the engine has to keep in order to reverse one transaction (§6.2).
///
/// Opaque and serialisable: a lossless before-image, never a PDFium pointer.
/// It preserves unrecognised dictionary data, so undoing an edit to an
/// imported annotation puts back what was there rather than what pulpit
/// understood of it (A5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentUndo {
    /// The operations that reverse the transaction, in the order to apply
    /// them — the inverse of the forward order, so a sweep is put back the way
    /// it was taken.
    pub operations: Vec<UndoOperation>,
    /// The revision this undoes back to, for diagnostics and for the journal's
    /// replay order. Not a precondition: applying an undo checks the *current*
    /// revision like any other mutation.
    pub restores: DocumentRevision,
    /// What the undo menu calls the action being reversed.
    pub label: String,
}

/// One step of an undo.
///
/// Deliberately not a [`DocumentCommand`]. Reversing a delete has to put the
/// annotation back under *its own identity*, and `Create` cannot: it mints a
/// new one. An undo/redo cycle that renamed every annotation it touched would
/// break A3, and with it every reference the session holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UndoOperation {
    /// Put a deleted annotation back, with its identity and its unrecognised
    /// dictionary entries (A5).
    RestoreAnnotation {
        id: AnnotationId,
        before: Box<AnnotationBeforeImage>,
    },
    /// Put an edited annotation back the way it was.
    ReplaceAnnotation {
        id: AnnotationId,
        before: Box<AnnotationBeforeImage>,
    },
    /// Remove an annotation that was created.
    DeleteAnnotation { id: AnnotationId },
    /// Put a field's previous value back.
    SetField { name: String, value: String },
}

/// A lossless copy of what an annotation was before it was changed.
///
/// `preserved` is the part that matters for A5: any dictionary entry pulpit
/// did not model, kept as opaque bytes so undoing an edit to an imported
/// annotation restores what the other producer wrote rather than pulpit's
/// understanding of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnotationBeforeImage {
    pub page: PageIndex,
    /// The modelled part, when pulpit understood the annotation well enough to
    /// describe it. `None` for an annotation that was preserved rather than
    /// modelled — which is also an annotation the editor never offers to
    /// change, so nothing can produce this case today.
    pub draft: Option<AnnotationDraft>,
    /// Everything pulpit did not model, as the engine chooses to encode it.
    pub preserved: Vec<u8>,
}

/// What a successful mutation reports (§6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    /// One per command, in order.
    pub effects: Vec<AppliedEffect>,
    pub document_revision: DocumentRevision,
    /// A rectangle covering the whole transaction, for a partial repaint.
    /// Full-page rendering stays the correct baseline (§9.4).
    pub dirty_region: Option<PageRect>,
    /// The pages the transaction touched, so a caller knows what to re-render.
    pub dirty_pages: Vec<PageIndex>,
    /// The operation that reverses this one. Applying it is itself a mutation
    /// and returns an `Applied` whose `undo` redoes it, so redo needs no
    /// request of its own (§9.5).
    pub undo: DocumentUndo,
}

/// What to do when saving (§11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SaveOptions {
    /// Write incrementally, appending a new revision rather than rewriting the
    /// file. Preserves any existing signature's byte ranges, at the cost of a
    /// larger file.
    pub incremental: bool,
    /// Re-open the written file and check it before renaming into place. On by
    /// default in every path that matters; off only for a test that wants the
    /// raw bytes.
    pub verify: bool,
}

impl SaveOptions {
    /// The options every ordinary Save As uses.
    pub fn verified() -> SaveOptions {
        SaveOptions {
            incremental: false,
            verify: true,
        }
    }
}

/// What a completed save reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedDocument {
    pub path: std::path::PathBuf,
    /// The revision installed in the output (§9.1).
    pub revision: DocumentRevision,
    pub bytes: u64,
    /// The identities of every annotation the verification pass found in the
    /// reopened file. Empty when verification was not asked for.
    pub verified_ids: Vec<AnnotationId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulpit_core::annotate::{IdGenerator, InkDraft, InkPoint};

    fn ink_command(points: usize) -> DocumentCommand {
        DocumentCommand::Annotation(AnnotationCommand::Create(AnnotationDraft::Ink(InkDraft {
            page: PageIndex(0),
            points: vec![InkPoint::new(1.0, 1.0); points],
            style: MarkStyle::default(),
        })))
    }

    #[test]
    fn revisions_start_at_zero_and_only_go_up() {
        let start = DocumentRevision::INITIAL;
        assert_eq!(start.0, 0);
        assert_eq!(start.next(), DocumentRevision(1));
        assert!(start.next() > start);
        assert_eq!(DocumentRevision(7).to_string(), "r7");
    }

    #[test]
    fn a_transaction_is_checked_against_the_limits_before_it_is_sent() {
        assert!(DocumentTransaction(vec![ink_command(10)])
            .validate()
            .is_ok());

        let too_many = DocumentTransaction(vec![
            ink_command(1);
            limits::MAX_OPERATIONS_PER_TRANSACTION + 1
        ]);
        assert!(too_many.validate().is_err());

        let too_long = DocumentTransaction(vec![ink_command(limits::MAX_POINTS_PER_INK + 1)]);
        assert!(too_long.validate().is_err());

        // Each stroke is legal on its own; together they are not.
        let strokes = limits::MAX_POINTS_PER_TRANSACTION / limits::MAX_POINTS_PER_INK + 1;
        let too_much = DocumentTransaction(vec![ink_command(limits::MAX_POINTS_PER_INK); strokes]);
        assert!(too_much.validate().is_err());
    }

    #[test]
    fn an_over_long_field_value_is_refused_by_the_transaction() {
        let transaction = DocumentTransaction::one(DocumentCommand::SetField {
            name: "comments".into(),
            value: "x".repeat(limits::MAX_FIELD_VALUE_BYTES + 1),
        });
        assert!(transaction.validate().is_err());
    }

    #[test]
    fn one_sweep_is_labelled_as_one_action_however_many_marks_it_took() {
        let mut generator = IdGenerator::new(0);
        let sweep =
            DocumentTransaction::from_annotations((0..11).map(|_| AnnotationCommand::Delete {
                id: generator.next_id(),
            }));
        assert_eq!(sweep.len(), 11);
        assert_eq!(sweep.label(), "Erase");
        assert_eq!(DocumentTransaction::default().label(), "Nothing");
    }

    #[test]
    fn a_fields_editor_goes_at_its_first_widget_on_the_page() {
        let field = FormField {
            name: "choice".into(),
            kind: FieldKind::RadioGroup,
            value: "b".into(),
            read_only: false,
            options: vec!["a".into(), "b".into()],
            allows_custom_value: false,
            multiple_selection: false,
            selected: Vec::new(),
            widgets: vec![
                FieldWidget {
                    page: PageIndex(1),
                    bounds: PageRect::new(10.0, 10.0, 24.0, 24.0),
                    option: Some("a".into()),
                },
                FieldWidget {
                    page: PageIndex(1),
                    bounds: PageRect::new(10.0, 40.0, 24.0, 54.0),
                    option: Some("b".into()),
                },
                FieldWidget {
                    page: PageIndex(3),
                    bounds: PageRect::new(90.0, 40.0, 104.0, 54.0),
                    option: None,
                },
            ],
        };
        assert_eq!(
            field.anchor_on(PageIndex(1)),
            Some(PageRect::new(10.0, 10.0, 24.0, 24.0))
        );
        assert_eq!(field.anchor_on(PageIndex(0)), None);
        assert_eq!(field.pages(), vec![PageIndex(1), PageIndex(3)]);
        assert!(field.is_editable());
    }

    #[test]
    fn a_read_only_or_unfillable_field_is_shown_and_not_edited() {
        let mut field = FormField {
            name: "total".into(),
            kind: FieldKind::Text,
            value: "42".into(),
            read_only: true,
            options: Vec::new(),
            allows_custom_value: false,
            multiple_selection: false,
            selected: Vec::new(),
            widgets: Vec::new(),
        };
        assert!(!field.is_editable());
        field.read_only = false;
        assert!(field.is_editable());
        field.kind = FieldKind::Signature;
        assert!(
            !field.is_editable(),
            "a signature field is not filled by the form editor"
        );
        assert!(!FieldKind::PushButton.is_fillable());
    }

    #[test]
    fn compatibility_levels_say_what_they_permit() {
        assert!(CompatibilityLevel::Native.allows_form_filling());
        assert!(CompatibilityLevel::NativeWithLimitations.allows_form_filling());
        assert!(!CompatibilityLevel::AnnotateOnly.allows_form_filling());
        assert!(CompatibilityLevel::AnnotateOnly.allows_annotation());
        assert!(!CompatibilityLevel::Unsupported.allows_annotation());
        for level in [
            CompatibilityLevel::Native,
            CompatibilityLevel::NativeWithLimitations,
            CompatibilityLevel::AnnotateOnly,
            CompatibilityLevel::Unsupported,
        ] {
            assert!(!level.label().is_empty());
        }
    }

    #[test]
    fn every_warning_says_something_a_user_can_act_on() {
        for warning in [
            DocumentWarning::Encrypted,
            DocumentWarning::XfaForm,
            DocumentWarning::JavaScript,
            DocumentWarning::Signed,
            DocumentWarning::MutationForbidden,
            DocumentWarning::FormUnavailable,
            DocumentWarning::ScriptReachesOut,
        ] {
            assert!(warning.message().len() > 30, "{warning:?}");
        }
        // A9: the signature warning must not claim a saved copy stays valid.
        assert!(DocumentWarning::Signed.message().contains("not carry"));
    }

    #[test]
    fn a_summary_converts_to_the_shape_hit_testing_wants() {
        let summary = AnnotationSummary {
            id: IdGenerator::new(1).next_id(),
            page: PageIndex(0),
            kind: AnnotationKind::Ink,
            bounds: PageRect::new(0.0, 0.0, 10.0, 10.0),
            style: MarkStyle::default(),
            contents: AnnotationContents::default(),
            support: AnnotationSupport::Editable,
            revision: DocumentRevision(3),
            path: vec![PagePoint::new(0.0, 0.0), PagePoint::new(10.0, 10.0)],
            quads: Vec::new(),
            geometry_elided: false,
        };
        let hit = summary.to_hit();
        assert_eq!(hit.id, summary.id);
        assert!(hit.editable);
        assert_eq!(hit.width, summary.style.width);
        assert!(summary.editable());
    }

    #[test]
    fn an_unsupported_annotation_is_never_reported_editable() {
        for support in [
            AnnotationSupport::ReadOnlySupported,
            AnnotationSupport::Unsupported,
            AnnotationSupport::Malformed,
        ] {
            assert!(!support.is_editable(), "{support:?}");
        }
        assert!(AnnotationSupport::Editable.is_editable());
    }
}
