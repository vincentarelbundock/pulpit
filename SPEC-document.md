# pulpit Document Mode Specification

Reading, annotating, filling and safely saving an ordinary PDF, as a mode of
pulpit rather than a second application.

Companion to `SPEC-package.md`. Cryptographic signing is out of scope and is
specified separately in `SPEC-signing.md`.

**MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT** and **MAY** are normative.

This specification supersedes `pdfform/SPEC-SHARED-ANNOTATIONS.md` and the
parts of `pdfform/SPEC.md` that survive the fold described in §14. pdfform is
absorbed: its form model, its AcroForm read/write path, its field editor and
its test corpus move into this workspace, and its application shell, worker,
PDF engine and visual-item model are deleted rather than ported.

The governing product decision is:

> **A1. A completed document annotation has exactly one authoritative
> representation: a native annotation in the open PDF document.**

Pointer, spotlight, selection and unfinished-gesture graphics remain transient
interaction feedback. There is no durable SVG-overlay annotation system, no
annotation sidecar as the primary document representation, and no later
"export overlays" conversion.

---

## 1. Product outcome

pulpit opens a PDF and does three things with it, in one binary, over one
document model:

1. **Presents** it — the existing presenter and audience windows.
2. **Annotates** it — ink, text highlighting, free text, notes, stamps and
   visible signatures, written as native PDF annotations.
3. **Fills** it — AcroForm fields, where the document has them.

A completed annotation is embedded in the saved PDF and remains visible in
standards-compliant PDF viewers. Supported annotations created by another
viewer become editable. Unsupported annotations are preserved without silent
conversion or loss.

The feature is not complete merely because pulpit can flatten marks into page
pixels. A mark is a true annotation only when the output PDF contains an
annotation dictionary associated with the page's `/Annots` array.

The application MUST NOT imply that a visible handwritten signature is a
cryptographic digital signature.

## 2. Reader and Presenter are layouts, not modes

There is no document mode or presentation mode under the hood: no separate
window stack, state machine, binary, interaction policy or persisted layout
classification. Reader and Presenter are layouts in the existing sense of
`crates/pulpit/src/layout/`: trees of cells holding widgets, designed in the
same designer and stored and validated by the same store.

The sole behavioral distinction is the primary viewer widget in that tree.
`CurrentSlide` draws one page fitted to its cell with black filling the unused
space. `DocumentPage` draws pages in continuous scroll. All other differences
are widget placement and configuration and MUST NOT change interaction
semantics elsewhere in the application.

The widget catalog gains document widgets:

```rust
// crates/pulpit/src/widgets/mod.rs
pub enum WidgetKind {
    // … existing presentation widgets …

    // Document
    /// The page surface: continuous scroll, free zoom, annotation and form
    /// interaction. The one widget a document layout cannot omit.
    DocumentPage,
    /// Page counter, page entry, zoom control, fit-width and fit-page.
    DocumentNav,
    /// Bookmarks and page thumbnails.
    DocumentOutline,
    /// Tool selection and style for ink, highlighter, text, notes and stamps.
    AnnotationTools,
}
```

and `layout/builtin.rs` gains a `reader_default()` beside
`presenter_default()` (§2.1).

Consequences that follow from this and are therefore not restated later:

- theming, tokens, icons, toasts, image residency, document watching, panels
  and thumbnails are the existing ones, used unchanged;
- a document layout is user-editable in the designer like any other;
- adding widget kinds is a layout-schema change and MUST go through
  `layout/validate.rs` and the store's existing migration path, so that a
  layout saved by an older version still loads.

Presentation widgets in a document layout, and document widgets in a presenter
layout, are not errors. A cell whose widget has nothing to show renders its
empty behavior, as today.

### 2.1 Two defaults: Presenter and Reader

`built_in_layouts()` returns exactly two layouts, one per mode, and neither is
a variant of the other:

| Layout | Id | Mode | Role |
|---|---|---|---|
| **Presenter Default** | `presenter-default` | presentation | the reference presenter screen, unchanged |
| **Reader** | `reader-default` | document | the layout a document opens with |

Both are `Origin::BuiltIn`, read-only, canonical and warning-free under
`validate()`, like every other built-in. The existing built-in tests assert an
exact ordered id list and a count; both MUST be updated rather than loosened,
because the assertion on exact ids is what keeps a stored user layout's
`LayoutId` from silently colliding with a new built-in.

`reader-default` is a stable identifier. Once released it MUST NOT be reused
for a differently-shaped layout, because stored settings reference it by id.

### 2.2 Reader

**Reader** is a page and the two things you reach for while reading it: a
control band across the top, and an outline rail down the side.

```text
┌──────────────────────────────┬───────────────────────────────┐
│ DocumentNav                  │ AnnotationTools               │  0.07
├──────────┬───────────────────┴───────────────────────────────┤
│          │                                                   │
│ Document │                                                   │
│ Outline  │              DocumentPage                         │  0.93
│          │                                                   │
│   0.18   │                     0.82                          │
└──────────┴───────────────────────────────────────────────────┘
```

```rust
/// **Reader** — the layout a document opens with. The page gets everything
/// that is not a control: a shallow band along the top carries navigation and
/// the annotation tools, and a narrow rail carries the outline.
///
/// The band is the height of a button and no more. The rail is narrower than
/// the presenter's, because it holds page thumbnails and bookmark titles
/// rather than notes set as prose, and every point past what those need is a
/// point the page is not getting.
pub fn reader_default(ratio: AspectRatio) -> Layout {
    let mut b = Builder::new();

    let band_children = vec![
        b.panel(WidgetKind::DocumentNav),
        b.panel(WidgetKind::AnnotationTools),
    ];
    let band = b.split(
        "Navigation and tools",
        Direction::Horizontal,
        &[0.5, 0.5],
        band_children,
    );

    let body_children = vec![b.panel(WidgetKind::DocumentOutline), b.page()];
    let body = b.split(
        "Document",
        Direction::Horizontal,
        &[0.18, 0.82],
        body_children,
    );

    let root = b.split("Reader", Direction::Vertical, &[0.07, 0.93], vec![band, body]);
    finish("Reader", "reader-default", root, ratio)
}
```

Every proportion is a round fraction, as in the presenter built-ins, so the
layout reads the same at any window size and stays easy to reason about when a
user edits a copy of it.

**The page cell needs its own builder helper.** `Builder::slide()` exists
because a slide is the only bright thing on a dark presenter screen and will
look the same way on the wall; it paints no background and no padding. A
document is the opposite case: the page *is* the artifact, the leftover space
around it should read as a mount rather than as void, and a reader is looked at
for an hour rather than glanced at. So:

```rust
/// A page cell: the document on a mount.
///
/// The inverse of [`Builder::slide`]. A slide is bright against a dark screen
/// because that is how it will look on the wall; a page has no wall, and the
/// space a portrait page leaves in a landscape cell should read as a mount
/// around the sheet rather than as a hole. The page surface scrolls inside
/// the cell, so the cell takes no padding of its own.
fn page(&mut self) -> Node {
    let mut cell = Cell::with_widget(self.id(), Widget::new(WidgetKind::DocumentPage));
    cell.background = CellBackground::Canvas;
    cell.padding = 0.0;
    Node::Leaf(cell)
}
```

