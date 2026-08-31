#set document(title: [Internals])
#metadata((tags: ("architecture", "platform", "invariants"))) <website-metadata>

#title()

= Architecture

Pulpit has one interaction model, not presenter and reader modes under the
hood. The active layout is only a tree of widgets. Its primary viewer widget
chooses how the current PDF is drawn: `CurrentSlide` is one fitted page on a
black canvas, while `DocumentPage` is a continuous scrolling column. Search,
navigation history, annotations, document state, shortcuts, display control
and every other interaction remain application-wide and do not branch on a
persisted or declared mode. Switching between Presenter and Reader therefore
mounts another layout; it does not enter another state machine.

```
Application (iced daemon, one update loop)
├── PresentationState        authoritative domain state         pulpit-core
├── DisplayCoordinator       snapshots, roles, reconcile()      pulpit-display
├── DocumentManager          watch, debounce, atomic reload     pulpit::doc
├── ipc                      framing, spawn, doorbell, shm      pulpit-core
├── RendererSupervisor       worker pool, IPC, generations      pulpit-render
├── FrameCache               byte-bounded CPU/GPU accounting    pulpit-render
├── atomic                   replace a file, never overwrite    pulpit-render
├── images::PageTable        a folder or comic archive as pages pulpit-render
├── InputRouter              fixed keys + remote aliases        pulpit::settings
├── SessionInhibitor         acquire/release, crash-safe        pulpit
├── Reading (speech)         cursor, sentences, language        pulpit-core
├── Speaker                  engines, voices, downloads         pulpit-media
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

=== The one exception: `pulpit_core::ipc`

`pulpit-core` holds one module that is none of those things. `ipc` spawns
child processes, maps files and blocks on a clock, and it is in the domain
crate for a structural reason rather than a principled one: `pulpit-render`
and `pulpit-media` are siblings that cannot see each other, `pulpit` sits
above both, and the only place all three can reach is `pulpit-core`.

Before it existed, message framing, worker spawning, the wake-up doorbell and
shared-memory naming were written four times between those crates. Two of the
copies had already drifted apart in ways that mattered. A shared-memory sweep
that reclaims files whose owning process has died was taught to one crate's
naming scheme and silently skipped the other's, so every crash with a media
overlay playing leaked its rings into `tmpfs` until the machine was rebooted.
And the fork-bomb marker — the only bound that stops a worker re-executing
this binary and spawning workers of its own — was declared once per
supervisor, each copy carrying a comment saying the declarations had to agree,
with nothing checking that they did; one of four spawn sites had stopped
setting it entirely.

The alternative was a sixth published crate for six hundred lines of pipe
plumbing, which buys a Cargo boundary nobody consumes.

What purity actually buys is fast, deterministic domain tests, and that is
preserved by one rule: *no module outside `ipc` may depend on `ipc`*. The
domain is still pure; the crate is not. The visible price is that testing
`pulpit-core` now touches the filesystem.

== Signing identity boundary

Reusable signature profiles are ordinary schema-versioned settings: a name,
certificate summary, credential reference, and appearance defaults. The
settings file contains neither passphrases nor key bytes. A generated
credential is an encrypted PKCS#12 file beside the settings in `signatures/`,
created atomically with owner-private permissions; an imported credential is
only referenced and is never deleted by Pulpit.

PKCS#12 loading and local ECDSA P-256 generation live in `pulpit-render`'s
signing module, but the UI supervisor owns the key for the short signing
session. It is never sent to the PDFium document worker or renderer workers.
Passphrases and decoded key material use zeroizing buffers, and the editor
clears its passphrase fields when it closes. The Settings signature pad stores
only normalized ink points: those are appearance data, not a cryptographic
signature or a secret.

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
- There are no exceptions, and there used to be one. A coarse stand-in was
  rendered ahead of the real frame on the jumps where a window would
  otherwise hold the wrong page, on the premise that a full page costs an
  order of magnitude more than a small one. Measured on two real books, a
  full page rasterises in about 9 ms against a preview's 5 ms: most of a
  page's cost is parsing and laying it out, which is paid at any size. The
  tier bought a few milliseconds and cost a whole second rung — three times
  the jobs queued in front of the frame somebody was waiting for, and a
  standing family of defects in which a page climbs partway up the ladder
  and stops. A window now waits on one frame, which either exists or does
  not.
- The presenter's current-slide panel changes in the same beat the room sees:
  it is the surface the operator is watching, and holding the previous page
  there until a full render landed made it the last thing in the application
  to answer the key they pressed. A panel with nothing at all still shows the
  deck thumbnail, which is a different mechanism — one picture, given up once,
  never a rung. Blanking is still not mirrored: the room's screen goes dark,
  the presenter's place in the deck does not. Every display change is logged
  at debug level.
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

Everything — key presses, deadline ticks, file-watch hints, topology hints,
renderer, document and media replies — becomes an application message handled by one `update`
function. Subscriptions are stable across view rebuilds, so no watcher or
timer is ever duplicated.

Each worker-facing supervisor is pumped from a one-slot doorbell and from the
slow watchdog tick, both of which run the same bounded drain, so IPC results stay inside the same
single-threaded state transition as user input. The doorbell is a one-deep
channel a worker's reader thread rings after its message is on the queue: it
carries no payload, a burst of finished frames collapses into one pass of the
event loop, and a missed ring costs nothing because the next drain takes
everything waiting. It exists because a finished frame used to become visible
only when the tick next got round to looking, so every rendering, document or
media step paid up to a tick for the poll alone. Reaching a drain budget posts
a continuation message, yielding to drawing without leaving queued work for
the watchdog. The tick remains for deadlines, restart checks, clocks, and
resume detection: a silent worker is exactly what no doorbell reports.

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
+ *The clip itself is a transport too.* A press on a video or an animation on
  the presenter's slide is interpreted by the application — click toggles
  playback, a horizontal drag on a clip scrubs it, a double-click projects it
  across the whole slide area — and reaches the session as the same
  `MediaRequest::{Video, Image}` commands the widget sends. Raw pointer
  events go to web overlays alone: the runtimes carry click-toggle parity of
  their own (the mpv worker and the wrapper pages), and forwarding the press
  *and* interpreting it would toggle twice.

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
+ *No lock is held across an unbounded wait.* A lock the event-loop thread can
  contend for MUST NOT be held by any thread across a wait that has no bound —
  a syscall or a dispatch that may block until an event that is not promised
  to arrive. Work done with such a lock held is bounded request/response
  (a round trip, a flush, an enumeration); a helper thread that waits for the
  platform to speak does so with no shared lock held and with its own timeout,
  after which it reports "no hint" and the caller falls back to polling. The
  Wayland output adapter deadlocked the whole UI on exactly this: its listener
  thread held both adapter mutexes inside a blocking dispatch, and a
  suspend/resume that changed no output meant the compositor never spoke
  again, so the event-loop thread's next snapshot parked forever.
+ *Persisted values are portable.* Settings, cache and log locations use
  platform-standard directories; paths are `Path`/`PathBuf`, not assumed
  UTF-8. Nothing persisted may encode a monitor index, absolute window
  position, OS font name, physical DPI, or platform shortcut spelling when a
  semantic representation exists. Migrations are deterministic and tested from
  fixtures of every supported platform.
+ *A file is replaced, never opened and overwritten.* Every writer that
  replaces an existing file MUST go through `pulpit_render::atomic`: an
  unpredictable hidden temporary beside the destination, created with
  `O_CREAT|O_EXCL` so a planted name or symlink is a refusal rather than a
  write-through, then written, `fsync`ed, renamed, and the directory
  `fsync`ed. Opening the destination directly is forbidden — a crash
  mid-write would leave the reader with neither their old file nor a
  complete new one. The primitive asks for a `Visibility` rather than
  assuming: material of the reader's own is `Private` (`0o600` from the
  instant it exists), and a document they asked us to write to a path they
  chose is `Inherited`, which is their umask's decision to make.
+ *Native shell, Pulpit interior.* Ordinary windows use OS decorations; Pulpit
  MUST NOT draw a custom title bar to look the same everywhere. Inside the
  frame it uses one deliberate visual language rather than imitating GTK,
  WinUI or AppKit.

== Reading aloud

Speech is split the same way rendering is, and for the same reason: the part
that decides is pure and the part that touches the world is not.

`pulpit_core::speech` holds the whole decision half — sentence segmentation,
the reading cursor, language identification and the `Auto` policy — and reads
no clock, opens no device and spawns nothing. That is what makes the awkward
cases ordinary tests: a page whose text arrives after speech has moved on, a
`Finished` that races a pause, a page with no text layer, the end of the
document. None of them need a sound card to reproduce.

`pulpit-media::speech` holds the other half and links *nothing*. The
synthesiser is an installed program driven over a pipe; the audio player is
another one. No inference runtime, no audio library, no engine of any kind is
compiled in.

That is also why speech lives in `pulpit-media` rather than in a crate of its
own. The two halves of that crate share no types and are not one mechanism;
what they share is the policy that gives the crate its shape — a heavy runtime
pulpit *launches* rather than links, discovered honestly, supervised, and
reported on when it is absent. A browser renders an overlay and a synthesiser
renders a sentence, and neither puts an engine inside the presenter binary.

Three things follow from linking nothing, and all three are the point:

+ *Isolation is already there.* A synthesiser that crashes is a child process
  this code spawned, not a library inside the event loop, so speech gets a
  thread where rendering needs a worker process. `pulpit-media` states the
  rule: a runtime that only *launches* an installed program lives in the
  application's own executable; only one that *links* an optional library
  needs a separate binary.
+ *Stopping is a `kill`.* The one thing this feature is judged on is whether
  it stops when told, and killing a player is as immediate as that gets.
+ *The licence boundary is a process boundary.* The current piper is GPL.
  Driving an installed copy over a pipe is not a derivative work; linking one
  into an MIT/Apache binary would be a problem to solve rather than avoid.

Engines and voices are *data*, not code — a catalog of pinned URLs, hashes,
languages and sample rates. Supporting a different synthesiser is an entry in
that catalog. The abandonment risk worth designing against is not that a model
stops being maintained (a pinned `.onnx` keeps working indefinitely) but that
the *program* which runs it is replaced, and a manifest absorbs that.

Three invariants:

+ *Nothing unverified is used.* Every downloaded artifact is checked against a
  sha256 pinned in the binary before first use and deleted if it does not
  match; nothing appears under its final name until it has passed. This is a
  runtime fetch onto a stranger's machine that is then executed, so the hash
  is a security boundary rather than the reproducibility nicety it is for
  `make pdfium`.
+ *Sample rate travels with the audio.* It is a property of the voice — never
  of the engine, the quality tier or the platform. The shipped catalog holds
  16000, 22050 and 44100 Hz voices, and two voices of the same language and
  the same tier disagree. Assuming it produces chipmunk or slow-motion speech.
+ *Availability is three-valued.* `Capabilities::speech` distinguishes "this
  session cannot" from "one download away" from "ready", because the two
  negative answers have different remedies and a greyed-out control expresses
  neither. This is the `accessibility_bridge` argument applied again.

Latency is paid once. A synthesiser is spawned per sentence — which makes the
end of an utterance unambiguous, since a closed stdout *is* the frame boundary
— and the cost of the next spawn is hidden under the sentence currently
playing. Measured on the shipped engine: a voice loads in about 0.13 s and
synthesises at roughly twenty times real time, so the gap is inaudible except
before the first sentence and at a page turn, where the next page's text has
not been asked for yet.

The known limitation is reading order. PDF text extraction returns content in
content-stream order, which on a two-column page interleaves the columns line
by line. Speech is where that becomes audible rather than merely untidy, and
it is a property of the text layer rather than of anything above it.

== Visual and interaction system

+ *Seven colour roles, no other vocabulary.* `canvas`, `surface`,
  `slide_canvas`, `text`, `muted`, `accent`, `alert`. Borders, overlays,
  disabled and interaction states, and readable text on colour are derived
  centrally from those roles. Components MUST NOT invent aliases such as
  `primary`, `danger` or named hues. Spacing (4/8/12/16/24/32), radii and
  typography scales likewise live in the theme layer, not in views.
+ *Platform typography, deliberate readouts.* Interface prose and controls use
  the platform UI sans so Pulpit belongs on the host desktop and inherits its
  language coverage. Clocks, timers, page positions and other compact numeric
  readouts use the bundled DejaVu Sans Mono role so changing digits do not
  shift or vary between machines. A display face is not part of the interface
  vocabulary.
+ *Five type roles, and a view picks a role rather than a size.* Title (22) for
  the name of a dialog, overlay or page, one per surface; heading (17) for a
  section within one; body (14) for prose; label (12) for the text inside a
  control and for the field labels above them; caption (11) for subordinate
  metadata. `theme::typography` hands out the role already carrying its size,
  weight and colour, and views MUST ask it rather than call `.size()` with a
  token; what is left for the constants is sizing something that is not text.
  Title and heading also take the platform face one weight up, because three
  points is a difference a reader measures rather than sees. The ladder is
  strictly increasing and tested as such: a header MUST NOT be set smaller
  than the text beneath it. A field label is the exception that proves it —
  it is a weight above, and never a size below, the control it names.
+ *Geometry follows function.* Controls and fields use a 4-pixel radius,
  panels 8, and dialogs 12. Pills are reserved for genuine segmented choices;
  passive surfaces do not accumulate nested cards. Shadows are reserved for
  transient overlays that must separate from content beneath them.
+ *Accent is a budget.* It marks focus, selection, current/live state, and the
  primary forward action. Passive headings and ordinary readouts use text or
  muted roles. Alert is reserved for warnings, errors, overtime, and
  destructive actions.
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
+ *Layouts are device-independent.* Flexible tracks use normalised split
  proportions; a cell may instead hug its widget's declared minimum extent on
  either axis, also in logical units. No monitor index, desktop coordinate, OS
  font name or physical DPI. The designer canvas MUST use the same layout
  algorithm as the live presenter view — a separate approximation is
  forbidden.

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
  Search hits, bookmarks and form-field lists use immutable `Arc` snapshots
  for the same reason: responsive view closures clone a pointer, not every
  row in a large document.
+ *Polling is a watchdog, not a delivery path.* Renderer, document, media and
  file-watch queues ring one-slot doorbells after enqueueing. X11 and Wayland
  topology listeners block on native change notifications and re-enumerate on
  a helper thread; backends without a listener use a one-second helper-thread
  fallback. Work merely being in flight does not keep the 50 ms animation
  tick alive or rebuild both window trees.
+ *A search is a stream of questions, not one question.* Every keystroke in
  the find box restarts the document scan, so each of the four things that
  made one scan slow was paid once per letter. All four are fixed in kind
  rather than tuned: typing is held for 120 ms before a scan starts, so a word
  is one scan and not six; a running scan is `is_live`, and the next chunk is
  released the moment its answer lands rather than on the next tick, so the
  scan runs at the worker's rate and not the timer's; three chunks are
  outstanding at once and the first covers four pages rather than
  thirty-two, so the first hits arrive in a round trip; and a scan starts at
  the page the reader is on and wraps, because the hit somebody searching from
  page 300 wants is usually near page 300. A chunk whose generation the reader
  has already typed past is answered empty by `reader_link::superseded`
  instead of being run — but only when a *newer* generation is queued behind
  it, never when it is one of several chunks of the current scan.
+ *A page's text is extracted once, not once per query.* `FPDF_LoadPage` plus
  `FPDFText_LoadPage` — parsing a content stream and laying out every glyph on
  it — is the expensive half of searching a page, and re-paying it per
  keystroke made a long deck feel like a slow application. `pdf::search::PageText`
  holds the extracted text and its character-to-UTF-16 offset map behind a
  size-bounded cache in both PDFium backends, and matching runs through
  `pulpit_core::search`, the same matcher used for notes and bookmark titles.
  So the second query over a document asks PDFium for nothing at all on the
  pages that do not match, and only for rectangles on those that do. The
  geometry contract is unchanged: hit offsets are mapped back through the text
  PDFium itself produced, so a mark and its match cannot disagree. The cache
  is dropped whole before any mutation, and per document on close.
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
+ *Measure before restructuring.* Three recorded negative results: replacing the
  CDP pipe's 1 ms retry sleep with `poll(2)` cost 2-3x the worker CPU for one
  to two milliseconds of latency (the sleep stays, with a comment saying why);
  and the 50 ms animation tick MUST NOT be removed until a wrapped GUI build
  has actually been profiled. Static-analysis findings are hypotheses until
  measurement attaches numbers to them, and debug builds never set targets.
  Doorbells do not remove that tick: it still owns animations and timed UI
  transitions, while the 250 ms watchdog owns deadline and restart checks.
  They remove queued delivery and mere in-flight work from the reasons to run
  it. The third: a form commit was going to keep
  its render generation and invalidate only the committed page, so that the
  snapshot's reopen stopped cooling every visible page at once. Measured
  first (`cargo test --release -p pulpit-render --test document_budgets`, and
  the numbers are in that file's `commit_path` module): the incremental
  snapshot write is about 5.3 ms and the reopen about 0.26 ms — a fixed cost
  the change could not remove — while a cold page costs about 2.4 ms at a
  1080p reader cell and 9.8 ms at a HiDPI one. The
  reader has one or two pages on screen, redrawn across two to six pool
  workers, behind a 250 ms debounce, with the previous frames still standing
  under A7 so nothing blanks. What the change would buy is a few milliseconds
  off a path that already waits a quarter of a second; what it would cost is
  the generation invariant becoming per-page and a correctness hazard it
  cannot see — a calculate script rewrites a field on another page while
  `FFI_Invalidate` reports dirty rectangles for the current page only. Not
  worth its invariant amendment; the whole-document snapshot stays.
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

== Superfluity

A thing is superfluous when deleting it changes no behaviour and loses no
knowledge. Behaviour is what the tests and the application exercise; knowledge
is what would have to be re-derived. Git remembers deleted code, so "we might
want it later" is not knowledge — a written reason is.

+ *A dead-code allowance names its item and its reason.* It MUST be written as
  `#[allow(dead_code)] // <why this is kept>` on the item itself.
  `#![allow(dead_code)]` at module scope is forbidden: it also hides every
  *future* dead item in that module, permanently and silently. Two reasons
  are in use and they are not interchangeable — `reached by its tests, not by
  the application` for API kept alive by the tests beside it, and `unreached,
  including by its own tests` for what nothing calls at all. The difference
  between `cargo check --bins` and `cargo check --tests` is what tells them
  apart. The standing exception is `vendor/mod.rs`, which silences the
  vendored `iced_aw` tree: that code is upstream's, and a per-item diff
  against it is a diff to maintain for ever.
