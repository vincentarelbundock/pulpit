#set document(title: [Internals])
#metadata((tags: ("architecture", "platform", "invariants"))) <website-metadata>

#title()

= Architecture

```
Application (iced daemon, one update loop)
├── PresentationState        authoritative domain state         pulpit-core
├── DisplayCoordinator       snapshots, roles, reconcile()      pulpit-display
├── DocumentManager          watch, debounce, atomic reload     pulpit::doc
├── RendererSupervisor       worker pool, IPC, generations      pulpit-render
├── FrameCache               byte-bounded CPU/GPU accounting    pulpit-render
├── InputRouter              keymap incl. raw scancodes         pulpit::settings
├── SessionInhibitor         acquire/release, crash-safe        pulpit
└── Settings & Diagnostics   atomic, versioned, reportable      pulpit::settings
```

Four packages, not nine: `core`, `display` and `render` are separate because
they cross a process or tool boundary, isolate a large external dependency, or
have a test surface worth running alone. The rest were app-only libraries
whose Cargo boundary bought nothing, and are now modules with the same rule —
no Iced, no clocks, no services below the application layer.

== Why the domain crates are pure

`pulpit-core`, the decision half of `pulpit-display`, and `pulpit::doc`'s
state machine contain no UI types, no window handles, no PDF library types and
no clock reads (time is passed in). That is what makes the hard cases —
reconnect at a new index, an unequal mirror, a partial write, a stale delayed
notification — ordinary unit tests that run in CI without a graphical session.

== The three rules

*1. One reconciliation function.*
`reconcile(snapshot, roles, capabilities, windows) -> Outcome` is pure and
idempotent: applying its actions and calling it again produces nothing. Swap
is a role exchange followed by ordinary reconciliation, never a sequence of
ad-hoc window moves. Repeated notifications are free
(`Reconciliation::Unchanged`) and older snapshots are dropped
(`Reconciliation::Stale`).

*2. No native handle survives an event-loop turn.*
`DisplayRoles` stores versioned identity _records_. Reconciliation enumerates
afresh, resolves an identity to a live handle immediately before a native
call, and forgets it. A monitor disappearing between resolution and use
returns `PlacementOutcome::Disappeared`, which triggers another snapshot — not
an error path.

*3. The audience frame is never worse than it was.*

- The audience window is created only when Start is pressed, initially hidden,
  assigned, and shown only with a valid frame (`Warning::AwaitingFirstFrame`).
  Stop destroys it.
- Every live page is rendered at exactly one size per window, and the cache
  falls back across generations so a reload cannot blank the output. "Never
  worse" covers quality, not only absence: each output holds the last picture
  it settled on until an exact replacement for the page it wants is ready, so
  no window climbs a ladder of textures for one page turn. A panel with
  nothing yet shows the deck thumbnail, and gives it up once, to the first
  real frame.
- The one deliberate exception is the coarse stand-in, and it is asked for
  only on the jumps where it would be shown: a window holding some *other*
  page, with nothing at its own size for the page it wants. A correct page
  coarsely beats a sharp picture of somewhere else. One rule serves both
  windows and one render answers both, because a window that took a stand-in
  the plan had not asked for would wait for a frame nobody was rendering.
  Ordinary turns land on a prefetched frame, so they neither render it nor
  show it; a stand-in never appears over the page it is already showing, which
  would be the ladder rather than the cure; and the very first frame of a
  session is always the real one — no window is revealed with a soft picture
  that sharpens in front of the room.
- The presenter's current-slide panel changes in the same beat the room sees,
  stand-in included: it is the surface the operator is watching, and holding
  the previous page there until a full canonical render landed made it the
  last thing in the application to answer the key they pressed. Blanking is
  still not mirrored: the room's screen goes dark, the presenter's place in
  the deck does not. Every display change is logged at debug level.
- The render queue serves the committed audience page first, then the
  presenter panels' preview-size frames, and only then the audience-size
  prefetch of the neighbouring pages: prefetch is a background luxury, and one
  such frame costs roughly ten panel frames. Warming covers two pages behind
  the presenter as well as two ahead, so stepping backward is as seamless as
  stepping forward.
- A rebuilt PDF is promoted only after its first audience frame renders; a
  failed or partial build leaves the working document untouched and retries
  with bounded backoff.
- Renderer workers are separate processes. A crash fails one request, is
  reported, and is followed by a bounded restart.

== Message flow

Everything — key presses, timer ticks, file-watch hints, topology hints,
renderer replies — becomes an application message handled by one `update`
function. Subscriptions are stable across view rebuilds, so no watcher or
timer is ever duplicated.

The renderer is pumped from the tick handler and from a doorbell, both of
which run the same drain, so IPC results stay inside the same
single-threaded state transition as user input. The doorbell is a one-deep
channel a worker's reader thread rings after its message is on the queue: it
carries no payload, a burst of finished frames collapses into one pass of the
event loop, and a missed ring costs nothing because the next drain takes
everything waiting. It exists because a finished frame used to become visible
only when the tick next got round to looking, so every rendering step of a
page turn paid up to a tick for the poll alone. The tick is unchanged and
still drains on its own schedule: a silent worker — the deadline and restart
cases — is exactly what no doorbell reports.

== Cache accounting

Eviction is bounded by decoded bytes, never page count: a 3840×2160 RGBA frame
is 33,177,600 bytes. What is counted is the decoded bitmap, which is what the
cache holds — the textures made from it belong to a window's renderer, one copy
per window that draws it, and are neither sized nor timed from here. The
frames currently on screen — and the prefetched neighbours whose whole purpose
is to survive until the next page turn — are pinned and never evicted, a frame
larger than the whole budget is refused rather than allowed to evict
everything, and the statistics are visible in diagnostics. Pinning the
neighbours means the audience-size ones by name, not merely the panel-size
frames for the same pages: an audience frame is tens of megabytes against a
panel's few, which makes it both the most valuable thing in the cache and the
first thing an unpinned budget takes. Leaving them out is not a slow leak but
a treadmill — the frame bought to make the next turn seamless is evicted
before that turn, re-requested, and paid for again — and it presents as a
sluggish page turn rather than as anything about memory. A slide request is
satisfied only by a frame of exactly its own size — width *and* height, since a
`/FitR` zoom re-crops a page — so an audience-resolution frame never suppresses
the panel-size render the presenter windows depend on, and one page under one
crop is one picture rather than whichever of two the hash order offers. Notes,
which are nobody's atomic transition, still take a nearby fitting frame.
Deck thumbnails live on a separate budget and are rendered in one pass at one
width — chosen per document so the whole deck fits — and are then immutable
for the document's life, so no texture downstream ever swaps because of them.

= Standing invariants

These are enforced today by the shipped runtime and must not be weakened by
any later change. The key words *MUST*, *MUST NOT*, *SHOULD*, *SHOULD NOT* and
*MAY* are normative.