The mount is not a new variant. `CellBackground::Canvas` already exists for
exactly this role — its own doc comment reads "a light canvas, used to
separate slide content from presenter tools" — and `slide()`'s comment already
names the concept: painting the cell light "turns that leftover space into a
wide grey mount around the page". A new variant would be a layout-schema
change through `layout/validate.rs` and the store migration (§2) for what is a
colour decision; if `Canvas` reads wrong behind a white page, the fix is
retuning the `Canvas` token in `theme/tokens.rs`.

**There is no field panel.** Most PDFs have no AcroForm, and a rail that is
empty for most documents is worse than none. Fields are edited in place on
the page through the form-fill environment (§8.6), which is the whole of the
form story: one editing surface, no inspector beside it.

### 2.3 Choosing between the defaults

Which default opens is a property of the document, not of a global setting:

- a document opened for reading, filling or annotating starts in `reader-default`;
- a presentation started from the presenter starts in `presenter-default`;
- each mode remembers its own last-used layout independently, so choosing a
  presenter variant never changes what a PDF opens into and the reverse;
- switching a live session between modes keeps the document open and the
  revision unchanged. Mode is which layout is mounted, not which document is
  loaded.

An unsaved document MUST NOT be closed by a mode switch; if presentation mode
cannot show unsaved annotations, it shows them, because they are in the
document (A1), not in an overlay.

### 2.4 Aspect ratio

`AspectRatio` is a design aid: layouts are stored proportionally and scale to
whatever they land on, and the ratio only sets what the designer previews. The
presenter built-ins are authored at `SixteenNine` because a presenter screen is
a screen.

A Reader is used in an application window at whatever size the user dragged it
to, and often a tall one, since a page is portrait. `reader_default(ratio)`
therefore takes its design ratio: a caller with a live window passes
`AspectRatio::Detected { width, height }` from that window, and
`built_in_layouts()` passes the `SixteenNine` fallback so the built-in list
stays parameterless, display-free and testable exactly as today.

This is the only place a built-in's ratio is not a fixed preset, and it exists
so the designer previews a Reader the size a Reader is actually used at.
`finish()` takes the ratio as a parameter rather than hard-coding
`SixteenNine`.

## 3. Scope

### 3.1 Required annotation kinds

| User-facing kind | PDF representation | Editable |
|---|---|---|
| Ink | `/Subtype /Ink` | Yes |
| Text highlighter | `/Subtype /Highlight` with `/QuadPoints` | Yes |
| Plain text | `/Subtype /FreeText` | Yes |
| Typst text | `/FreeText` or `/Stamp` with generated `/AP` | Yes, when pulpit metadata is present |
| Sticky note | `/Subtype /Text` | Yes |
| Check, cross, visible signature | `/Subtype /Stamp`, or `/Ink` for drawn signatures | Yes |

Marks are true annotation objects, never flattened page content.

### 3.2 Transient effects

The following are never written to the PDF:

- pointer dot;
- spotlight and page dimming;
- eraser cursor;
- hover, focus, selection bounds and resize handles;
- a text selection that has not been committed to a highlight;
- an unfinished pointer gesture;
- a text caret and uncommitted IME composition;
- drag previews before a move or resize is committed.

Transient state MUST NOT enter the session snapshot. The journal of §11.1
records only committed commands; an in-progress text edit is committed or
cancelled per §8.5 and is never snapshotted mid-composition.

### 3.3 Deferred

- replies and threaded comments;
- audio, movie, 3D, rich-media and file-attachment annotations;
- redaction;
- cryptographic signatures;
- collaborative annotation merging;
- editing arbitrary vendor appearance streams;
- freehand highlighter as a durable annotation: a freehand transparent stroke
  is either an `/Ink` annotation created with the ink tool or a
  presentation-only effect, never a `/Highlight`;
- squiggly, underline and strike-out text markup, which reuse the
  highlighter's selection machinery and MAY be added once `/Highlight` ships;
- highlighting text in pages with no extractable text layer, including scanned
  pages awaiting OCR;
- pressure-sensitive ink unless the PDF mapping is first specified;
- partial erasing of a stroke, unless whole-stroke erasing proves inadequate;
- dynamic XFA execution, and PDF JavaScript outside a form's own field scripts
  (§4): document-level scripts, open actions, and any script effect that would
  leave the process;
- OCR-driven field recognition;
- in-place save over the source file (A6, §11.3).

Unsupported annotations remain renderable and preservable even when editing is
deferred.

### 3.4 Compatibility levels

Every opened document is assigned one level, displayed to the user:

- **Native** — opens, renders, AcroForm fields recognized, required
  interactions supported, no unsupported required actions.
- **Native with limitations** — fields editable; some JavaScript, validation,
  formatting, submission, appearance or IME-composition behavior (§8.6)
  unavailable.
- **Annotate only** — native form semantics absent or unavailable; the page
  renders as a stable surface and every annotation tool works.
- **Unsupported** — the document does not render, or cannot be opened safely.

Encryption, XFA, JavaScript and existing signatures MUST produce a clear
warning at this level rather than a silent degradation.

## 4. Non-negotiable invariants

### A1. One committed representation

Every completed supported annotation is created in the worker's open PDF
document immediately upon commit. Application state MAY cache a summary for
selection and undo; that cache is not a second persistence format.

### A2. Gesture state is bounded and ephemeral

The UI MAY draw an unfinished stroke directly for latency. On pointer release
it sends one create or replace command. The preview is discarded only after a
rendered frame at or beyond that command's document revision arrives.

### A3. Stable identity

Every editable annotation has an `AnnotationId`. New annotations write that ID
to the standard PDF `/NM` entry. Imported annotations with a unique valid `/NM`
use it. Missing or duplicate names receive session identities, and such an
annotation is written a fresh unique `/NM` the first time it is modified or
saved. Once written, an `/NM` is stable: saving again does not rename it, which
is what lets §16.2 and the round-trip tests of §13.3 enumerate by
`AnnotationId` after reopening.

An indirect object number is never the sole durable identity, because save
operations may renumber objects.

### A4. Page-space geometry

Persistent geometry uses PDF page points in an explicitly defined canonical
space. It never uses window pixels or rendered-bitmap pixels.

Canonical page space has:

- origin at the displayed crop box's top-left;
- positive x to the right and positive y downward;
- dimensions measured in PDF points after page rotation;
- finite coordinates bounded to a documented margin around the page.

`pulpit-render` alone converts canonical geometry to PDF user space, including
crop-box offsets, bottom-left origin, `/UserUnit` and page rotation. Every
conversion has inverse and round-trip tests.

The presenter's normalised points are converted at the UI boundary using the
page's canonical width and height. Normalised coordinates MUST NOT enter the
PDF API. pdfform's `NormalizedRect` does not survive the fold (§14).

### A5. Unsupported data survives

Opening, filling forms or editing supported annotations MUST NOT delete or
rewrite unsupported annotations. The writer preserves unrecognized dictionary
entries and existing appearance streams unless the user modifies that exact
annotation and the engine has declared it editable.