+ *Two implementations of one OS-level object MUST agree on their safety
  properties* — creation mode, name predictability, and whether an existing
  file is adopted or refused — even when their data structures differ and no
  code is shared. A divergence there is a bug in the weaker one, not a style
  difference. This is not hypothetical: `pulpit-media`'s shared-memory ring
  once created its regions in world-writable `/dev/shm` under a name derived
  from the pid and a counter, adopting an existing file rather than refusing
  it, at whatever the umask gave — while `pulpit-render`'s equivalent had an
  unpredictable name, `create_new`, and `0o600`. It went unnoticed for
  precisely the reason the two were filed as "similar but deliberately
  separate."
+ *A pass that finds nothing records the instrument, not just the result.*
  "`jscpd` found no clones" and "there is no duplication" are different
  claims, and publishing the second on the strength of the first is how four
  independent implementations of atomic file replacement survived a clean
  copy-paste audit — they were not copies, so a token-level detector could
  not see them. State what was run and what it can see.
+ *An unused declaration is not an unused compilation.* Grepping for
  `<crate>::` finds dependency declarations nothing names, which is a
  different thing from a dependency nothing builds — a direct declaration
  usually resolves to a copy already in the graph. `cargo tree -i <crate>` is
  the check that tells them apart, and it MUST be run before a removal is
  described as a saving.