== The audience frame

+ *The audience window is output, not chrome.* It MUST contain only the
  presentation frame or an intentional black/white blanking frame — never
  notifications, controls, focus rings, diagnostics, dialogs, loading flashes,
  or runtime-selection UI. Blanking colours are exact output colours,
  independent of the application palette.
+ *A presenter-side animation, theme change, dialog, resize, or error MUST NOT
  invalidate the last valid audience frame.* After a session has produced a
  frame, its last complete frame is retained until a complete replacement
  arrives; partial buffers and worker errors never reach the audience. The
  `last_audience` cache key is pinned for this reason.
+ *A frame MUST be resident in the renderer that draws it before it is laid
  out.* Residency is per window, not per application: Iced gives each window
  its own image cache and atlas, and its explicit allocation task reaches only
  the lowest-numbered window. Any image of two mebibytes or more — every slide
  panel, every audience frame — is uploaded on a worker thread, is skipped by
  the frame it belongs to while that upload runs, and measures as nothing
  before it lands, so a widget drawn on another window's guarantee is laid out
  at zero size and paints a black rectangle for a page turn. Each window's
  view therefore holds allocations from its own renderer, for exactly the
  pictures it draws plus the page a turn away, taken one per pass so the
  blocking upload lands while that window is idle rather than during a turn.
  One per pass makes those uploads paced by how often the application draws,
  so a newly cached frame holds the fast tick briefly: settling the instant
  the last render landed left precisely the pre-uploads that make the next
  turn instant to trickle at the idle cadence, and a turn arriving first paid
  for one synchronously on the event loop. The application does not track
  which pictures a window has actually uploaded — that is the window's own
  widget state — so the hold is timed from a frame arriving and errs toward
  staying awake a tick too long rather than a tick too short.
+ *The PDF page remains the fallback.* Without a media runtime, every overlay
  shows its poster or the PDF page and the deck still presents.
+ *Failure is presenter-side.* No known platform-specific failure may blank,
  replace, or expose controls on the audience frame.

== Overlay runtime

+ *One session, two consumers.* Each overlay has exactly one authoritative
  session producing one ordered frame stream. The audience and presenter views
  consume the same frames and MUST NOT create separate decoders, browser tabs
  or JavaScript contexts.
+ *Capabilities, not operating-system branches.* A runtime is chosen by what
  it probes as able to do. A runtime lacking a required capability is skipped
  even when it claims the content type. A security-policy denial is never
  bypassed by trying a less restrictive runtime.
+ *Heavy runtimes stay out of process.* A worker crash MUST fail only its
  sessions. The main executable MUST NOT link a media engine, and packaging
  MUST verify this from the final executable's dynamic dependencies.
+ *One browser process per worker, one page per session.* A new session MUST
  take a page from the pool, never a process. Because the CDP pipe is shared,
  every message MUST be routed by the session id of the page it names — a
  session reading the pipe directly consumes another session's frames. A
  browser crash therefore fails every session in that worker together; each
  falls back independently.
+ *Frames are asked for at the overlay's size.* The screencast carries
  `maxWidth`/`maxHeight` so Chrome encodes the size Pulpit wants rather than
  the worker resampling. A viewport change MUST restart the screencast.
+ *A wrapper page draws no controls.* Anything the generated page paints is
  painted on the projector — a hover-revealed scrub bar is a scrub bar the
  room sees. Playback controls MUST live on the presenter's layout and reach
  the content through `MediaRequest::{Video, Image}`, which the worker
  translates into calls on the page's `window.__tp`. The page reports its
  playhead back through `__tpReport`. Host commands are built from a fixed
  vocabulary in the worker; there is deliberately no generic `eval`.

One runtime implements animated images, video and interactive HTML:
`external-chromium`, an installed Chromium-family browser driven over the
Chrome DevTools Protocol, never linked into Pulpit. The cost is recorded
honestly: a machine with no such browser installed shows the fallback for
_every_ media overlay, not just for HTML. That is accepted, not a gap.

== Platform boundary

+ *Ask what the session can do, never what OS it is.* Views and domain logic
  ask for capabilities — arbitrary placement, safe un-fullscreening, system
  appearance, sleep inhibition, native menus, accessibility bridge, media
  keys. An unavailable capability produces one of three deliberate outcomes: a
  documented safe fallback, a specific manual action for the user, or a
  disabled command with an explanation. Silent no-ops and optimistic success
  messages are forbidden. Failure to target a chosen display MUST NOT be
  reported as success merely because some form of fullscreen was entered.
+ *Four contracts, one snapshot.* `DisplayBackend`, `PlatformServices`,
  `WindowPolicy` and `InputPolicy`, plus an immutable `Capabilities` snapshot
  refreshed when the session or display server changes. Adapters return data
  and explicit outcomes; they MUST NOT mutate presentation state.
+ *Pure crates stay pure.* No UI, OS, PDF-library or clock types in the
  domain, layout, document, rendering or settings models. Platform-only crates
  appear under target-specific `Cargo.toml` sections; conditional compilation
  belongs in adapter modules, not in state transitions or views. `unsafe` and
  FFI are confined to the smallest adapter module, each block documenting its
  lifetime, thread and ownership invariants.
+ *Persisted values are portable.* Settings, cache and log locations use
  platform-standard directories; paths are `Path`/`PathBuf`, not assumed
  UTF-8. Nothing persisted may encode a monitor index, absolute window
  position, OS font name, physical DPI, or platform shortcut spelling when a
  semantic representation exists. Migrations are deterministic and tested from
  fixtures of every supported platform.
+ *Native shell, Pulpit interior.* Ordinary windows use OS decorations; Pulpit
  MUST NOT draw a custom title bar to look the same everywhere. Inside the
  frame it uses one deliberate visual language rather than imitating GTK,
  WinUI or AppKit.

== Visual and interaction system

+ *Seven colour roles, no other vocabulary.* `canvas`, `surface`,
  `slide_canvas`, `text`, `muted`, `accent`, `alert`. Borders, overlays,
  disabled and interaction states, and readable text on colour are derived
  centrally from those roles. Components MUST NOT invent aliases such as
  `primary`, `danger` or named hues. Spacing (4/8/12/16/24/32), radii and
  typography scales likewise live in the theme layer, not in views.
+ *Status is never conveyed by colour alone*, and contrast is at least 4.5:1
  for normal text, 3:1 for large text, control borders, focus rings and
  meaningful graphics. High-contrast system modes take precedence over the
  selected palette.
+ *Logical units everywhere but rasterisation.* Layout, spacing, hit targets
  and persisted window sizes are logical pixels; slide rasterisation uses the
  destination's physical size and scale factor. A scale change triggers
  relayout and a resized render without blanking the audience. No view may
  assume 96 DPI, integer scale, or equal scale between displays. Pointer
  targets are at least 32 logical pixels, live controls at least 40.