### A6. Source files remain immutable

The source path is rejected as a save destination. Document mode saves via
Save As. Any later in-place save MUST be separately specified and MUST use a
recoverable atomic replacement.

### A7. No stale-frame handoff

Every successful mutation increments a `DocumentRevision`. Every render result
names the revision it contains. A UI preview is removed only when a frame for
the affected page carries the expected or a later revision.

### A8. Bounded hostile-input handling

Annotation counts, ink point counts, quad counts, text lengths, appearance
sizes, nesting and decoded allocation sizes are bounded before allocation or
rendering. Malformed annotation data produces a diagnostic, not a process
abort.

### A9. Signed-document honesty

Changing annotations or field values changes the document and can invalidate
cryptographic signatures. pulpit MUST detect existing signatures and warn
before the first mutation. It MUST NOT report a previously valid signature as
valid after saving a modified document unless `SPEC-signing.md` establishes
that result.

## 5. Architecture

### 5.1 Workspace

No new binary. Per `SPEC-package.md` §1 the render worker is a role of the
single executable, re-executed with a flag; document mutation is a capability
of that existing worker, not a new helper.

```text
pulpit-core
  annotation tools, gesture state, hit-testing, styles, commands, undo grouping

pulpit-render
  PDF engine: PDFium binding, document and page lifetime, canonical transforms,
  text extraction, native annotation model, AcroForm read/write, appearance
  generation, rasterization, revisions, save, worker protocol and transport

pulpit-display, pulpit-media
  unchanged

pulpit
  presenter UI, audience synchronization, pointer and spotlight,
  document mode layout and widgets, undo presentation, session and recovery

pulpit-testkit  (new, dev-only)
  the AcroForm hazard corpus, PDF builder, mutation and verification helpers
```

Forms live as a module inside `pulpit-render` rather than a separate crate.
This follows the workspace's demonstrated preference, recorded in
`crates/pulpit/src/doc/mod.rs`: standalone crates were consolidated into
modules once the boundary stopped earning its cost. A ~600-line AcroForm
module does not earn a crate.

`pulpit-testkit` is a crate because several test targets depend on it and it
must not ship in the binary.

### 5.2 What the fold deletes

Because there is now one project and one consumer, the following cease to
exist as problems rather than being solved:

- the crates.io publication boundary and its release round-trip;
- a shared-MSRV negotiation — one workspace declares `rust-version` once, as
  it already does;
- defensive feature gating to keep Iced, media or Typst out of a second
  consumer's build;
- a public-API compatibility policy for two independent consumers: rustdoc on
  every public type, non-exhaustive enums for forward extension, protocol
  version negotiation between differently-versioned builds, changelog entries
  for wire migrations;
- a dual-implementation migration window in which two annotation systems
  coexist behind feature flags.

Internal discipline still applies. The worker protocol remains versioned and
length-bounded (§9) because supervisor and worker are separate processes that
can disagree after an upgrade — not because two projects consume it.

### 5.3 `pulpit-core`

Owns UI-toolkit-independent annotation interaction:

- `AnnotationTool` and style choices;
- stroke sampling, simplification and validation;
- text-selection gesture state, including word and line expansion;
- gesture state and cancellation;
- hit-testing against canonical annotation summaries;
- whole-object eraser selection;
- command construction and command grouping rules for undo;
- serializable recovery descriptions containing no PDFium handles.

It MUST NOT depend on Iced, PDFium, Typst or SVG.

The existing `Annotations` type is split. Pointer and gesture state is not
stored alongside committed document annotations:

```rust
pub struct AnnotationInteraction {
    tool: Option<AnnotationTool>,
    gesture: Option<Gesture>,
    pointer: Option<NormalisedPoint>,
    spotlight: Option<NormalisedPoint>,
}

pub enum AnnotationDraft {
    Ink(InkDraft),
    Highlight(HighlightDraft),
    FreeText(FreeTextDraft),
    Note(NoteDraft),
    Stamp(StampDraft),
}

pub enum AnnotationCommand {
    Create(AnnotationDraft),
    Replace { id: AnnotationId, replacement: AnnotationDraft },
    Delete { id: AnnotationId },
}
```

### 5.4 `pulpit-render`

Owns:

- dynamic PDFium discovery and binding;
- document and page lifetime management;
- page metadata and canonical transforms;
- page text extraction and selection-to-quadrilateral resolution;
- annotation enumeration and classification;
- native annotation creation, replacement and deletion;
- AcroForm field discovery, value read and widget geometry;
- the interactive form-fill environment: initialization, forwarded input
  events, invalidation callbacks and `FPDF_FFLDraw` compositing (§8.6);
- appearance stream generation;
- rendering into caller-provided RGBA storage;
- document revisions;
- save and reopen validation;
- the worker protocol and shared-memory bitmap transport;
- preservation of unsupported annotations.

Presentation scheduling — render generations, slide priority, audience cache
policy, coarse and refined queues — stays out of the document engine's own API
surface. Opening, annotating, filling, rendering or saving a PDF MUST NOT
require constructing a `RenderGeneration`. This is now an internal-module
concern rather than a published-crate constraint, and is enforced by the
document API's own tests rather than by feature flags.

## 6. Document API

The API expresses a long-lived document, annotation summaries, form fields,
mutations, renders and saves without exposing raw PDFium pointers.

```rust
pub struct PdfEngine { /* backend bindings */ }
pub struct PdfDocument { /* worker-confined document */ }

impl PdfEngine {
    pub fn bind(options: BindOptions) -> Result<Self, PdfError>;
    pub fn open(
        &self,
        source: DocumentSource,
        password: Option<SecretString>,
    ) -> Result<PdfDocument, PdfError>;
}

impl PdfDocument {
    pub fn info(&self) -> &DocumentInfo;
    pub fn annotations(&self, page: PageIndex)
        -> Result<Vec<AnnotationSummary>, PdfError>;
    pub fn select_text(&self, page: PageIndex, selection: TextSelection)
        -> Result<TextSelectionResult, PdfError>;
    pub fn fields(&self) -> Result<Vec<FormField>, PdfError>;
    pub fn apply(
        &mut self,
        expected: DocumentRevision,
        transaction: DocumentTransaction,
    ) -> Result<Applied, PdfError>;
    pub fn render_into(&mut self, request: RenderRequest, rgba: &mut [u8])
        -> Result<RenderedPage, PdfError>;
    pub fn save_as(&mut self, destination: &Path, options: SaveOptions)
        -> Result<SavedDocument, PdfError>;
}
```

Mutation flows through one pair of types, in-process and over the wire alike —
§9.5's `Apply` is this `apply` verbatim, and the optimistic `expected` revision
is checked identically in both:

```rust
pub enum DocumentCommand {
    Annotation(AnnotationCommand),
    SetField { name: String, value: String },
}

/// One atomic user action: a single command for an ordinary edit, several for
/// an eraser sweep or compound replacement. Bounded by the protocol's maximum
/// operations per transaction. One transaction is one revision increment and
/// one undo entry (§9.1).
pub struct DocumentTransaction(pub Vec<DocumentCommand>);
```