Two negative results here, recorded so they are not re-litigated without
numbers. Consolidating `pulpit-render`'s `shm.rs` with `pulpit-media`'s
`surface.rs` is *not* advisable: one is a single resizable region, the other a
ring of fixed slots with hold/release, both already delegate naming and
path-safety to `pulpit_core::ipc::shm`, and the residue is a six-line
`path_for` that cannot move into `ipc` because it wraps `ipc`'s `Option` into
each crate's own error type. And `view.rs` and `layout_renderer.rs` are one
layer rather than two competing ones — `view.rs` delegates at four call sites.

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

Two things follow from the first of those, and both were once wrong in the
code. The palettes offer the *same marks* — ink, the highlighter, a label, a
note, the eraser, the rubber band and the text selection, in the same order,
under the same digits.
What presentation has and document mode does not is the pointer, which makes no
mark at all; what document mode has and presentation does not is moving and
resizing a held mark, because a mark dragged to a new place mid-talk is a mark
the audience watches move. A tool one palette had and the other did not was
never a restraint on what could reach the file: it was only a mark the
presenter had to change mode to make.

The text selection is the highlighter's sweep with nothing committed at the
end: the same engine queries over the same extracted text, and a release that
holds the words instead of marking them. What it holds outlives the drag —
that is its whole point — and answers exactly two questions: the primary-C
chord copies it, and the read-page key reads it aloud instead of the page
while one is held (the menu row says "Read page or selection" for the same
reason). It is put down by the things that would make it stale: a new
gesture, a change of tool, a page change, Escape.

Three of the palette's controls are *one control with a mode* rather than
several buttons: the pointer is a dot or a spotlight, the rubber band gathers
marks or copies a region or copies text, and the highlighter washes the words
it sweeps or underlines them or strikes them out. In each case the gesture is
identical and only the answer differs, so they share a slot and part company in
the tool's options, beside the colour. A palette row is finite, and three
buttons that are one gesture crowd out three that are not.

The highlighter's three are `/Highlight`, `/Underline` and `/StrikeOut`: the
same draft, the same `/QuadPoints` over real extracted text, and the same rule
that none of them may be dragged somewhere else. What differs is the subtype
written to the file — and it must actually differ, because an underline written
as a highlight looks right in pulpit, which draws it from the kind it
remembers, and is a yellow wash in every other reader.

And the overlay draws *every* kind the document holds for the page, not only
ink. A slide's pixels are rendered without annotations, which is what keeps an
unfinished gesture and the mark it becomes from being on the screen at once;
the price is that a mark the overlay does not draw is a mark presentation does
not show at all. A highlight made at the lectern used to disappear at the
release for exactly that reason — it was in the file, and on nobody's screen.