+ *Keyboard first, pointer fully.* Every presenter and layout-editor operation
  is reachable by keyboard; focus order follows reading order and focus stays
  visible. Hover MUST NOT be the only way to reveal information or an action.
  Drag interactions have keyboard alternatives.
+ *Semantic commands.* Actions have stable identifiers independent of
  shortcut, menu position, label or platform. Keyboard, menus, buttons,
  remotes and automation dispatch the same action model.
+ *Motion communicates state, never decorates.* It MUST NOT delay navigation,
  audience updates or blanking, SHOULD complete within 200 ms, and is reduced
  when the system asks for reduced motion.
+ *Errors say what failed, what is currently safe, and what to do next.* A
  transient toast is never the sole record of a presentation-critical failure,
  and never appears on the audience window.
+ *Layouts are device-independent.* Normalised split proportions, minimum
  sizes in logical units, no monitor index, desktop coordinate, OS font name
  or physical DPI. The designer canvas MUST use the same layout algorithm as
  the live presenter view — a separate approximation is forbidden.

== Performance

+ *Cost follows the active set, not history.* Off-page media sessions are
  parked (`Page.stopScreencast`, so they cost no browser encoding, no CDP
  transport and no worker decode) and evicted beyond a bounded parked count
  and parked ring-byte budget. The worker's poll loop pumps only sessions that
  are active or have queued events.
+ *Frames are delivered at the consumption rate, and only the newest.* Capture
  is capped (default 30 fps) by `everyNthFrame` plus an authoritative publish
  deadline; within one supervisor drain only each session's newest frame is
  copied. Superseded frames release their ring slot uncopied.
+ *No copy that ownership can avoid.* Exact-size frames go from decode
  straight to the ring; scratch buffers are reused rather than reallocated per
  frame; image handles share the frame cache's allocation
  (`Bytes::from_owner`) instead of deep-copying it; the annotation model is
  snapshotted behind an `Arc` with a revision counter and drawn through
  `canvas::Cache`, so unchanged strokes are neither cloned nor re-tessellated.
+ *A picture is compared by identity, never by its pixels.* Iced's image
  handle derives equality, and its pixel buffer compares by content, so a
  single `==` between two audience frames memcmps thirty megabytes. Anything
  that asks "is this the same picture?" on a draw pass — residency
  bookkeeping above all — MUST compare `Handle::id`. Getting this wrong does
  not look like a slow projector; it looks like a slow application.
+ *A window keeps resident only what it draws.* A texture atlas grows to fit
  whatever is held in it and *copies every existing layer* each time it grows,
  so holding the frame cache resident — a quarter of a gigabyte of pictures,
  for four panels and one projector — paid a full-atlas texture copy every
  time the budget refilled. Rendering is not the expensive part of a page
  turn: a 4K page is single-digit milliseconds through PDFium, where an
  unbounded atlas is tens.
+ *Budgets must be honest.* A byte budget names what it actually counts
  ("source bytes"). Pinned overcommit is reported rather than hidden, and no
  permanently-zero figure is displayed — which is why texture bytes are not a
  cache statistic at all: they remain unavailable through Iced, and a field
  that could only ever be an estimate or a zero is worse than its absence.
+ *Measure before restructuring.* Two recorded negative results: replacing the
  CDP pipe's 1 ms retry sleep with `poll(2)` cost 2-3x the worker CPU for one
  to two milliseconds of latency (the sleep stays, with a comment saying why);
  and the 50 ms application tick MUST NOT be replaced until a wrapped GUI
  build has actually been profiled. Static-analysis findings are hypotheses
  until measurement attaches numbers to them, and debug builds never set
  targets. The renderer doorbell is not that replacement and MUST NOT be read
  as licence for it: the tick still runs, still drains, and still owns the
  deadline and restart checks — the doorbell only removes the poll's latency
  from the path a page turn takes.
+ *Measure the build you ship.* A page turn that felt slow was chased
  through six rounds of instrumentation, and the four largest causes were
  found in this order, none of them by reading the code: a debug binary
  (PDFium ships optimised, so rasterising measures the same and everything
  around it does not); a `workers = 2` left in a settings file from before
  `default_workers` existed, so an improved default never reached the machine
  that needed it; a worker pipeline sixteen jobs deep, which had been right
  when the supervisor was polled and became the bottleneck once the doorbell
  removed the poll; and panel-size renders of pages no layout draws. The
  report names the build for this reason, and a figure taken from a debug
  session sets no target. The end state on a 730-page deck is a settled turn
  of about a millisecond.
+ *A queue you cannot see is a queue you cannot steer.* Work handed to a
  worker has left the only queue the supervisor can reorder or cancel
  cheaply, so a worker is given a little work in hand and no more. When that
  depth was sixteen, a page turn's half-second was 6 ms rasterising, 4 ms in
  the supervisor's queue, and 507 ms sitting in a worker's inbox. Whenever a
  latency is being hunted, split the wait by *where* it is spent before
  changing anything: every aggregate in that investigation hid the thing it
  was built to find, and each split moved the answer by an order of
  magnitude.
+ *The render queue order is settled.* The committed audience page first, then
  the presenter panels, then the audience-size prefetch of the neighbours,
  which is a background luxury costing roughly ten panel frames apiece.
  Promoting the prefetch above the panels was tried and reverted — it delayed
  every panel update by hundreds of milliseconds and read as a late pop on
  each navigation. Do not reorder these tiers without numbers from a release
  build; a page turn that feels slow is far likelier to be a starved cache or
  a polled event than a mis-sorted queue, and both of those have been the
  answer before.

== One representation of a mark

A completed annotation has exactly one authoritative representation: a native
annotation in the open PDF. This is invariant A1 of `SPEC-document.md`, and it
is the decision the whole of document mode rests on — so it is worth being
precise about what it cost and what it removed.

What it removed: the per-slide ink cache that held a presenter's marks in
process memory, and the export assembly that stamped them into a copy of the
deck as page content. Both are gone. A presenter's completed stroke is
committed to the document engine when the pen comes up, and what the overlay
draws afterwards is a *view* of the annotations the document holds for the page
the slide is showing. Saving the document saves the marks, because the marks
are the document's annotations; there is no separate "annotated copy".

Three consequences follow, and each is load-bearing:

+ *Presentation and document mode edit the same annotation.* A stroke drawn at
  the lectern can be selected, moved and deleted in document mode afterwards,
  and a highlight made in document mode appears on the slide. There is one
  undo history — the document's — so undo order follows user action order
  across both.
+ *The unfinished gesture is still the overlay's alone.* A stroke under the
  pen, the pointer, the spotlight and a label being typed never reach the file
  (A2). Latency is unchanged: the pen follows the hand with no worker in the
  loop, and the round trip happens once, on release.