`PdfDocument` MAY be `Send` when moved as an owned value but MUST NOT claim
`Sync` unless the backend proves concurrent access safe. The supported default
is one document owned by one worker execution context. Parallel rendering MAY
open independent read-only instances; mutations are serialized.

### 6.1 Annotation summaries

```rust
pub struct AnnotationSummary {
    pub id: AnnotationId,
    pub page: PageIndex,
    pub kind: AnnotationKind,
    pub bounds: PageRect,
    pub style: AnnotationStyle,
    pub contents: AnnotationContents,
    pub editable: bool,
    pub revision: DocumentRevision,
}
```

Summaries carry enough geometry for hit-testing and inspectors but no raw
object references. Large ink arrays and quad lists MAY be fetched lazily by ID
if eager enumeration would exceed protocol bounds.

### 6.2 Mutation result

```rust
pub struct Applied {
    pub effects: Vec<AppliedEffect>,    // one per command, in order:
                                        // annotation summary, or field value
    pub document_revision: DocumentRevision,
    pub dirty_region: Option<PageRect>, // covering the whole transaction
    pub undo: DocumentUndo,
}
```

`DocumentUndo` is an opaque, serializable engine operation or a lossless
before-image sufficient to reverse the mutation. It MUST preserve unrecognized
dictionary data when undoing an edit to an imported annotation.

Applying a `DocumentUndo` — the protocol's `Undo` request — is itself a
mutation: it increments the revision and returns an `Applied` whose `undo`
field is the operation that redoes it. Redo therefore needs no request of its
own, and an undo/redo cycle preserves A3 identities because the operation
restores the annotation rather than recreating it.

### 6.3 Text selection

```rust
pub enum TextSelection {
    Range { anchor: PagePoint, head: PagePoint },
    Word { at: PagePoint },
    Line { at: PagePoint },
}

pub struct TextSelectionResult {
    pub quads: Vec<PageQuad>,
    pub text: String,
    pub truncated: bool,
}
```

Selection input is canonical page geometry; the engine resolves it against the
page's extracted text and returns quadrilaterals in reading order plus the
selected text, both bounded by protocol limits. A page with no extractable
text returns an empty result rather than an error, and the UI reports that the
highlighter is unavailable there. Selection is a read-only query and MUST NOT
increment `DocumentRevision`.

### 6.4 Form fields

```rust
pub struct FormField {
    pub name: String,
    pub kind: FieldKind,
    pub value: String,
    pub read_only: bool,
    pub options: Vec<String>,
    pub allows_custom_value: bool,
    pub multiple_selection: bool,
    /// Where the field is drawn. Empty when neither the producer nor the
    /// reader of the document could say — the inspector is still a way in.
    pub widgets: Vec<FieldWidget>,
}

pub struct FieldWidget {
    pub page: PageIndex,
    pub bounds: PageRect,
    /// The value this widget stands for when a field's widgets mean different
    /// things — a radio group's options. `None` when pressing the widget means
    /// the field rather than one of its values.
    pub option: Option<String>,
}
```

This is pdfform's `FormValue`/`WidgetRect` with `NormalizedRect` replaced by
canonical `PageRect` per A4. Since editing happens in place through the
form-fill environment (§8.6), this model exists for listing, navigation and
verification — Save As value checks in particular — not for hosting an
editor. `anchor_on(page)` names a field's first widget on a page.

## 7. Native PDF mappings

### 7.1 Ink

One completed freehand gesture creates one `/Ink` annotation. Its `/InkList`
contains one path unless a single logical annotation intentionally groups
several. The annotation has `/Type /Annot`, `/Subtype /Ink`, a `/Rect`
enclosing the stroke and its painted width, `/InkList` in PDF user space, `/C`
with sRGB-derived components, `/CA` where appropriate, `/BS` or equivalent
border width, `/NM` containing the `AnnotationId`, `/M` when available, and a
generated `/AP /N`.

The appearance is authoritative for consistent viewing, but standard geometry
entries remain populated so other viewers can recognize and edit ordinary ink.

Degenerate one-point strokes are represented by a visible dot appearance and
the most interoperable valid ink geometry found by compatibility tests. Empty,
non-finite or fully out-of-bounds strokes are rejected before mutation.

### 7.2 Text highlighter

The highlighter is a text-markup tool. It selects extracted text and produces a
true `/Highlight`:

- `/Type /Annot`;
- `/Subtype /Highlight`;
- `/QuadPoints` listing one quadrilateral per contiguous run of selected text,
  in PDF user space, in the standard entry order;
- `/Rect` enclosing every quadrilateral;
- `/C` with sRGB-derived components and `/CA` for opacity;
- `/NM` containing the `AnnotationId`, `/M` when available;
- `/Contents` carrying the selected text as an accessibility and search
  fallback, bounded by the text-length limit;
- a generated `/AP /N`.

`/QuadPoints` is normative geometry, not a derived hint: a viewer that reflows
or re-extracts text MUST be able to recover the marked region without the
appearance. Quads are emitted per text run rather than per glyph, so a
selection spanning lines or columns yields several quads in reading order. A
selection producing zero valid quads is rejected before mutation.

The blend mode used in `/AP` MUST be tested in Acrobat, PDFium, MuPDF and
Poppler. If multiply blending is not consistently honored, the appearance uses
the simplest interoperable transparent fill and records the limitation.

A transparent freehand stroke remains available through the ink tool as `/Ink`
(§7.1), and presentation-mode highlighting remains transient. Neither is
written as `/Highlight`, whose `/QuadPoints` semantics describe text-aligned
regions.

### 7.3 Plain free text

`/FreeText` with `/Contents`, `/DA`, `/Q`, `/Rect`, `/NM` and a normal
appearance. Fonts required by the appearance are embedded or selected under a
documented portable-font policy. Save validation MUST NOT rely on a viewer
regenerating the appearance.

### 7.4 Typst text

Typst markup has no lossless standard `/FreeText` encoding. A Typst annotation
is nevertheless a true annotation object with:

- a standard subtype that displays an `/AP` normal appearance;
- plain fallback `/Contents` where a meaningful fallback exists;
- generated vector or bounded raster appearance content;
- the original Typst source and schema version in a namespaced pulpit metadata
  entry;
- ordinary `/Rect`, `/NM`, flags and modification metadata.

Other viewers display the appearance; they are not required to edit the source.
pulpit recognizes the namespaced metadata and reopens the source for editing.
If the metadata is absent, the annotation is an ordinary imported annotation of
its standard subtype.

SVG MAY remain an internal preview or compilation intermediate. SVG bytes are
not a PDF annotation representation.

### 7.5 Sticky notes

`/Text`, preserving `/Contents`, open/closed state where supported, color,
author metadata if explicitly configured, `/NM` and unrecognized imported
entries. Opening a note inspector MUST NOT mutate the PDF.

### 7.6 Stamps, checks and visible signatures

Check marks, crosses and image-based visible signatures use `/Stamp` with a
normal appearance. Drawn signatures MAY use `/Ink`. UI and metadata call these
"marks" or "visible signatures", never cryptographic signatures.