The conversion between slide space (fractions of what the projector shows) and
canonical page space (PDF points from the crop box's top-left corner) lives in
`pulpit_core::annotate::presenter` and nowhere else, in both directions. A
split-page deck is why it is not a scale factor: a slide can be half a physical
page, and a mark two thirds of the way across the slide is one third of the way
across the paper. The round trip is property-tested across every rotation, crop
and region, and again through a real PDF in
`pulpit-render/tests/presenter_ink.rs` — a mark that moves between the talk and
the file is a bug nobody would find until afterwards.

=== Shapes, and which subtype each one becomes

A box round a figure, a circle round a number and an arrow pointing at the
thing you mean are one tool with a mode — `ShapeKind` — for the same reason
the highlighter has three nibs and the band has three kinds: the gesture is
one drag, and only what it leaves behind differs. Four more buttons would have
made the palette a rail of icons.

What they become in the file is *not* one family, and that is a decision
rather than an oversight:

- A box is a `/Square` and an ellipse is a `/Circle`. Those are the
  annotations PDF has for exactly them; Okular and Acrobat draw them as
  shapes and let their own users edit them. Neither needs an entry pulpit
  cannot write: `/Rect` is the whole of their geometry.
- A line and an arrow are `/Ink`. `/Line` keeps its endpoints in `/L` and its
  arrowheads in `/LE`, both arrays, and PDFium's annotation API can write
  neither — it offers strings, a rectangle, a colour, a border, flags, quad
  points and ink strokes, and nothing that writes an array of numbers or of
  names. A `/Line` without `/L` is malformed. The alternatives were a
  rasterised `/Stamp`, which is a picture in every other viewer and cannot be
  edited anywhere, and this: a real, editable, universally drawn stroke that
  happens to be straight. An arrow is one stroke that doubles back —
  shaft, tip, barb, tip, barb — so its head is part of the same mark and not
  three marks to erase separately.

Both halves are drawn from one piece of arithmetic. `shape_outline` returns
the polyline a shape is previewed as while the hand is still moving, and for
a line and an arrow it is *also* what is committed as the `/InkList`: a
preview built from different arithmetic than the mark is a preview that can
disagree with what lands on the page. A box and an ellipse are drawn on that
same rectangle, with their border half a width inside it — where PDF 12.5.6.8
puts a square's border — so at a wide pen the committed mark settles a few
points in from the line the hand was shown.

`/Square` and `/Circle` carry an appearance pulpit writes itself, for the
reason a note's icon does (§7.4): PDFium generates one from its own reading of
`/C`, `/IC` and the border, and a mark that looks different in each viewer is
not the mark the reader made. The shape is inset by half the border width,
which is where PDF 12.5.6.8 puts a square's border — drawn on the rectangle
itself, half the stroke would fall outside `/Rect` and be clipped. Nothing
writes `/IC`: a box round a figure has to leave the figure visible.

An appearance is only ever *re*generated over a mark pulpit drew. A move
rewrites the rectangle and leaves the drawing alone otherwise, because
regenerating one over an annotation pulpit did not draw replaces what the
file says with what pulpit would have said — another producer's filled,
dashed, cloud-bordered ellipse becomes a plain stroke, and its `/Contents`
becomes the word "Rectangle". `Writing` is the distinction, and A5 is the
reason for it. A stamp is stricter still: nothing readable in a `/Stamp` says
whether its picture is a check, a cross or a rasterised Typst mark, so a
summary of one has to guess, and a move that acted on the guess would turn
every stamp it touched into the guess. A stamp's appearance is written when
the mark is made and when a rewritten Typst mark hands over a new picture,
and at no other time.

The stamp is the same story a step along. Its machinery already existed —
`StampMark`, `StampDraft`, the `/Stamp` subtype, an icon — and was reachable
only as an implementation detail of the text tool, which carries a rasterised
Typst mark as a picture. What was missing was a way to arm it and an
appearance for the two marks that are not pictures; a check placed without one
was an annotation in `/Annots` and on nobody's screen. `StampChoice` is what
the palette holds — a check or a cross — and it is deliberately not
`StampMark`, whose third variant carries a picture: a picture is something a
reader supplies rather than a mode a button can be in.

=== The annotations panel

The sidebar's third tab lists every mark in the document, and it is a view of
the file's annotations in exactly the sense above: its rows are built from
`AnnotationSummary`, nothing is stored beside them, and a row deleted from the
list makes the same `AnnotationCommand::Delete` transaction a delete on the
page makes — one revision, one undo entry, in user action order with
everything else. It is reached by pressing its icon in the rail a reader has
already opened and by nothing else: the marks are worth a tab, and a reader
who has not opened the sidebar is not looking for them, so there is no key.
Like the form tab, it is offered only where it could hold something — a
document pulpit cannot annotate never grows one.

Three things had to be decided.

*Whose list.* Okular shows an author and a date per annotation because its
reviews panel is built for several people marking up one document. Pulpit's
marks carry neither, and this panel is an index rather than a review tool: a
row says what a mark is, what page it is on, and what it says — the Typst
source for a mark pulpit wrote, `/Contents` otherwise, and for an ink stroke,
which says nothing at all, its kind and its page. Writing `/T` and `/M` is a
decision about the model, and can be made later without changing the panel.

*How the whole document is enumerated.* `ListAnnotations` answers one page and
a panel is about all of them, so it sweeps the document a chunk at a time
behind the window's own pages and shows what has arrived while it fills — the
shape search already uses for the same problem, down to what the bound counts.
It bounds what is *outstanding*, not what one call may ask for, and that
distinction is the whole of it: the pump runs on every tick and again on every
answer the worker sends, so a per-call bound would let each answered page
start another chunk and the queue would grow by a chunk per answer until every
page of a five-hundred-page document sat in front of the render the reader is
waiting on. The window's own pages are counted in the same bound, because they
go to the same worker through the same queue.

*Keeping it current.* Every edit names the pages it touched, and those pages'
annotation lists are dropped — which the eraser's hit-testing already
depended on. The panel is rebuilt from that same drop and the sweep asks for
the dirty pages again, so the list follows `DocumentRevision` rather than a
timer. It is also built only while it is the rail's view: every page that
scrolls past reports its marks, and rebuilding a list nobody has open would be
work done for no reader.

Marks the document arrived with are listed beside the reader's own, with their
`AnnotationSupport` said in words — `read-only`, `not editable here`,
`malformed` — and no delete control, because deleting is a rewrite and pulpit
does not rewrite what it does not model (A5). A dimmed button that refuses
when pressed would say less and promise more.

== One render pipeline for slides and pages

The reader's pages are rendered by the same supervised worker pool, through
the same byte-budgeted frame cache and the same shared-memory transport as
the projector's slides — as `FrameKind::Page` entries whose jobs set
`with_annotations`, because on a reader page the document's own marks are the
point. The bespoke path this replaced — one serial document worker answering
render requests over a pipe, pixels inline, no cache, no cancellation — is
why paging used to lag behind stale renders and why a settling page could be
repainted by a late frame.

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
into the cache: the lookup walks generations from the newest down, so a page
keeps its pre-edit picture until a complete frame containing the edit exists,
and a frame from an older generation can never repaint a newer one. Before
the first edit no snapshot exists and none is taken: pages render from the
presentation's own document, already open in every pool worker.

What stayed in the document worker is everything that is not rendering: the
annotation transactions, the undo history, text selection (both ways of asking:
from one character to another, and everything inside a rectangle), the outline
— and form filling entirely, because a focused field's uncommitted state
lives in PDFium's form-fill environment, keyed to the live `FPDF_PAGE`, and
exists in no saved copy.

== Bookmarks: the one edit PDFium cannot hold

The outline is the one thing the reader edits that PDFium cannot carry for
them: `FPDFBookmark_\*` is a read-only family, so an edited bookmark tree has
nowhere to live in the open document. It lives in the engine instead —
`PdfDocument` adopts the tree as a model on the first bookmark edit — and
every create, rename and delete is an ordinary `DocumentCommand` in an
ordinary transaction: one revision, one undo entry, journalled and replayed
like a mark. A command names its entry by tree path rather than by a minted
identity, and the optimistic revision check is what makes a position sound: a
path built against revision N is applied at revision N or refused, and a
journal replay resolves every path identically because it replays the same
transactions in the same order against the same file.

The tree reaches the file the way ISO 32000-2 §7.5.6 says a finished file is
modified. Save As lets the backend write as before; then `pdfoutline` appends
an incremental update to what was written — a freshly numbered outline item
per entry (§12.3.3), a new `/Outlines` root, and the catalog re-emitted under
its own object number — composed, like `sign::apply`, from `verify` (finding
the catalog, walking the page tree for `/Dest` page references) and
`pdfwrite` (the incremental writer signing already uses). Every other viewer
reads the result as ordinary bookmarks, because that is exactly what it is.
An encrypted file is refused whole before anything is built, and A6 stands
throughout: the source is never written, and a failure in the append fails
the save while leaving the destination holding the backend's complete write.

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
  [`pdfform-testkit` entire], [`pulpit-render`'s `tests/testkit`],
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
the page rectangles it wants redrawn. The application draws no field editor,
and the two writes that do not come from someone typing — undoing a fill, and
committing a date or a time a picker chose — go through `set_field`, which is
PDFium's editor driven from outside rather than a second one.

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

The environment is also the security boundary, and the line it draws is between
a form computing its own values and a form reaching outside the process.

A form's own field scripts — format, keystroke, validate, calculate — run.
Real-world forms are built out of them, and a viewer that skips them shows
different numbers from the ones the file describes. Two things are required and
neither works alone: the pinned library must be a `-v8` build, and
`m_pJsPlatform` must be non-null, because PDFium's header says a null one
prevents JavaScript from executing at all. Both were once the other way round,
so the regression this guards against is silent in both directions — a
`pdfium_form_javascript` test types into one field and reads the answer out of
another that only a calculation could have written.

Two constraints come with that build and are not negotiable. The published
`-v8` artifacts are also XFA builds, and `FPDFDOC_InitFormFillEnvironment`
answers a version-1 `FPDF_FORMFILLINFO` with a null handle — which reads
downstream as "this document has no form", with no error anywhere. The struct
is therefore version 2, with `xfa_disabled` set. And PDFium's V8 isolate
belongs to the thread that created it; using it from a second thread is a
segmentation fault inside V8 rather than an error return. The document worker's
`serve` loop is single-threaded, so a running pulpit satisfies this for free,
but tests do not — libtest gives every test its own thread, which is what
`testkit::on_the_pdfium_thread` exists to undo. `DocumentBackend` and its
PDFium implementation are deliberately not `Send`, so a document MUST NOT be
moved between threads and the compiler, rather than a comment, is what says so.

What does not run is anything that leaves the process. Opening a URL, emailing
itself, uploading itself, reading a file: every one of those is a callback
pulpit leaves null — `FFI_DoURIAction`, `FFI_EmailTo`, `FFI_UploadTo`,
`FFI_OpenFile`, `FFI_PopupMenu` and the network ones. The JS platform's own
callbacks are all implemented, because a null one is a crash rather than a
refusal; each records a `HostRequest` and returns the answer a dismissed dialog
gives, so the script finishes and the application — the layer with a user in
front of it — decides what to honour. The tests assert each null is still null,
because they are a posture and not an omission.

The clock is the one thing a script still reaches, and the boundary is narrower
than it looks. `FFI_GetLocalTime` returns a fixed value rather than the wall
clock, which closes the *host* clock — the one PDFium asks its embedder for, and
the one a viewer would answer from the system time. It does not close
`new Date()`: under the V8 build that is V8's own, on V8's own clock, so a
calculation script that stamps the date it was filled on gets the real one.
Measured, not assumed — `a_scripts_own_date_is_the_real_one_and_this_callback_does_not_change_that`
asserts a plausible current year, so a build that ever did close V8's clock
fails there and sends the reader back to this paragraph. Closing it would mean
reaching into V8's time source, which is not reachable through a library loaded
at run time. What the fixed value buys is that nothing pulpit *hands over* is a
clock reading, which keeps the callback consistent with the rest of the engine —
time is passed in, never read — and leaves one fewer path to close if V8's clock
ever becomes controllable.

Reporting an attempted submission cannot be done as it happens.
`doc.submitForm` is refused through the null
`FFI_UploadTo`/`FFI_PostRequestURL` rather than through the JS platform, and
wiring those two callbacks does not help — measured, neither is ever reached.
So both halves of the reporting are static, done once at open:

- The four `/AA` field scripts are read through
  `FPDFAnnot_GetFormAdditionalActionJavaScript`, which hands them over
  decompressed, and a form naming `submitForm`, `mailDoc` or `launchURL` warns.
- A widget carrying an `/A` action dictionary warns as well. *Which* action it
  is cannot be told: `FPDFAnnot_GetLink` answers null for a widget, and
  `FPDFAction_GetType` has no value for `/SubmitForm` or `/ResetForm` anyway.
  One warning covers submit, reset and script buttons together, which is
  honest, because pulpit presses none of them.

Between them a form that tries to leave the machine is named before anyone
types into it — which is the moment that matters, rather than afterwards.

== Who draws a field's value

PDFium splits a form's pixels in two, and both halves have to be drawn by
whoever is producing the picture. `FPDF_RenderPageBitmap` draws page
*content* — the boxes, the rules, the printed labels. A widget's *value* is
drawn separately, by `FPDF_FFLDraw`, out of a form-fill environment.

That is true of a form nobody has touched. The values were already in the file;
they are simply not part of what a page render draws. So a renderer with no
environment produces a form that looks blank, and the only field that ever
appears is one that a §9.4 partial repaint happened to cover — because those
come from the document worker, which has an environment for editing. The
symptom is "the entries only show up when I click on a field", and it has
nothing to do with clicking.

The render pool therefore keeps a form-fill environment per open document that
has one, purely to draw with — no events are ever forwarded to it. It costs
nothing for a slide deck, where `FPDF_GetFormType` says there is no form.

Exactly one environment may exist per `FPDF_DOCUMENT`. Two draw every field
twice, and the second pass over the first is visible as text heavier than the
page around it. The document engine opens its document *through* the pool
backend and then starts an environment it can also type into, so it releases
the backend's on the way past: editing and drawing belong together, and
whoever is editing keeps the environment.

== Following the pointer over a form

PDFium wants `FORM_OnMouseMove` so a button under the pointer draws its
rollover appearance. The worker is serial, and a round trip per pointer sample
would queue in front of the page renders — the same trap the text-selection
query fell into, where every sample queued a query and the one that committed
sat behind the backlog.

The same cure applies: at most one move in flight, at most one waiting, and the
one waiting is always the newest, because an intermediate position the pointer
has already left is not worth drawing. Coalescing rather than a clock is what
keeps the rollover as close to the hand as the round trip allows. The guard
must be released on a *refused* event as well as an answered one, or a document
that will not take form events latches it shut and the form stops following the
pointer for the rest of the session — which is why a refusal is reported rather
than dropped.

== Choice fields, and why the two kinds differ

A list box moves its own selection on `FORM_OnKeyDown` with an arrow key. A
closed combo box ignores exactly the same key: in a real viewer that key would
be travelling to a dropdown that is not open, and PDFium has no dropdown open.
Clicking the arrow does not open one either.

So a combo box is the field the application has to translate for, and
`FORM_SetIndexSelected` — carried as `SelectOption` — is PDFium's own way in:
the engine performs the selection, generates the appearance and reports the
change, exactly as it does for a keystroke. To choose an index the application
needs to know what is selected now and how many options there are, which is
what `FormEventResult::focused_choice` carries.

It is reported for a list box as well, because the drawn list — the one place
the application renders a field's editing state (§8.6) — is drawn for every
*non-editable* choice field, list box and combo box alike, and it needs the
labels and the selection to draw. What the backend reports rather than what the
application guesses is which kind it is: `list_box` says a list box, `editable`
says an editable combo. The arrow keys follow those flags — a list box moves
its own selection natively and is never stepped by the application, so nothing
is double-stepped; an editable combo keeps the engine's list entirely, because
it has a caret PDFium is drawing and a second surface over that is what §8.6
forbids. Stepping a combo stops at both ends rather than wrapping, and a combo
holding a value outside its own `/Opt` list starts at the near end.

A combo box is also not a text field, so PDFium never calls
`FFI_SetTextFieldFocus` for one. Deciding the keyboard belongs to the form on
that callback alone left the arrow keys with the toolbar, scrolling the page.

== Undoing a filled field

A field edit is a document change like any other, and §9.1 wants it in the same
history as the marks — a fill followed by a stroke undoes the stroke first. The
inverse is a `SetField` carrying the field's before-image, captured before the
event that commits it, because afterwards the old value is gone.

Putting a value back is not quite the only write that does not come from
someone typing — the date and time pickers send the same `SetField` forward to
commit a value someone chose rather than typed, which is what makes a picked
date an ordinary edit with an ordinary undo instead of a second kind of
change. Both go through the editor anyway: focus the widget,
`FORM_SelectAllText`, `FORM_ReplaceSelection`, kill the focus to commit. PDFium does exactly what it
does for a person who selected all and typed, so there is still one
implementation of what a value looks like in a field. Two details are
load-bearing. The annotation handed to `FORM_SetFocusedAnnot` must come from
the page the form environment has open — an annotation read off a second
`FPDF_PAGE` for the same page is not one PDFium recognises, and it refuses the
focus silently. And the commit that reports the change has to be named from the
focus captured *before* the event: a focus loss is the usual way a text field
commits, so by the time the commit is reported there is no focused widget left
to name it. Both of those were bugs that no test caught, because nothing
asserted on the committed field's name.

The kill is also why the pickers close before they commit rather than after.
`FORM_ForceToKillFocus` leaves the field unfocused, so the caret is gone by the
time the change is reported and the next form event reports focus afresh; a
helper still on screen would be anchored to a field nothing is in.

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
  [`PlatformServices`], [appearance, opening a URL or file, sleep inhibition, directories, putting an image on the clipboard, sending a document to a printer, or to the platform's own print dialog],
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
a millisecond is a millisecond the interface is not drawing — the whole
`update`, rebuilding each top-level view, planning renders, following the
committed page with media, and bounded delivery drains for renderer, document,
and media messages. Frame drains include copying a large frame out of shared
memory on this thread. Work inside a worker process is not a stage; it is
already visible as render latency.

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
native dialogs, menus, an accessibility bridge, media keys, an image
clipboard and printing exist. `report()` renders it for the diagnostics bundle and
the settings page; `limitations()` yields the ones worth telling the presenter
about.

The image clipboard is the one service that reaches past the toolkit: Iced's
clipboard carries text and nothing else, so `PlatformServices::copy_image`
goes around it instead. It is a capability rather than an assumption
because a headless session and a compositor with no data-control protocol both
have nowhere to put the pixels, and the select tool's image kind has to be able
to say so before it spends a render on one.

On Wayland the copy is three offers at once, through `wl-clipboard-rs`
directly: `image/png` for anything that pastes pixels, and a PNG written into
the cache offered as `text/uri-list` and `x-special/gnome-copied-files` for
anything that pastes files — which is what a file manager is, and why a
one-format copy pasted into Thunar as nothing at all. X11 and the other
desktops keep the one-format `arboard` path, and a Wayland session where the
direct offer fails falls back to it: pixels alone still serve every paste but
the file manager's.

The X11 adapter claims placement; the Wayland adapter does not, on any
compositor. The UI adapts on the resulting capability claim alone — never on
`cfg!(target_os = ...)`.

== Printing: the file goes out, not the pixels

The renderer worker draws any page at any scale, so pushing bitmaps at a
printer was available. It is the wrong half of the job to take. Duplex, paper
sizes, trays, margins and colour management belong to the platform's own print
system, along with the dialog that lets someone choose between them, and none
of them are improved by being written a second time here. So pulpit answers
only what nobody else can — whether the reader's own marks and form entries
are on the paper, and, where the session has no print dialog of its own, which
pages — writes a PDF that says exactly that, and hands the file over.
`crates/pulpit/src/printing.rs` is that decision; the dialog is
`PlatformServices::print_with_dialog` and the bare spooler behind it is
`PlatformServices::print`.


Printing "as I have marked it up" means the annotations and the field values
as they are *on screen*, which is not what is on disk until a Save As has been
made. Rather than make the reader save first, printing asks the worker for the
same copy Save As would write, to a scratch directory, spools that and deletes
it. Two things hold about that copy and the code says both: it is never
offered as the document, and for a signed document it is not the signed one —
new bytes, a new file, and a name (`printing::spool_name`) that says what it
is. The print queue still shows the *document's* name, because a reader
looking at their queue should not have to recognise "(to print 4213)".

Three capabilities rather than one. `printing` is whether there is anything
here to hand a file to at all â a spooler or a system dialog, since a
sandboxed session can have the portal and no `lp` and prints perfectly well. `system_print_dialog` is whether the desktop
puts up a print dialog of its own. `print_options` is whether the spooler
takes the job's particulars — a page range, a copy count, a named queue —
because CUPS does and a shell `print` verb does not. Which of the three are
set decides who asks the reader what, and the views ask the capability rather
than the operating system:

- With a system dialog, pulpit's own is down to one question — the marks —
  and everything else is asked next, properly, by the desktop. The Print
  button says `Print…` because there is more to answer.
- With a spooler and no dialog, pulpit asks the particulars its spooler will
  honour, because otherwise nobody asks.
- With neither, the dialog says so rather than offering controls that do
  nothing. Where a job names something its spooler cannot do, the adapter
  answers `Unsupported` rather than printing the document whole: forty pages
  when four were asked for is not a partial success, and the reader finds out
  at the printer.

The two must not both ask. A range typed into pulpit's dialog and then a
system dialog opening at "all pages" is a reader overruled by the application
without being told, so where `system_print_dialog` is set `PrintDialog`
carries `asks_particulars: false` and its plan sends no range, no copy count
and no queue at all.

`crates/pulpit/src/platform/cups.rs` is `lp` and `lpstat`, shared by the Linux
and macOS adapters because there is nothing platform-specific about printing
on either. It waits for `lp` to exit rather than spawning it, because the file
it just handed over may be a scratch copy about to be deleted. It is now the
fallback under both system dialogs rather than the only path.

=== The system dialog on each platform

On Linux it is `org.freedesktop.portal.Print`
(`platform/portal_print.rs`): a two-call handshake, `PreparePrint` to put the
dialog up and `Print` to hand over a file descriptor with the token that
stands for what the reader chose. The request's object path is *derived* from
the `handle_token` and subscribed to before `PreparePrint` is called, because
the portal may answer before the call returns and a subscription taken
afterwards loses that race; the returned path is compared against the derived
one rather than trusted, since a mismatch would otherwise be a wait that never
ends. There is no parent window: obtaining one means holding a native handle
across an event-loop turn, which the second rule forbids, and the
specification's answer for that case is the empty string.

On macOS it is `NSPrintOperation` (`platform/appkit_print.rs`), reached
through PDFKit. macOS has no call that shows a print panel for a *file*: the
panel comes attached to an operation, and an operation asks something to draw
the pages. `-[PDFDocument printOperationForPrintInfo:scalingMode:autoRotate:]`
is what makes that something Apple's code rather than ours, so the panel's
paper, duplex, tray, range and copies are applied by PDFKit to PDFKit's
rendering and pulpit still contributes only a file and a job name. The
rejected alternative is worth naming: reading settings back out of an
`NSPrintInfo` and spooling with `lp` is a third of the code, and every setting
nobody remembered to translate becomes a control the reader set and the
printer ignored. A dialog whose choices silently do nothing is worse than no
dialog.

Windows has no system print dialog here and reports none. Its shell `print`
verb takes no options and shows nothing; the dialog `PrintDlgEx` puts up hands
back a device context for the application to draw every page onto, which is
the half of the job this section exists to refuse. Lifting it means taking
that half on for one platform, and it has not been taken.

=== Printing leaves the event loop

A modal dialog the reader is looking at is a call that does not return for as
long as they look at it. Made from the event loop, that would freeze both
windows â the audience's among them â so `spool` hands the job to a thread and
takes the answer back as `PrintMsg::Spooled`. `print_in_flight` is what stops
a second dialog opening behind the first, and it is deliberately *not* cleared
when the document closes: a job already on a thread is out of reach, and
pretending otherwise buys a second dialog.

AppKit is the exception and says so through
`PlatformServices::print_dialog_wants_main_thread`. `runOperation` refuses to
be driven from anywhere but the main thread, so on macOS the call is made in
place and pulpit's own drawing stops until the panel closes. AppKit services
the panel from its own modal run loop, so the panel stays live; what stops is
pulpit. The audience window keeps the last complete frame it had throughout,
which is all the third rule asks of it.

A cancelled dialog is `Outcome::Refused`, and nothing is said to the reader
about it. Reporting a cancel as a failure tells someone their own decision
went wrong.

The document's own `/P` print bits are reported and never quietly obeyed or
quietly ignored. They are a request made by whoever produced the file, to a
viewer that is not obliged to honour them and could not be made to — every
other reader on the machine will print the same file. pulpit says what the
document asked for and makes the reader answer it, which is the one behaviour
that is neither a lie to the reader nor a pretence of enforcement.

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

Text works the same way. `theme::typography` turns the five steps of the type
scale into the roles a view actually wants — a title, a heading, prose, a
field label, a control label, a caption — each already carrying its size,
weight and colour. Sizing text by hand is how a dialog acquires a header
smaller than its own body, or three sizes doing one job across four dialogs,
so the numbers are chosen there and nowhere else. A control label sets no
colour: the button owns its foreground across hover, pressed and disabled,
and a label that painted itself would freeze one of those states over the
other three.

Editable fields share one recipe: surface fill, a quiet one-pixel edge,
4-pixel corners, and a two-pixel accent focus edge. Scrollbars keep a
14-pixel pointer lane and a 48-pixel minimum thumb while drawing only a
5-pixel thumb; the thumb strengthens on hover and uses accent only while
dragged.

== Narrow windows

The presenter window supports a 480-by-600 logical-point minimum. Responsive
behaviour belongs to the contents of a cell, not to a second saved layout:
Iced's `responsive` widget measures the space a control run actually receives,
and actions that do not fit move behind one labelled *More* popover. No action
is discarded. The current page controls and the armed crop or annotation tool
remain on the band, while the popover uses text labels so an action displaced
from its familiar icon can still be found.

Below 760 logical points, a document's outline/search rail becomes a
300-point drawer over the page. It remains below the application toolbar and
keeps the same disclosure animation, but it no longer turns the document into
a narrow strip. Dialogs fill up to their maximum width and scroll vertically;
shortcut help steps from three columns to two and then one. The layout editor
likewise moves its widget library above the canvas below 700 points.

The saved proportional tree is unchanged by all of this. Custom presenter
layouts keep their authored structure; individual widgets may compact their
own controls, and the document sidebar may float, but Pulpit does not invent
or persist alternate trees at breakpoints.

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
application blocks on that listener off the event-loop thread, then
re-enumerates and sends the immutable snapshot as an application message. A
1 Hz fallback runs on the same helper thread only when a backend has no native
listener; enumeration never blocks `update`.

== Targeted fullscreen

On X11 with an EWMH window manager the sequence that works is: leave
fullscreen, `ConfigureWindow` to the target monitor's origin, then
`_NET_WM_STATE_FULLSCREEN`. The request returns `Pending`; three later
event-loop turns re-resolve the native handle and observe where the window
landed after 20, 50 and 100 ms. This replaces a synchronous 170 ms sleep and
keeps the rule that no native handle survives a turn. Every placement returns
an explicit outcome — `Applied`, `Pending`, `Refused`, `Disappeared`,
`Unsupported`, `Failed` — and the UI surfaces the ones the user must act on.
*Refusal is detected by observation, never assumed to be success.*

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
- a *leaf* cell holding at most one widget, plus padding, background, what to
  do when it is empty, and whether either axis fills its authored proportion
  or hugs the widget's functional minimum. Neighbouring cells are separated
  by one muted hairline in the split gutter; cells and widgets do not draw
  perimeter borders.

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

Flexible layout space is stored proportionally, so it scales to any screen
without letterboxing or reflow. A hug cell takes its widget's declared minimum
plus its padding first, and the flexible siblings divide what remains. This is
how a one-row toolbar stays one row in a tall window without baking that
window's dimensions into the file. The editor's aspect-ratio selector changes
only what the canvas previews — never the saved layout. When the presenter
screen's real ratio is far from the design ratio, the presenter window shows
one dismissible notice suggesting a review at that ratio.

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

=== The registry: one authority, one place to add a widget

`crates/pulpit/src/widgets/registry.rs` is the single authority for widget
identity and rendering. `WidgetKind` and `Family` still exist — a closed enum
buys the exhaustiveness check that makes a missing implementation a compile
error rather than a blank pane mid-talk — but nothing outside a widget's own
family module matches on either. Everything a contributor used to have to
hunt down across the crate is one of three tables instead:

- `widgets/catalog.rs` — one `WidgetDefinition` per kind: label, tooltip,
  group, compound parts, placement policy, capabilities, minimum size, and
  what a layout thumbnail should sketch for it (`ThumbnailContent`).
- `widgets/registry.rs`'s `widget_registry!` macro — one line per kind: its
  stable dotted `WidgetId`, its family's `view`, and a `plan` hook (what
  frames it needs rendered, given its cell's share of the window).
- `widgets/mod.rs` — the vocabulary itself: the `WidgetKind` variant,
  `WidgetKind::ALL`, `WidgetKind::family()`, and `WidgetConfig::default_for`.

*Adding a widget* touches five places, all inside `widgets/`: the variant and
its three one-line arms in `widgets/mod.rs`; one `WidgetDefinition` in
`widgets/catalog.rs`; the widget's own module; one arm in its family's
`view.rs` choosing among that family's kinds (the one `match WidgetKind` site
that stays — a family choosing among its own is that family's business, not
the host's); and one line in the `widget_registry!` invocation.
`pulpit.status.blank` (`widgets/status/blank.rs`, `WidgetKind::BlankSpace`) is
a living, minimal example: a static decorative panel with no configuration
and no capabilities, wired through exactly those five touches.

What stays a host service rather than registry data: `layout::validate`
reads `WidgetCapability`, not kinds; `layout::panels` aggregates the
`WidgetPlan`s widgets declare rather than knowing what any one kind is;
`layout::thumbnail` reads `ThumbnailContent` from the catalog rather than
matching kinds itself. A widget declares what it needs and what it looks
like; the host only aggregates.

A source-scan test in `widgets/registry.rs`
(`widgetkind_and_family_matches_stay_where_they_belong`) is the gate: it
walks the crate's source tree at test time and fails the build if a `match`
on `WidgetKind` or `Family` turns up outside the allowlisted registry,
vocabulary and per-family `view.rs` files. A new central dispatch point is
meant to fail loudly, not wait for review to notice.

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

The hamburger remains the one global command menu, and it is kept
deliberately small: opening a document, the ways into the layouts, settings
and speech pages, and the keyboard reference. Every command that has a key —
reloading, the overview, swapping displays, fullscreen, the timer, quitting —
lives on that key and is advertised by the reference rather than by a menu
row that duplicates it; live controls such as Start and Stop remain outside
it too. Its keyboard-reference command opens a presenter-only
overlay generated from the fixed keymap. The mode-neutral no-document surface
uses compact subsets of that same model, so advertised keys cannot drift from
input handling. Hardware aliases from presenter remotes are resolved
separately and do not become visual clutter in the keyboard reference.

Which surface a key belongs to is decided by one ordered list, the key
ladder (`keyladder::LADDER`). Every press descends it top rung first —
typing surfaces, held marks, the overview grid, the captured widget, the
open panels and popups innermost first, the editor pages, a focused media
overlay, the document viewer, reader fullscreen, and finally the keymap —
and the first active rung to consume the press wins. A key reused across
contexts is therefore a vocabulary, not a conflict: a digit arms a slide
tool in presenter mode and a document tool in reader mode because only one
of those rungs is ever active for the press. Escape needs no rules of its
own; it closes the topmost open thing because the innermost surfaces sit on
the highest rungs. The order is behaviour — moving a rung moves who wins a
contested key — so it MUST stay in that one table, with the reasons written
on the rungs, and never re-grow as scattered early returns. A shortcut that
must fire while a text box holds the caret declares it on its action
(`Action::reaches_captured`) and is honoured only under commanding
modifiers, rather than being special-cased at the dispatch site.

Modifiers reach the keymap as roles, not key caps. The event's raw Control
and Command flags are folded by `InputPolicy::split_modifiers` into
_primary_ — Command on macOS, Control elsewhere — and _control_, the
Control key specifically on the one platform where that is a different key;
one press never counts as both. Bindings are written against these roles,
so `primary + Q` is ⌘Q on a Mac without a second keymap, and macOS Ctrl
combinations stay free. Stored keymaps that spell the old conflated flag
load as primary, which is what it always meant in practice.

Two hardware-alias decisions are made knowingly. The volume, media and
browser keys, and `F1`, resolve to navigation and blanking because presenter
remotes emit them and identify themselves no further; a laptop's own volume
keys therefore page the deck while pulpit is focused. The aliases sit on
the ladder's bottom rung, apply only unmodified, and never appear in the
reference. If this trade ever turns out wrong in rooms, the remedy is a
setting that disables `Keymap::resolve_remote`, not per-device guessing.

What the open document _is_ — its properties — is a section of the settings
page rather than a rail view or a dialog of its own: the rail holds per-page
navigation, while this is one question about the whole file, read once,
beside the other facts about the session. The answer is a
`DocumentRequest::Properties` round trip made when the settings page opens
rather than at document open, since a presenter putting a deck on a projector
never asks it.
Every string in it is written by whoever produced the file, so it crosses the
wire through `InfoText`, which bounds it to `MAX_INFO_TEXT_BYTES` and
collapses its control characters and line structure — the same treatment
`AnnotationContents` gives `/Contents`, for the same reason. A key the
document left empty is reported as absent rather than as an empty string, so
the section can leave the row out instead of drawing a blank one, and the page
scan reports `Unmeasured` rather than claiming a uniformity it did not check.

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

The same controls live on the clip itself: a click toggles playback, a
horizontal drag scrubs, and a double-click — or the transport's projection
button — throws the media across the whole slide area on the audience and
presenter screens together, letterboxed on black above everything else.
Escape, or navigating to another slide, puts it back. The projection is
drawing state only: the session keeps its viewport and its playhead, so the
frames are scaled up rather than re-rendered, and nothing restarts.

=== Accent and alert

Accent is the interface's interaction signal, not a decoration: focus,
selection, current/live state, and the Forward action. Timer and clock digits
are ordinary text until a warning or alarm makes them alert-coloured. That
change is intentionally rare, so it remains visible at a glance.

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
  [*Presenter*], [The live-presentation view: current slide at 72% width, with the clock and timer, the next slide and the notes in a rail, and navigation and annotation tools in a band below.],
  [*Reader*], [What a new PDF opens with: the page under a control band carrying the menu, document navigation and the annotation tools, with the outline in a narrow rail that becomes an overlay drawer in a narrow window.],
)

There are two and no more, one per mode, and neither is a variant of the
other. Pulpit does not choose or rearrange a
layout based on the deck or display aspect ratio. Built-ins cannot be renamed,
overwritten or deleted. *Duplicate to Customize* makes an editable copy and
opens it.

== Which layout a file opens into

A new PDF opens in the Reader. Page size and aspect ratio do not participate:
a deck and a report are the same file format, and Pulpit does not guess what
the user intends from their geometry.

A layout chosen by hand while a document is open --- including the *Read or
present* switch --- is recorded in `layout.per_document` against the BLAKE3 of
that file's bytes, and that choice wins on every later open of the same file.
By contents rather than by path, so moving or renaming a document keeps the
choice and two copies of one document agree; `session::fingerprint`, which
deliberately notices an edit in place, answers a different question and is
right for crash recovery rather than for this. Saving copies the entry onto
the file just written, which has the same document in it and different bytes
around it; the source keeps its own, because pulpit never writes over the file
it opened. The list holds 200 files and drops the least recently chosen.


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

Currently pinned: `chromium/7999`. Every packaged target uses the `pdfium-v8-*`
flavour so AcroForm formatting, validation and calculation scripts behave the
same in Nix and in the direct-download bundles. For Linux x86-64 the artifact
is `pdfium-v8-linux-x64.tgz`, SHA-256
`b1098d069e9bc05ba4f2c83156133c82e6eeeb1c979d6e314db60a2582145994`.
The per-target names and hashes live together in `scripts/fetch-pdfium.sh` and
`flake.nix` and MUST move together.

The V8/XFA upstream flavour contains both engines, but pulpit currently sets
`xfa_disabled` and does not call `FPDF_LoadXFA`: V8-backed AcroForm JavaScript
is shipped; XFA field loading remains a separate compatibility decision. This
keeps the document-worker protocol and the security boundary honest instead
of silently treating a bundled engine as an implemented document format.

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

Binding is *lazy*, attempted on the first PDF open rather than at worker
startup (`SPEC-images.md` §45.3, §45.4). This deliberately softens the rule
above by one step: a worker still exits with that diagnostic when it is asked
to open a PDF it cannot render, but it no longer exits before knowing what it
was asked for. Since image directories are documents too, refusing to display
a JPEG because a PDF library is absent is not defensible, and the reasoning
behind the original rule — a deck silently rendering as blanks — does not
apply to a format the worker can fully decode. `pdf::router::RoutingBackend`
is what dispatches per document, and it is the only thing that names both
backends.


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

An image document — a folder, or a `.cbz` / `.cbt` comic archive — is outside
that order entirely: it is pinned to `SlidesOnly`, consults no default and
records no per-document choice (`SPEC-images.md` §46.4, `SPEC-reader-formats.md`
§59.4). A presenter whose default is a `SplitPage` mapping would otherwise have
every photograph cut down the middle with its right half treated as speaker
notes.

= Formats other than PDF

`SPEC-reader-formats.md` is the authoritative account of which formats pulpit
reads, which it refuses, and why each decision was made. The short version:

- **Class A, archives of images** — `.cbz` and `.cbt` are implemented. They
  are pure Rust, add no native dependency, and reuse the image page table
  entirely: an archive is a directory that happens to be one file. `.cb7`
  waits on a maintained pure-Rust 7z decoder; `.cbr` is *not planned*, because
  unrar's licence is not one this project will carry or ship.
- **Class B, paginated formats behind native libraries** — DjVu, XPS,
  PostScript and DVI fit `PdfBackend` as it stands, so the architecture is not
  what blocks them. The cost is a pinned native library on five platforms and
  its CVE surface, forever. If any is ever done it MUST bind an *installed*
  library and report `Unsupported` naming it when absent; PDFium is the single
  bundled exception and it is the reason the application exists.
- **Class C, reflowable formats** — EPUB and the rest have no page count until
  a viewport is chosen, and the presenter and audience windows are different
  sizes, so "page 7" would name different content in each. That is a rule 3
  violation rather than a missing feature. Not planned for the presenter.

Two rules from that document bear on code outside it. A format's absence must
never break another format, which is what `pdf::router::RoutingBackend` and
lazy PDFium binding buy. And a refusal must never be reported as a corrupt
file: "pulpit cannot read this kind of file" and "this file is damaged" are
different facts, and telling a presenter the second when the first is true
sends them looking for a problem that does not exist. The router refuses a
`.cbr` *before* routing, precisely so it cannot fall through to PDFium and
come back as a damaged PDF.

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