+ *A document that cannot be annotated cannot keep marks.* This is the honest
  cost. Where marks used to persist in memory regardless, they now depend on
  document mode having opened the file. The presenter is told once, when the
  first mark is made, rather than discovering it after the talk.

The conversion between slide space (fractions of what the projector shows) and
canonical page space (PDF points from the crop box's top-left corner) lives in
`pulpit_core::annotate::presenter` and nowhere else, in both directions. A
split-page deck is why it is not a scale factor: a slide can be half a physical
page, and a mark two thirds of the way across the slide is one third of the way
across the paper. The round trip is property-tested across every rotation, crop
and region, and again through a real PDF in
`pulpit-render/tests/presenter_ink.rs` — a mark that moves between the talk and
the file is a bug nobody would find until afterwards.

== One render pipeline for slides and pages

The reader's pages are rendered by the same supervised worker pool, through
the same byte-budgeted frame cache and the same shared-memory transport as
the projector's slides — as `FrameKind::Page` entries whose jobs set
`with_annotations`, because on a reader page the document's own marks are the
point. The bespoke path this replaced — one serial document worker answering
render requests over a pipe, pixels inline, no cache, no cancellation — is
why paging used to lag behind stale renders and why a settling page could be
repainted by a late coarse frame.

What made the bespoke path look necessary is A7: a frame must contain the
annotation that was just committed, and only the process holding the mutated
document has it — the edits are not in the file on disk until the user saves.
The resolution is a *revision snapshot*: after a debounced burst of edits the
document worker writes an incremental `FPDF_SaveAsCopy` of its in-memory
document to a scratch path pulpit owns (A6 holds; the destination is never
the source), and the pool opens that snapshot as a document of its own under
a fresh render generation. Reader generations live in a namespace far above
the presentation's (`READER_RENDER_BASE`), are advanced once per snapshot,
and are never fed to `cancel_older_than` — reader jobs are cancelled by id,
by their own sweep, so neither plan can cancel the other's work.

Generation order is revision order, and that equivalence is what carries A7
into the cache: the lookup walks generations from the newest down and a
coarse frame can never outrank a refined one at the same generation, so a
page keeps its pre-edit picture until a complete frame containing the edit
exists, and a late or lesser frame can never repaint a better one. Before
the first edit no snapshot exists and none is taken: pages render from the
presentation's own document, already open in every pool worker.

What stayed in the document worker is everything that is not rendering: the
annotation transactions, the undo history, text selection, the outline — and
form filling entirely, because a focused field's uncommitted state lives in
PDFium's form-fill environment, keyed to the live `FPDF_PAGE`, and exists in
no saved copy.

== The fold: what came from pdfform

pdfform was a second Rust application for filling PDF forms, and it carried a
second copy of this project's renderer, worker, theme, toast and residency
code. `SPEC-document.md` §14 folds it in. What that specification lists as
moving has moved, and can be checked:

#table(
  columns: (auto, auto),
  table.header([*From pdfform*], [*Now*]),
  [`FormValue`, `WidgetRect`], [`FormField`, `FieldWidget`, in canonical `PageRect`],
  [AcroForm discovery, choice metadata, write-back], [the `document` module's form path],
  [`edit/fields.rs`], [not ported: no field panel],
  [`pdfform-testkit` entire], [`pulpit-testkit`],
  [AcroForm, non-destruction, hostile-input tests], [beside the code they cover],
  [`SPEC.md` compatibility levels], [`SPEC-document.md` §3.4],
  [`SPEC-SIGNING.md`], [`SPEC-signing.md`],
)

What was *not* ported is the larger half: the whole application shell, its
worker, its `PdfEngine` scaffolding, its normalised-coordinate geometry, and
its `Tool`/`EditKind`/`EditItem` visual-item model — roughly 11k of pdfform's
~18k lines. Native annotations replace the visual-item model outright, which
is the same decision as A1 seen from the other end: an edit is not an item in
an application's list that gets written out later, it is an annotation in the
document from the moment it is completed.

No pulpit crate depends on pdfform, and nothing in this workspace reads from
it. `SPEC-document.md` supersedes `pdfform/SPEC-SHARED-ANNOTATIONS.md`, which
was the negotiated boundary between the two projects and is the machinery the
fold deletes rather than maintains.

== Form filling: why the editor is PDFium's

A PDF form field is not a text box. It is a text box plus a pile of rules about
how a value is drawn into it: the font and size in its `/DA`, comb spacing that
puts one character per cell, auto-sizing that shrinks type to fit, quadding
that right-aligns it, multiline wrapping, and for a checkbox a glyph out of
`/ZapfDingbats`. Every one of those rules is already implemented — in PDFium,
in the code that generates the field's appearance stream.

So pulpit does not implement them again. Raw input events over a page in form
mode are forwarded to PDFium's interactive form-fill environment
(`FORM_OnLButtonDown`, `FORM_OnChar`, `FORM_OnKeyDown` and the rest), which
does the hit-testing, the focus, the caret and the editing, and answers with
the page rectangles it wants redrawn. The application draws no field editor and
never sets a value from outside the page; `set_field` exists and refuses.

That is not fastidiousness. A second implementation of "what a filled field
looks like" disagrees with the first somewhere, and where it disagrees is
between what the person filling the form sees and what the file will show
everybody else. Deleting the second implementation deletes the whole class.

Three consequences are easy to get wrong and are worth writing down:

+ *Every render of a document with a form needs an `FPDF_FFLDraw` pass.*
  `FPDF_RenderPageBitmap` draws the appearance stream the file was saved with.
  A value typed a second ago lives in the form-fill environment and is in no
  appearance yet, so without the compositing pass someone types into a box that
  stays empty.
+ *The page stays loaded for the length of an interaction.* PDFium keys a
  field's editing state — focus, caret, uncommitted text — to the `FPDF_PAGE`
  it was given in `FORM_OnAfterLoadPage`. Loading the page per event hands it a
  different pointer every time, and every keystroke lands on a form that has
  just been told nothing is selected. This is the one place in the codebase
  where a native page handle deliberately outlives the call that made it, and
  what it holds is uncommitted state by definition — which is exactly what a
  worker crash mid-fill is allowed to lose.
+ *Editing keys are characters.* PDFium edits text in `FORM_OnChar` and uses
  `FORM_OnKeyDown` for the keys that move the caret. Backspace sent as a key
  down is accepted and does nothing at all — no error, no deletion.

The environment is also the security boundary. A PDF can carry JavaScript, ask
to open a URL, email itself, upload itself or read a file, and every one of
those is a callback pulpit leaves null: no `m_pJsPlatform`, no `FFI_DoURIAction`,
no `FFI_EmailTo`, `FFI_UploadTo`, `FFI_OpenFile`, `FFI_PopupMenu` or any of the
network ones. `FFI_GetLocalTime` returns a fixed value rather than the wall
clock, for the same reason the Typst compiler is closed-world. The tests assert
each of those is absent, because they are a posture and not an omission.

Measured on the development machines, in a debug build: one keystroke through
the environment costs about 12 µs, and a full-page redraw with the compositing
pass about 240 µs. Both leave a 16 ms frame nearly untouched, which is what
`SPEC-document.md` §14.3's gating spike had to establish — and why the
specification's insistence that the IPC hop stay in place costs nothing worth
having.

== Document mode: the recorded budgets

The six performance claims in `SPEC-document.md` §13.6 are asserted by tests
rather than argued for, and the tests print what they measured so the baseline
can be re-read rather than remembered:

```
cargo test -p pulpit budgets -- --nocapture
cargo test -p pulpit-render --test document_budgets -- --nocapture
```

Two of them are *ratios* and four are absolute. The ratios — does listing a
page's annotations cost more in a larger document, does checking a held frame
cost more when the frame is bigger — are the honest form for a question about
how cost grows, because a ratio reads the same on a fast machine and a slow
one. The absolute thresholds are set an order of magnitude above the baseline
on the development machines: a regression threshold is for catching a
structural mistake, not for failing on a loaded runner.

The baseline, on a debug build (a release build is several times faster, and
the thresholds hold there by a wider margin):

#table(
  columns: (auto, auto, auto),
  table.header([*What*], [*Measured*], [*Enforced as*]),
  [1000 pointer moves during a stroke], [0.46 ms], [under 16 ms],
  [100 strokes drawn and handed over], [1.2 ms], [under 50 ms],
  [Committing a 200-point stroke], [67 µs], [under 20 ms],
  [Listing one page, 4 vs 400 pages], [3.6 µs vs 3.7 µs], [under 8× the small],
  [Render plan, 4 vs 4000 pages], [0.8 µs vs 1.0 µs], [under 8× the small],
  [10 000 pointer samples committed], [132 points], [under 500 points],
)