External images are decoded with bounded dimensions and encoded into a PDF
image XObject without retaining an unsafe source path in the annotation.

## 8. Interaction

### 8.1 Ink

- Pointer-down starts a bounded `InkDraft`.
- Movement samples canonical points using the shared sampling algorithm.
- The unfinished path is drawn directly in the current window and, in
  presentation, sent to the audience path.
- Pointer-up simplifies and validates, then sends exactly one create command.
- Escape or cancellation discards the draft with no PDF mutation.
- A successful create produces exactly one undo entry.
- A failed create leaves the document unchanged and reports a recoverable
  error; the preview is removed or offered for retry, never silently accepted.

### 8.2 Highlighter

- Pointer-down anchors a text selection; drag extends it; double- and
  triple-click select word and line.
- The UI draws the live selection from the quads returned by `select_text`,
  which it MAY re-query as the drag moves. The selection is transient and
  causes no mutation.
- Pointer-up sends exactly one create command carrying the resolved quads, the
  selected text and the current style.
- A selection resolving to zero quads, or a page with no extractable text,
  commits nothing and reports why.
- A successful create produces exactly one undo entry, as for ink.
- Editing an existing highlight MAY change color and opacity. Changing the
  marked region is a replace command carrying a newly resolved quad set; the
  highlighter does not offer free geometric resizing, because `/QuadPoints`
  must continue to describe real text runs.

### 8.3 Eraser

The eraser deletes the topmost editable annotation intersected by the gesture.
One sweep MAY delete several annotations but is grouped into one undo
transaction. Unsupported annotations and form widgets are not deleted.

Partial stroke erasure is deferred. If added, it is represented atomically as
replacement of one `/Ink` by zero or more `/Ink` annotations, and remains one
undo step.

### 8.4 Selection, move and resize

Selection is application state, and is a set: the reader may hold one mark or
several. A completed move or resize issues one replace command and creates one
undo entry. The existing annotation is unchanged during
a drag; the UI MAY show a transformed preview. Cancellation restores the
unmodified rendered annotation with no worker mutation.

Picking a mark up is not a tool. With no tool armed — the hand — a press
selects the topmost mark under the pointer and begins a move; a press on a
corner handle of the current selection begins a resize instead; a press on
bare page clears the selection and pans. A mark that overlaps a link or a form
field therefore takes the press, and a press that selects a mark the document
only preserves (§8.2) selects it without moving it. Double-clicking a mark
opens what it says for rewriting (§8.5) rather than being a second way to
select it, so the three verbs — move or resize, rewrite, delete — are reached
by three gestures and no armed tool. The application MUST NOT require a mode
to be entered before an existing mark can be edited.

Several marks at once is what the selection tool is for, and its only job. It
drags a rubber band, and on release holds every editable annotation the band
*encloses* — not those it merely clips, which would make the selection
unaimable, and not those the document only preserves, which can be neither
moved nor deleted. A band that gathered nothing puts down what was held, as a
press on bare page does. Deleting a held set is one transaction and one undo
entry however many marks it takes (§9.1); a preserved mark inside the set is
passed over rather than refusing the press. Resize grips are offered only when
exactly one mark is held.

Text-markup annotations are excluded from free move and resize; see §8.2.

### 8.5 Text

Text composition uses a native text editor so IME, clipboard, selection and
dead keys behave correctly. Committing new text creates one annotation;
committing an edit replaces one. Escape cancels without mutation. Focus loss
follows one documented commit-or-cancel policy.

An edit is opened by double-clicking a mark that carries text — a free text
box, a note, or a stamp typeset from markup, which reopens its source rather
than its picture (§7.4). Double-click is honoured whatever tool is armed:
opening what a mark says is what double-clicking text means everywhere else,
not a mode. A mark with no text of its own is not opened by it.

### 8.6 Form filling

Form filling is driven by PDFium's interactive form-fill environment
(`fpdf_formfill.h`), not by application-drawn field editors. The application
never renders a field's value or editing state itself; PDFium performs
hit-testing, focus, the caret, text editing under the field's own `/DA`
(comb, auto-size, quadding, multiline), checkbox and radio toggling, and
choice widgets, drawing all of it into the page bitmap. This removes the
appearance-imitation problem entirely: the code that edits a field is the
code that generates its appearance.

- In form mode, raw input events over a page are forwarded to the engine
  (`FORM_OnLButtonDown/Up`, `FORM_OnMouseMove`, `FORM_OnChar`,
  `FORM_OnKeyDown`, `FORM_OnFocus`); the engine responds with invalidated
  page rectangles, which are re-composited via `FPDF_FFLDraw`. Annotation
  tools and form interaction are distinct input regimes selected by the
  active tool; one pointer event is never interpreted by both.
- The form-fill environment is initialized for every opened document even
  when no events are forwarded: `FPDF_RenderPageBitmap` alone does not draw
  live form field contents, so every page render composites an
  `FPDF_FFLDraw` pass.
- A committed field value change (focus loss, selection change, toggle) is
  observed through the environment's change notifications and recorded as
  one revision and one undo entry. Field editing and annotation editing
  share one undo history in user action order: a field edit followed by an
  ink stroke undoes the stroke first. In-field editing state before commit
  (caret position, uncommitted text) is PDFium's and is not in the undo
  history.
- A read-only field is displayed and not editable; the engine enforces this.
- The one exception to "the application never renders a field's editing
  state" is a **non-editable** combo box's or list box's *open list*, which
  pulpit draws itself. An open dropdown is transient viewer chrome: no saved
  file contains one, so nothing about it is an appearance to imitate. A press
  on such a widget is answered by focusing it — `FORM_SetFocusedAnnot`, not
  `FORM_OnLButtonDown` — so the engine opens no list of its own, and the
  option the drawn list chooses crosses back as `SelectOption`
  (`FORM_SetIndexSelected`). The engine still performs the selection,
  generates the appearance and runs the field's scripts, so there is still one
  implementation of the committed value and it is PDFium's. An **editable**
  combo box is excluded and keeps the engine's own list: it has a caret PDFium
  is drawing, and a second editing surface over that is what this section
  forbids. Which path a field takes is read from `/Ff` bit 19, not decided by
  the application.
- A **multi-select** list box (`/Ff` bit 22) is drawn by the same overlay with
  a tick per row rather than one chosen row. Clicking a row, or pressing Space
  on the highlighted one, toggles that row and **leaves the list open**;
  Enter closes it and Escape closes it, and both mean the same thing, because
  there is nothing held back for Enter to commit. The event is the same
  `SelectOption` the single-select path sends, because `FORM_SetIndexSelected`
  is already per-index: on a single-select field the engine clears the other
  options, and on a multi-select one it leaves them alone. Which of the two
  happens is decided inside PDFium by the field's own flag, not by the
  application.
  - The engine drops and immediately restores the widget's focus after each
    such selection, because PDFium holds a multi-select list box's pending
    selection in the form-fill widget and writes it to `/V` and `/I` only when
    the field loses focus. An overlay the application draws can read only the
    committed field, so without this every tick would come back unticked and
    the rows would show the selection as it was one press ago.
  - **Consequence:** each tick is one commit, one `DocumentRevision` and one
    undo entry, rather than one entry per visit to the field. This is
    accepted: each tick is a change a person made deliberately and may want
    back on its own, and the alternative — an overlay whose ticks lag the
    document — is worse. `CommittedField` carries `selected` and
    `previous_selected`, so each of those entries has a faithful before-image;
    one string could not name three choices.
  - An **editable** combo box is still excluded, multi-select or not, for the
    reason above: its list stays PDFium's.