The last row is the one worth reading twice. A tablet reporting at an absurd
rate must not produce proportionally more audience traffic or a proportionally
larger annotation, and what stops it is not a throttle but the two bounds that
were there anyway: samples closer together than the minimum distance are
dropped, and the committed stroke is simplified. The test draws a wave rather
than a line, because a line simplifies to its two endpoints at any sample rate
and would pass without proving anything.

The render-plan row is what replaced the old staleness check when reader
frames moved into the shared cache. The plan is recomputed every tick, so it
must cost the pages in the window and their margin, never a walk of the
document — which is also why `Column::visible` binary-searches the one
contiguous run of visible pages rather than filtering all of them.

== Definition of supported

A desktop platform is *supported* only when the workspace builds from a clean
checkout with documented prerequisites; the application is packaged with an
icon, identity, required native libraries and platform-standard
install/uninstall behaviour; presenter and audience windows pass the topology,
mixed-DPI, fullscreen, reconnect and sleep/resume scenarios the platform
permits; unsupported placement or fullscreen is detected and explained; native
decorations, paths, shortcuts, dialogs and lifecycle rules are used; keyboard
and screen-reader acceptance checks pass; no known platform failure can
disturb the audience frame; and diagnostics identify the OS, session/window
backend, renderer, display capabilities, scale factors and every fallback in
effect. Physical display qualification is a release gate for each new platform
adapter. Passing compilation alone is *experimental*, not supported.

A new runtime or adapter MUST plug into the same reconciliation and
presentation state machines; it MUST NOT fork the application workflow.

== Review checklist

- Does this preserve the last valid audience frame?
- Is this application behaviour or a platform service?
- Is the view asking for a capability rather than an OS name?
- Does it work with keyboard and assistive technology?
- Are focus, disabled, failure and fallback states visible and explained?
- Does it work at fractional scale and 200% text scaling?
- Is any saved value tied to a monitor index, pixel density, platform path or
  shortcut spelling?
- Does it use semantic tokens and commands?
- Is platform code confined to an adapter, tested with a null implementation?
- Would a failure affect only the presenter window?

= The platform boundary

`pulpit::platform` is the only module that knows about D-Bus, portals,
`xdg-open`, `%APPDATA%` or `~/Library`. Everything else talks to four traits
plus a snapshot:

#table(
  columns: (1fr, 2fr),
  stroke: none,
  inset: 0.55em,
  [*Contract*], [*What it owns*],
  [`PlatformServices`], [appearance, reveal/open, notifications, sleep inhibition, directories, recent documents],
  [`WindowPolicy`], [application id, minimum window size, quit-on-last-close, clamping restored bounds back onto a live work area],
  [`InputPolicy`], [the primary modifier, how a shortcut is written, which combinations the desktop has already reserved],
  [`Capabilities`], [a snapshot of what this session can actually do],
)

`Platform::detect()` assembles them; `Platform::null()` gives a recording
adapter that tests assert against without touching a desktop.

== Outcomes, not booleans

Every operation that leaves the process returns `Outcome`:

```rust
Outcome::Done                          // it happened
Outcome::Refused { by, reason }        // the desktop said no, and why
Outcome::Unsupported { what }          // this session cannot do it at all
Outcome::Failed { reason }             // it should have worked and did not
```

The distinction is not pedantry. "Wayland will not let an application place
its own windows" is a fact about the session that the presenter should be told
once and calmly; "the compositor refused this specific request" is a fact
about this attempt, worth retrying; "the call errored" is a bug. Collapsing
all three into `false` is how an application ends up silently doing nothing on
a projector.

== What a page turn spends its time on

`pulpit::latency` records every page turn and reports it in the
diagnostics bundle, because every performance question asked of this
application so far has been answered by reading the source and arguing, and
the arguments have been wrong about as often as right.

A *turn* is timed from the state moving to the moment each surface showed the
page that was asked for, and is reported two ways: _settled_, the last
surface to answer, and _first picture_, the earliest correct thing on either
screen — which are the answers to two different complaints, "it did not
respond" and "it looked soft for a moment". A turn overtaken by the next key
press is abandoned rather than averaged in: a presenter holding the arrow key
down is not waiting for any of the pages in between.

A *stage* is a named piece of synchronous work, reported with its worst case
as well as its mean. Stages count only what happens on the event loop, where
a millisecond is a millisecond the interface is not drawing — planning
renders, following the committed page with media, and taking delivery of
frames, which includes copying a large one out of shared memory on this
thread. Work inside a worker process is not a stage; it is already visible as
render latency.

Uploading a picture to the GPU is on the event loop too, but outside
`update`: it happens while a window lays itself out. It is therefore reported
through a meter the `residency` widget writes to and the application reads,
rather than as a stage. The meter is shared by handle because its two ends sit
on opposite sides of the view boundary — a window's residency is widget state
reached through `&App`, the recorder is `&mut` only inside `update`, and one
`Cell` between them needs no locking on a single thread.

It was left out at first, on the grounds that the application cannot see what
a window has uploaded. That is true and beside the point: it does not need to
know *which* pictures are resident, only how long it was stopped putting them
there. Leaving it out meant the one part of a page turn that blocks the event
loop outside `update` was the one part never counted, while the report — on
the strength of the parts that were — said the event loop was innocent.

== Capabilities over OS checks

`Capabilities` reports the backend, the quality of display identity
(`Stable` → `Connector` → `Geometric` → `None`), whether arbitrary placement
and safe un-fullscreening are possible, whether appearance
and high contrast can be read, whether sleep can be inhibited, and whether
native dialogs, menus, an accessibility bridge, media keys and notifications
exist. `report()` renders it for the diagnostics bundle and the settings page;
`limitations()` yields the ones worth telling the presenter about.

The X11 adapter claims placement; the Wayland adapter does not, on any
compositor. The UI adapts on the resulting capability claim alone — never on
`cfg!(target_os = ...)`.

= The design system

`crates/pulpit/src/theme/tokens.rs` defines the seven colour roles shared
by Settings and code. Borders, interaction states, and readable text over
fills are derived from them. The same layer defines the 4/8/12/16/24/32
spacing scale, radii, a type scale, and hit-target sizes. Dark and light
palettes are editable; system high contrast wins.

Contrast is a test, not a hope: the defaults keep body and muted text at 4.5:1
and meaningful graphics at 3:1. Settings warns when a custom pairing falls
below those ratios, while foregrounds over fills are selected automatically.

Views read the palette through `theme::ambient`, which is set once per view
pass. The explicit, palette-taking style builders underneath are what the
tests exercise; the ambient layer only spares every widget from threading the
palette by hand.

== Status: toasts, and why they are never the whole story

Notices appear in the corner of the *presenter* window and never on the
audience window. Routine information fades after four seconds, warnings after
eight, and failures do not expire at all — a presentation-critical problem
stays until it is dismissed, and carries a line saying what to do next.
Repeating a message refreshes it rather than stacking a duplicate, and routine
chatter can never evict a sticky failure.

Everything shown as a toast is also written to the diagnostics bundle. A toast
is a courtesy; the bundle is the record.

= Platform support today

#table(
  columns: (0.8fr, 1fr, 1.2fr, 1.2fr),
  stroke: none,
  inset: 0.55em,
  [*Platform*], [*Enumeration and identity*], [*Window placement*], [*Notes*],
  [X11], [XRandR + EDID], [yes, via EWMH], [reference platform],
  [Wayland], [`wl_output` + `xdg_output`], [*no* — compositor placement, explained in the UI], [Iced fullscreens on the monitor it picks; accepted permanently],
  [Windows / macOS], [none], [falls back], [not in this build],
)

Iced 0.14 exposes no monitor enumeration and no way to name the output for a
fullscreen window; `crates/pulpit-display` implements what it can behind a
trait, so an upstream
contribution or a pinned patch can replace an adapter without touching the
application.

= Display-control findings

The original spike question was not "can we draw two windows" — it was *what
does the platform actually permit, and what does parity with GTK cost?* These
are the answers this codebase is built on, derived from the pinned versions in
`Cargo.lock` (iced 0.14.0, winit 0.30.13) and from running the application on
X11.

== Iced's public API has no monitor story

`iced::window` (0.14) offers `set_mode(Windowed | Fullscreen | Hidden)`,
`move_to`, `position`, `scale_factor` and `monitor_size(window)` — the _size_
of the monitor containing a window, nothing more. There is no monitor
enumeration, no monitor identity of any kind, no way to fullscreen onto a
chosen output, and no monitor-connected or monitor-disconnected event.

`window::run(id, f)` hands a callback `&dyn HasWindowHandle +
HasDisplayHandle` — raw window handles, not winit's `Window` — so even a
targeted-fullscreen shim cannot be written against the public API alone. Two
ways forward, in order of preference: contribute `Monitor` enumeration and
`set_fullscreen(Some(monitor))` upstream, or carry a pinned `iced_winit`
patch.

== Monitor identity: what each tier costs on Linux/X11

#table(
  columns: (0.8fr, 1.4fr, 0.9fr, 1.4fr),
  stroke: none,
  inset: 0.55em,
  [*Tier*], [*Source*], [*Available*], [*Cost*],
  [1 — stable], [EDID serial via XRandR `EDID` output property], [yes, when the driver exports it], [~80 lines of EDID parsing, no new dependency],
  [2 — connector + make/model], [XRandR output name plus EDID descriptors], [yes], [free once tier 1 is parsed],
  [3 — make/model/size/position], [as above plus CRTC geometry], [yes], [free],
  [4 — session handle], [XRandR output id], [yes], [free, never persisted],
)

On the reference machine the adapter produced a tier-1 identity from EDID for
the built-in panel, so persisted display choices survive re-enumeration. The
Wayland adapter (`smithay-client-toolkit`) reaches tier 2: connector name plus
make and model.

== Topology events

XRandR provides `RRScreenChangeNotify` / `RRCrtcChangeNotify` /
`RROutputChangeNotify`; the adapter selects them and exposes
`wait_for_change()`. They are treated strictly as *hints*: every
reconciliation re-enumerates, and the snapshot carries a monotonic sequence
number so a delayed notification from an older topology is dropped. The
baseline remains a 1 Hz poll, which is what runs when no native listener
exists.

== Targeted fullscreen

On X11 with an EWMH window manager the sequence that works is: leave
fullscreen, `ConfigureWindow` to the target monitor's origin, then
`_NET_WM_STATE_FULLSCREEN`. Every placement request returns an explicit
outcome — `Applied`, `Refused`, `Disappeared`, `Unsupported`, `Failed` — and
the UI surfaces the ones the user must act on. *Refusal is detected by the
absence of `Applied`, never assumed to be success.*

On Wayland `xdg_toplevel.set_fullscreen(output)` is the only mechanism, and it
must be issued on the toolkit's own toplevel object. winit can do it —
`Fullscreen::Borderless(Some(monitor))` sends exactly that request — but Iced
0.14 gives the application no way to name the output: `window::Mode::Fullscreen`
carries no monitor, and `iced_winit` fullscreens on `current_monitor()`. The
gap is an API one, not a protocol one.

*This is accepted as a permanent limitation.* Closing it means forking or
vendoring `iced_winit`, and carrying a toolkit fork is a worse standing cost
than the fallback. Choosing the output for the audience window is therefore
not a capability the design has at all: it was removed from `Capabilities`
rather than left as a flag no adapter could honestly set, and the application
falls back to compositor fullscreen while saying so in the UI. On a Wayland
session the presenter may have to move the audience window once, by hand,
exactly as in a tiling compositor — a supported configuration, not a broken
one. Nothing else in the design depends on this capability.