- **Known limitation:** in-progress IME composition is not displayed inside
  the field; committed characters are forwarded. This is documented as a
  compatibility limitation (§3.4), not silently degraded.
- Form widgets are PDF annotations, but they are not exposed through the
  annotation editor, are classified separately, and cannot be removed with
  the eraser or the annotation delete command.
- There is no field inspector widget. Values are edited in place on the page
  only, so there is exactly one editing surface and no value-mismatch class
  of bugs.
- Undoing a field edit is the one thing that puts a value back without a
  person typing it, and it MUST go through the same editor, by the mechanism
  the field's kind takes: a text value is typed (focus the widget, select its
  contents, replace the selection), a checkbox or radio option is pressed,
  and a choice field's options are selected by index — text replacement edits
  a button not at all, silently. A multi-select list box's before-image is
  its selection indices, because one string cannot name three choices; the
  undo record carries them. It MUST NOT write `/V` or generate an appearance
  itself, so the comb spacing, auto-sizing, quadding and format script are
  still PDFium's, computed once. A state no press can produce — clearing a
  chosen radio group — is refused rather than faked.
- A forward `SetField` has exactly one other caller: the date and time
  helpers (§8.6), which commit a picked value through the same path for the
  same reason — pulpit chooses the *text* the pattern asks for, and PDFium's
  editor still writes it, runs the field's format script and produces the
  appearance. Undo of a picked value is then the ordinary inverse, so a
  picked date and a typed one leave the same kind of entry in the same
  history. Everything else about a field's value comes from a person typing
  into the engine's own editor; the application never writes `/V` itself.
  - **Consequence:** committing through `SetField` ends in
    `FORM_ForceToKillFocus`, so the field is left *unfocused*. The picker is
    closed before the commit and the caret does not stay in the field — the
    next form event re-reports focus from scratch, and a helper that assumed
    it still had the caret would be reading a focus that is gone.

The engine remains in the supervised worker process. Interactive events
travel over the existing IPC; invalidations return as dirty rectangles
(§9.4). Per-keystroke round-trips over a local pipe are well under a
millisecond and MUST NOT be optimized by moving PDFium in-process: form
filling exercises PDFium's most complex code paths on hostile input, and a
worker crash mid-fill must lose at most uncommitted in-field state, never
the document or committed values (§11.5).

### 8.7 Navigation

Committed annotations do not disappear on page or slide navigation. The
existing clear-on-slide-change behavior applies only to pointer, spotlight,
unfinished gestures and an optional explicitly temporary presentation mode.
That mode MUST NOT masquerade as document annotation editing.

Document mode navigation is continuous scroll with free zoom, plus fit-width
and fit-page. This is a different viewport model from the presenter's
fit-to-cell and is the one substantial piece of new view code (§14).

## 9. Rendering, revisions and the worker

### 9.1 Revision model

```rust
pub struct DocumentRevision(pub u64);
```

- Opening a document starts at revision zero.
- Every successful mutation increments the revision. A transaction increments
  it once, however many commands it contains, and undo and redo increment it
  like any other mutation.
- Failed and cancelled commands do not.
- Render requests MAY require a minimum revision.
- Render results state the exact revision rendered.
- Save results state the revision installed in the output.

Revisions are session-local monotonic counters, not PDF metadata and not
persistent version identifiers.

### 9.2 Preview handoff

After committing, the UI MAY retain its final gesture preview solely to avoid a
blank interval. It removes the preview when a page frame at or beyond the
mutation revision arrives. It MUST NOT paint the preview over a frame already
containing the same annotation.

If rendering fails, the annotation remains committed in the worker document,
the UI reports the render failure, and retry behavior is explicit. The UI MUST
NOT create a second durable copy as fallback.

### 9.3 Audience behavior

An unfinished stroke is broadcast as bounded transient gesture data so the
audience sees drawing with interactive latency. Completion broadcasts the
resulting annotation ID and expected revision. Audience overlays are removed
only when the audience frame contains that revision.

Pointer and spotlight continue through the transient presentation channel and
never affect the document revision.

### 9.4 Dirty regions

The API reports a dirty page rectangle, but full-page rendering is the required
correct baseline. Partial annotation compositing is an optional optimization
and MUST NOT introduce a second renderer with different appearance semantics.

### 9.5 Protocol

Versioned and length-bounded, because supervisor and worker are separate
processes:

```rust
pub enum DocumentRequest {
    Open(OpenDocument),
    ListAnnotations { page: PageIndex },
    GetAnnotation { id: AnnotationId },
    SelectText { page: PageIndex, selection: TextSelection },
    ListFields,
    /// Raw input forwarded to the form-fill environment in form mode (§8.6):
    /// pointer events in page space, committed characters and keys, focus.
    /// The worker replies with invalidated page rectangles and any observed
    /// committed field change (which carries the new revision).
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
    Render(RenderRequest),
    SaveAs(SaveRequest),
    Close,
}
```

Mutation uses optimistic revision checking. A request whose expected revision
does not match fails with `RevisionConflict` and performs no mutation,
preventing a delayed UI message from overwriting a later change.

Transactions are atomic at the document-model level: if one operation in an
eraser sweep or compound replacement fails validation, none is applied.

Redo is the `Undo` request carrying the inverse operation returned by the
undo's own `Applied` (§6.2); the protocol needs no `Redo` variant.

Limits are declared centrally and validated on both sides:

- maximum annotations per page and per document;
- maximum points per ink annotation and per transaction;
- maximum quadrilaterals per text-markup annotation and per selection query;
- maximum text and metadata byte lengths;
- maximum appearance stream bytes;
- maximum message and shared-memory sizes;
- maximum operations per transaction;
- maximum form fields and value length.

## 10. Import and preservation

### 10.1 Classification

```rust
pub enum AnnotationSupport {
    Editable,
    ReadOnlySupported,
    Unsupported,
    Malformed,
}
```

- `Editable` — round-trips through the model without known loss.
- `ReadOnlySupported` — understood and selectable, but editing would lose data.
- `Unsupported` — preserved and rendered, with bounded summary metadata only.
- `Malformed` — ignored or rendered only as PDFium safely permits, with a
  diagnostic.

### 10.2 Lossless editing gate

An imported annotation is editable only if the engine can preserve all
semantically relevant fields, or can prove that replacing them is the user's
requested operation. A valid appearance alone does not prove editability.

Unknown keys are retained when an editable annotation is updated unless they
conflict with fields the engine must regenerate. Conflicts are documented per
subtype.

### 10.3 Appearance behavior