The same reasoning removed a compositor-specific escape hatch. An earlier
build wrapped the Wayland adapter on Niri and drove `niri msg action
move-window-to-monitor` over that compositor's IPC, which did place the
audience window. It was deleted: one compositor out of many behaving
differently is a second code path through reconciliation, exercised only on
the maintainer's machine, in exchange for saving the user a single manual
window move. Shelling out to a compositor's CLI is also exactly the kind of
per-desktop special case the platform boundary exists to prevent.

A second, subtler limitation: this
adapter opens its own Wayland connection, so its outputs are correlated with
winit's windows by connector name rather than object identity.

== Scale factor

X11 has no per-monitor scale. The adapter reports `Xft.dpi / 96` as a global
hint, and the diagnostics bundle prints logical × scale = physical per monitor
so a mismatch is visible rather than silently wrong. On the reference machine
winit guessed 1.604 for a 2880×1800 panel while the adapter reported the Xft
value; that real topology is committed as
`tests/topology/08-captured-laptop-fractional-scale.txt`.

On Wayland the check is implemented: `WaylandBackend::scale_checks()` compares
reported logical size × scale factor against the current mode's physical
pixels for every output, and the result is logged at startup and included in
diagnostics. What remains is _running_ it on GNOME, KDE and a wlroots
compositor.

== Capability envelope

Encoded as `Capabilities { arbitrary_position, unfullscreen_safe,
place_before_map }` and consumed by the single
reconciliation function:

- *X11/EWMH*: everything true except `place_before_map`.
- *Wayland*: nothing placeable from here; unfullscreening is _not_ safe, so
  the reconciler leaves a fullscreen audience window alone and says why
  (`CannotLeaveFullscreen`).
- *Tiling WMs (i3/Sway/Niri)*: nothing is placeable; the reconciler skips
  placement and keeps both windows visible. This is a supported
  configuration, not an unsupported one.

== Pre-map placement, suspend and resume

Some window managers ignore placement issued before a window is mapped. Pulpit
needs no configuration flag for it: a placement that is not `Applied` is
queued and retried after the window exists, up to four times with increasing
delay, after which the user is told what to do manually.

Linux's monotonic clock stops while the machine is asleep, so a tick gap far
larger than the 50 ms interval — or a wall-clock gap that outruns the
monotonic one — is a resume. On resume the application re-enumerates the
topology, reconciles, and re-requests the frames both windows need, while
page, preview, timer, blanking state and display roles are deliberately
untouched.

= Presenter layouts and widgets

The tree model lives in `crates/pulpit/src/layout` and each widget family
in `crates/pulpit/src/widgets/<family>/`. Both are pure — no UI types, no
clock, no rendering — so every structural rule is unit-tested without a
display. The designer and the presenter view are two projections of the same
values, which is what makes "what you designed is what you present" true
rather than aspirational.

== The tree

Two node types only:

- a *split* with two or more children in one direction, plus their relative
  sizes, a gap and a minimum child size;
- a *leaf* cell holding at most one widget, plus padding, background and what
  to do when it is empty. Neighbouring cells are separated by one muted
  hairline in the split gutter; cells and widgets do not draw perimeter
  borders.

The tree is kept *canonical*: a split never directly contains a child split of
the same direction. That single rule is what makes divider behaviour
predictable:

- Splitting a cell *in the same direction as its parent* inserts a divider
  into the parent. Splitting the middle of a three-across row left-and-right
  gives four across, not three-across-with-a-nested-pair.
- Splitting *perpendicular* always nests.
- The selected cell's space is halved; its siblings keep their sizes.
- Deleting a node gives its space back to its siblings in proportion. A split
  left with one child dissolves, and if the survivor now sits inside a split
  of its own direction it is flattened in.

Layouts are stored proportionally, so they scale to any screen without
letterboxing or reflow. The editor's aspect-ratio selector changes only what
the canvas previews — never the saved layout. When the presenter screen's real
ratio is far from the design ratio, the presenter window shows one dismissible
notice suggesting a review at that ratio.

== Widgets

One directory per family — slides, notes, timing, navigation, status — each
with a pure `model.rs` (configuration, defaults, bounds, capabilities,
patches, the display decisions, and its own validation) and a `view.rs` that
takes only the input facets it draws.

Every presentation property — variant, scale, alignment, and slide fit — is a
constant in `widgets/tokens.rs`, not a per-widget setting. Colour comes only
from the seven global roles in the active Light or Dark palette. Widgets size
themselves to the cell they are given.

One widget is *compound* — `Previous + Current + Next` — and it counts as its
constituents everywhere: placing it satisfies the current-slide requirement,
and a bare `Current Slide` may then not be placed beside it. The timer, the
clock and navigation are deliberately _not_ compound: a layout may put the
buttons along the bottom and the slider in a rail, or leave either out.

Single-instance widgets are exactly: Speaker Notes, Slide Buttons, Slide
Slider, Pause or Resume, End Presentation, Annotations, Media Transport, Menu,
Start and Stop. Everything else may repeat. A single-instance widget already in
the layout shows *Already in Layout* on its library card and cannot be dragged.

=== Menu, Start and Stop

The hamburger and the audience window's Start and Stop are widgets like any
other, so where they sit at the lectern is the presenter's decision rather
than the application's. They were a fixed strip above the layout, and the
strip is now a fallback: it draws only the half a layout has not placed
itself, and disappears entirely once both are on the layout. That keeps every
layout written before these widgets existed — and every layout that
deliberately omits them — able to open a menu and start a projector.

The fallback offers Start and Stop on a *presentation* layout only. The reader
is a window onto a file rather than a talk, so a projector control there is a
control for something that is not happening, and it would cost the page the
height of a button. A document layout that genuinely wants one places the
widget. Both built-in Readers carry the Menu in their control band, so the
band is the one row of controls it looks like and the strip does not draw at
all.

Their flyouts (the menu itself, and the list of displays behind Start's
arrow) still hang from the top-left corner. They are positioned by
arithmetic, and that arithmetic knows only whether the strip is drawing the
control, not where on the layout the presenter put it.

=== Media Transport

Play, pause and scrub whatever video or animation is on the slide the
_audience_ is seeing — never the slide the presenter has previewed ahead to,
since pressing play must not start a clip nobody is watching.

It exists on the presenter's layout rather than inside the media because the
two views consume the same overlay frames: a control drawn in the content is a
control on the projector. The generated wrapper pages therefore draw nothing,
and this widget reaches them through the media protocol instead.

What it offers follows what the content can answer. A clip gets a scrub bar
and a mute button; an animation has no playhead and no audio, so it gets the
button alone. Media whose runtime never started still gets a transport, drawn
inert and reading *Not playing*.

=== Accent colours

Cyan (default), amber, white or slate — the sanctioned palette, no arbitrary
colours. *Amber is reserved for timing displays.* The Timer takes it by
default and other widgets are not offered it; choosing it elsewhere is refused
with an explanation rather than silently accepted, so the timer stays the one
thing on the screen that reads as urgent.