Existing appearances are retained until the annotation is edited. Editing a
supported annotation regenerates its appearance deterministically. The engine
MUST NOT depend on a third-party viewer to synthesize a missing appearance for
pulpit-created output.

## 11. Session, save and recovery

### 11.1 One session concept

pulpit already writes a crash snapshot beside its settings while running,
deletes it on clean quit, verifies a file fingerprint at startup, and offers
an inert `RestorePlan` that only an explicit answer applies. Document mode uses
that same machinery rather than a second one:

```rust
pub struct SessionSnapshot {
    pub source: PathBuf,
    pub fingerprint: String,
    pub payload: SessionPayload,
}

pub enum SessionPayload {
    Presentation { slide: SlideIndex, timer: …, blank: …, displays: … },
    Document { page: PageIndex, zoom: f32, journal: Vec<JournalEntry> },
}
```

A `JournalEntry` is one applied transaction or one undo/redo operation. The
journal records every revision-incrementing operation, in revision order —
undos and redos included — so replay reproduces the exact revision history,
and an edit the user undid stays undone after recovery.

The two payloads differ in durability policy, and this difference is normative:

- a **presentation** payload is a periodic snapshot, because losing a slide
  index costs a keystroke;
- a **document** payload appends each command durably at commit, because
  losing an edit is data loss.

Both keep the existing rules: nothing is restored without an explicit answer,
and the offer is honest — the fingerprint is checked and only the parts that
still apply are offered.

> **Open decision.** This unification is the specification's choice, not an
> established fact of the code. The alternative is to keep the presentation
> snapshot and a document journal as separate files with separate lifecycles.
> Unification is specified here because both already answer the same question
> — *what was the last run doing, and does it still apply to this file?* —
> and a single restore prompt is better product behavior than two. Settle this
> before writing the code, because recovery semantics follow from it.

### 11.2 In-memory mutation

Annotation and field commands update the worker's open document. They do not
write the source path. The document becomes dirty at the first successful
mutation, and dirty state is visible in the UI.

### 11.3 Save As

1. write to a temporary file beside the destination;
2. flush and close it;
3. reopen it with the engine;
4. enumerate created annotation IDs;
5. render every changed page;
6. verify expected form values;
7. optionally verify with MuPDF or Poppler in the test path;
8. atomically rename into the destination;
9. report the saved document revision.

The source path is rejected as a destination (A6). Output paths undergo the
existing safe-destination checks.

### 11.4 Recovery

Recovery applies journalled entries to a freshly opened source only after
fingerprint verification and explicit consent. Every recovered entry is
validated under current limits. If an annotation or field target no longer
resolves, recovery reports the conflict rather than applying the change to a
guessed target. The journal MUST NOT store PDFium pointers, object numbers as
sole identity, passwords or transient gestures.

### 11.5 Worker failure

After a worker crash, read-only open and render requests MAY be retried.
Mutations are not assumed committed without a response. The supervisor reopens
the last durable base and replays only confirmed entries from the bounded
journal, in revision order. Save publication is never automatically replayed
after an ambiguous response.

This mid-session replay restores the user's own live edits after a worker
crash and does not require the §11.4 restore prompt, which governs a fresh
application start; the two consent policies are different because the session
is not.

## 12. Security

- PDFium stays inside the worker process boundary.
- Raw annotation dictionaries and appearance streams are hostile input.
- Typst annotation compilation is closed-world: no file, package, environment
  or network access.
- URI, launch, media, submission and file-attachment actions are never executed
  as a consequence of selecting an annotation or focusing a field.
- A form's own JavaScript — field format, keystroke, validate and calculate
  scripts — MAY run, because those are how a form computes the values it
  displays. It runs against a host that answers no to everything leaving the
  process: no network, no filesystem, no navigation, no printing, no clock. A
  script's request for any of those is reported to the application as a
  `HostRequest` and performed only if the application, with a user present,
  chooses to. Document-level and open-action JavaScript is not run at all, and
  dynamic XFA remains disabled.
- Annotation `/Contents` is text, not executable markup, unless explicitly
  recognized as namespaced Typst source and passed to the closed-world
  compiler.
- Imported names and metadata are length-bounded and escaped in diagnostics.
- Appearance generation uses checked arithmetic for dimensions and buffer
  lengths.
- Encrypted documents honor PDF permissions and fail closed when mutation is
  forbidden or unsupported.

Limits are constants covered by boundary tests. Initial values are chosen from
corpus measurements before implementation and recorded beside the protocol,
rather than silently inherited from PDFium.

## 13. Testing

### 13.1 The AcroForm corpus

`pulpit-testkit` carries pdfform's corpus intact. Its premise is worth
restating because it is the reason the corpus survives the fold: the public
corpora (veraPDF, PDF.js, PDFium) exercise parsers and renderers, which this
project delegates to PDFium. What they do not cover is finding fields, filling
them and writing them back.

Every case MUST survive opening, filling what it offers and saving, leaving the
process alive, the source untouched, and either a readable PDF or a clean
error. Cases with a defensible correct answer assert it.

### 13.2 Unit tests

- canonical-to-PDF conversion for every rotation and non-zero crop;
- inverse conversion within documented tolerance;
- stroke sampling and simplification bounds;
- one-point, boundary and malformed strokes;
- text selection to quadrilaterals across line, column and rotation
  boundaries, including empty and text-free results;
- hit-testing and topmost eraser selection;
- revision conflict behavior;
- command grouping and undo/redo across form and annotation edits;
- ID generation, missing `/NM` and duplicate `/NM`;
- text and metadata escaping;
- transaction atomicity;
- protocol length and allocation limits;
- the document API compiles and runs without constructing presentation
  scheduling types (§5.4).

### 13.3 Round-trip tests

For each supported subtype: create, save, reopen in a fresh worker, enumerate
by `AnnotationId`, compare semantic geometry, style and contents, render and
compare within an approved image tolerance, edit, save, repeat.

Tests include page rotations 0/90/180/270, crop boxes, non-default media boxes,
large pages, transparency, Unicode, right-to-left text and malformed imported
annotations.

### 13.4 Cross-viewer tests

Generated output is checked with PDFium plus available MuPDF and Poppler tools.
A release-candidate manual matrix includes Acrobat Reader on one platform.

The checks establish that:

- annotation objects are present in `/Annots`;
- `/Highlight` quads land on the intended text in every tested viewer and are
  reported by each viewer's own text-markup tooling;
- marks are visible without appearance regeneration;
- opacity and bounds are reasonable;
- unrelated annotations and form values survive;
- save and reopen does not shift rotated-page geometry;
- filled field values are read back by other viewers;
- Typst annotations display elsewhere even when not editable.

### 13.5 Failure tests

Worker crash before a mutation response; after a confirmed mutation and before
render; during Save As. Stale expected revision. Full disk and permission
failure. Read-only and encrypted documents. Annotation-count and
appearance-size exhaustion. Corrupt `/Annots`, `/AP`, `/InkList`,
`/QuadPoints`, `/NM` and `/Contents`. Signed sources. Unsupported annotations
with unusual private keys.

### 13.6 Performance

Measured, not inferred:

- unfinished ink follows input without waiting for PDF rendering;
- pointer-up command processing does not block the UI thread;
- a committed annotation appears in a revised frame promptly;
- annotation and field enumeration is bounded on large documents;
- no bitmap or handle equality path compares full pixel buffers per frame;
- audience preview traffic stays bounded under high-frequency pointer input.

Budgets are recorded after a baseline run on the supported development machines
and then enforced as regression thresholds.

## 14. The fold

### 14.1 What moves from pdfform

| From | To |
|---|---|
| `pdfform-core` `FormValue`, `WidgetRect` | `pulpit-render` forms module, geometry converted to canonical `PageRect` |
| `pdfform-pdf` AcroForm discovery, choice metadata, value write-back | `pulpit-render` forms module |
| `pdfform-testkit` entire | `pulpit-testkit` |
| AcroForm, non-destruction and hostile-input tests | beside the code they cover |
| `SPEC.md` compatibility levels, export contract | §3.4 and §11.3 above |
| `SPEC-SIGNING.md` | `SPEC-signing.md` in this workspace |

### 14.2 What is deleted rather than ported

The `pdfform-app` shell entire — application state machine, view, chrome,
theme, tokens, icons, toasts, frames, residency, page surface and transform.
`pdfform-worker` entire. The `PdfEngine` bind/render/inspect scaffolding.
`NormalizedPoint`, `NormalizedRect` and `geometry.rs`. `Tool`, `EditKind`,
`EditItem` and `DocumentEdits` — the visual-item model, which native
annotations replace. `recovery.rs`, which §11.1 subsumes.

Roughly 11k of pdfform's ~14.3k lines of `src`. Where a deleted file was
itself a port of a pulpit file — `residency.rs` says so in its own header — the
original is the survivor, and any behavior the port improved is folded back
into it before the copy is removed.

### 14.3 Order

1. Split `AnnotationInteraction` from completed marks in `pulpit-core`.
2. Expose the document-oriented engine surface from `pulpit-render` with no
   presentation-scheduling types, and add canonical page geometry.
3. Implement `/Ink` creation, enumeration, rendering, deletion and Save As.
4. Route completed presenter ink gestures through the engine, retaining only
   the unfinished stroke as an overlay; add revision-based frame handoff.
5. Add the `DocumentPage`, `DocumentNav`, `DocumentOutline` and
   `AnnotationTools` widgets, the `Builder::page()` helper and the **Reader**
   built-in; implement continuous scroll and free zoom.
6. Spike the form-fill environment: raw `FORM_*` bindings through
   pdfium-render, event forwarding over the existing IPC, dirty-rect
   `FPDF_FFLDraw` compositing; measure type-to-glyph latency. This gate
   decides §8.6's viability before any form UI is built.
7. Port the AcroForm listing module and `pulpit-testkit`; wire in-place form
   filling per §8.6; get the corpus green.
8. Implement text extraction and selection, then `/Highlight`.
9. Implement free text, Typst text, notes and stamps.
10. Unify the session snapshot per §11.1 and implement document recovery.
11. Delete the old completed-overlay archive and annotation-export assembly
    once native output passes the cross-viewer tests.

Steps 3–4 land native ink for the existing presenter product before any
document-mode UI exists, so the riskiest change is exercised early against a
workflow that already has tests.

### 14.4 Existing presenter marks

Current session marks are documented as temporary and are not a durable public
format. No automatic migration of an active process's memory is required. Any
persisted archive data in a released format is imported once into native
annotations or explicitly retained as a read-only legacy import; it MUST NOT
remain a second writable system.

## 15. Milestones

**M1 — Native ink.** Import, create, delete `/Ink`. Stable IDs and coordinate
transforms. Appearance generation. Presenter live preview and revision handoff.
Save As and cross-viewer tests. Whole-object eraser and undo.

**M2 — Document mode.** `DocumentPage`, `DocumentNav`, `DocumentOutline`,
`AnnotationTools`. The **Reader** built-in and mode-aware default selection
(§2.1–§2.3). Continuous scroll and free zoom. Selection, move, resize. Grouped
eraser transactions. Dirty state.

**M3 — Forms.** Form-fill environment spike passed (§14.3 step 6). AcroForm
listing module, engine-driven in-place editing per §8.6, compatibility levels,
Save As value verification, `pulpit-testkit` corpus green.

**M4 — Highlighter and text.** Text extraction and selection resolution.
`/Highlight`. Plain `/FreeText`. Typst appearance and source metadata. `/Text`
notes. Check, cross and visible-signature stamps. Accessibility labels.

**M5 — Session and removal.** Unified session snapshot and document recovery.
Old overlay export paths removed. Unsupported-annotation preservation corpus
passes. pdfform's repository archived.

## 16. Acceptance criteria

The work is complete only when all of the following hold:

1. A completed ink gesture creates a native `/Ink` annotation in the worker's
   open PDF.
2. Saving and reopening preserves its ID, geometry, style and visible
   appearance.
3. The same annotation can be selected, edited, deleted, undone and redone in
   document mode and in presentation.
4. Highlighting selected text creates a `/Highlight` whose `/QuadPoints` mark
   that text in other viewers' own text-markup tooling, with the selected text
   recoverable from `/Contents`.
5. An AcroForm document can be filled, saved, reopened elsewhere, and shows the
   entered values.
6. Pointer, spotlight, text selection and unfinished gestures never enter the
   PDF.
7. No completed-annotation archive or SVG-overlay persistence system remains on
   the normal write path.
8. A completed gesture or field commit creates one undo entry and one document
   revision, and undo order follows user action order across both kinds.
9. Presenter and audience never double-paint a committed annotation during
   frame handoff.
10. Unsupported annotations and unrelated form values survive every edit.
11. Source PDFs are never overwritten.
12. Generated annotations display in PDFium, MuPDF, Poppler and the manual
    Acrobat matrix.
13. Worker crashes and stale messages cannot silently duplicate or lose a
    confirmed edit.
14. A document layout saved by an older version still loads.
15. **Reader** is a built-in, read-only, canonical and warning-free layout; a
    PDF opens into it without touching which presenter layout is selected, and
    the reverse.
16. The `pulpit-testkit` AcroForm corpus passes.
17. The document API is reachable without constructing presentation
    scheduling types.

## 17. Consequences

This design makes PDF mutation part of pulpit's document model. pulpit acquires
dirty state, Save As, signature warnings, recovery and mutation-failure
handling that temporary overlays avoided, and it acquires AcroForm semantics,
which are their own domain of hazards.

In exchange it stops maintaining a second copy of its own renderer, worker,
theme, toast and residency code in another repository; annotations interoperate
with ordinary PDF software; and the boundary machinery that a two-project split
demanded — a publication round-trip, an MSRV negotiation, defensive feature
gates and a public compatibility policy — is deleted rather than maintained.

The honest cost is scope. pulpit stops being a presenter that happens to render
PDFs and becomes a PDF application with a presenter mode. That is a product
decision, taken deliberately, and §2 is where it is either contained or not:
if document mode cannot be expressed as layouts and widgets over the existing
shell, the fold has failed and the split was right.