== Editing

There is no properties panel. Everything is done on the canvas or from the
toolbar, and the canvas draws the real widgets.

#table(
  columns: (1fr, 2fr),
  stroke: none,
  inset: 0.55em,
  [*Action*], [*How*],
  [Select a pane], [click it],
  [Add a pane], [*Pane left / right / above / below* in the toolbar, beside the selection],
  [Place a widget], [drag a card from the library onto a pane, or onto a pane's edge to split and place in one gesture],
  [Move a widget], [drag it between panes],
  [Clear a pane], [right-click it, or ✕ in the toolbar],
  [Remove a pane], [the same again on an empty pane],
  [Resize], [drag a divider; grab it near a corner to move a whole line of aligned dividers; `←`/`→` moves a focused divider 1%, `Shift` 5%],
  [Even split], [double-click a divider],
  [Undo / redo], [↺ and ↻ in the toolbar, `Ctrl/Cmd+Z`, `Ctrl/Cmd+Shift+Z`],
  [Save], [`Ctrl/Cmd+S`, or *Save* when leaving],
)

Four add-pane buttons replace the ready-made shapes: three across is *Pane
right* twice, and where the new pane lands is always obvious from the button
you pressed.

Dropping onto an occupied cell asks: *Replace Existing Widget*, *Swap Widgets*
(between two cells) or *Cancel*. Nothing is destroyed in one press: removing a
pane takes its widget first and the pane itself on the next press, and both
steps are undoable. Removing a split takes only the panes on either side of
it.

There is no save button and no discard button. Leaving the editor with unsaved
changes asks *Save*, *Discard* or *Cancel*, which is the moment both questions
mean something; a toolbar that can throw work away is a mis-click looking for
somewhere to happen. Validation warnings are reported when you save rather
than in a panel.

Undo covers every editing action — structure, resizes, placement — is
unbounded within the session, and is _not_ cleared by saving, so you can undo
past a save. Closing the editor clears it.

== Validation

Validation inspects *configuration, not presence*: a Slide Buttons widget with
its forward button hidden does not satisfy the forward-navigation requirement.

Warnings — no current slide, no forward or backward control, notes too small,
a widget that does not fit its cell, empty cells, an unreadable timer — are
explained and then allowed. A presenter may deliberately want a layout with no
notes. Only structural corruption and instance-limit violations block, which
is what protects an import from a hand-edited file. Geometry warnings depend
on the screen the layout is checked at, so the aspect-ratio selector changes
what the checks say.


== Built-ins

#table(
  columns: (1fr, 2fr),
  stroke: none,
  inset: 0.55em,
  [*Layout*], [*For*],
  [*Slide + Next + Notes*], [The slide-first default: current slide at 72% width, with next slide, notes and navigation in a rail.],
  [*Slide + Notes Beside*], [A 75/25 split that fits a 4:3 slide without padding on a 16:9 presenter display.],
  [*Slide + Time Below*], [A 90/10 split that fits a 16:9 slide above the natural spare strip on a 16:10 display; time and a draggable slider share the strip.],
  [*Slide + Time Beside*], [A 75/25 split for a full-height 4:3 slide, with time and navigation in the side rail.],
)

The names describe fixed geometry. Pulpit does not choose or rearrange a
layout based on the deck or display aspect ratio. Built-ins cannot be renamed,
overwritten or deleted. *Duplicate to Customize* makes an editable copy and
opens it.


= PDFium

PDFium is not vendored in this repository and not linked into the binaries:
it is loaded dynamically at run time, so a build without it still succeeds and
the application degrades visibly rather than silently.
== Binaries

`scripts/fetch-pdfium.sh` downloads a pinned release from
`bblanchon/pdfium-binaries` and verifies its SHA-256 before installing it into
`./lib`. Treat that project as an *unaffiliated third-party supply-chain
dependency*:

- The release tag (`chromium/NNNN`) and the per-target SHA-256 are pinned in
  the script. Never replace them with "latest".
- A target with no recorded hash refuses to install and prints the observed
  hash for review. Verify independently before pinning it.
- Archive the downloaded artefact alongside release inputs so a build can be
  reproduced after the upstream release is deleted.
- Review upstream changes (PDFium version, build flags, third-party notices)
  when bumping.

Currently pinned: `chromium/7999`, `pdfium-linux-x64.tgz`,
`c3af580f9df0fef9545b44115bc5ea440f286956b5f231df69fb373b8efc4f69`.

If the service disappears, PDFium can be built from source with `depot_tools`
and the same GN args the upstream project publishes (`args.gn` ships inside
each artefact and is worth archiving). The application only needs a shared
library exporting the standard `FPDF_*` symbols; nothing in this codebase
depends on that project specifically.

PDFium is BSD-3-Clause. Redistribution obligations, including the bundled
third-party notices shipped as `lib/PDFIUM-LICENSE`, are release requirements
and must be included in any package that ships the library.

== Discovery

Search order in `PdfiumBackend::bind`:

+ `PULPIT_PDFIUM_PATH` (a file or a directory)
+ the directory containing the executable, and `<exe dir>/lib`
+ `./lib` and `.`
+ the system loader path

Failure at every step is reported once, listing the paths tried, and the
renderer worker exits: PDFium ships with every supported package, so this is a
broken installation and placeholder pages on the projector would be worse than
stopping. `PULPIT_FORCE_FIXTURE_BACKEND=1` selects the fixture backend
explicitly, which is how the tests run without PDFium.


= Notes mapping contract

Slide indices are logical, so the mapping — not the PDF — decides how many
slides the deck has. Changing the mapping advances the render generation and
clamps the current slide into the new space.

== Where a mapping comes from

In priority order:

+ A mapping you chose for *this document* (`notes.per_document` in
  `settings.toml`). Nothing overrides it.
+ A recognised *metadata contract* in the PDF, if
  `notes.honour_metadata_contract` is on.
+ Your *default mapping* (`notes.default_mapping`).

== The metadata contract (Typst/Mosaic)

A generator can declare the mapping in any PDF metadata string — `Keywords`,
`Subject` or `Title` — as a single whitespace-delimited directive:

```text
pulpit:mapping=slides-only
pulpit:mapping=split;slide=0,0,0.5,1;notes=0.5,0,0.5,1
pulpit:mapping=alternating;notes-first=false
pulpit:mapping=two-ranges;notes-first=true
```

Regions are `x,y,width,height` as fractions of the page, origin top-left. A
malformed or unknown directive is *rejected*, not approximated: the document
then falls back to your default mapping.

In Typst:

```typ
#set document(keywords: ("pulpit:mapping=alternating;notes-first=false",))
```

The intended pipeline is *Typst/Mosaic → PDF → Pulpit*, while ordinary PDFs
keep working without any of this.
