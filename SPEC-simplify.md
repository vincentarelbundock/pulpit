# pulpit simplification specification

Companion to `SPEC-signing.md` and `SPEC-reader-formats.md`. Adds §67–§74.
Section numbers are stable so other documents and source comments may cite
them; gaps are deletions, not omissions.

**Status.** §68.2, §68.4, §69, §70, §71, §72 and §73 were carried out on the
`simplify` branch; each section below records what was done and what the doing
changed about the finding. §68.1, §68.3 and §68.5 are outstanding.

This document is a **findings record**, not a plan. It says what is
superfluous, how that was established, and what the rule is that stops it
coming back. Every claim below was measured against the tree at `45d2121`;
each section states the command that reproduces it, so a claim that has gone
stale can be retired rather than argued about.

The standing rule the whole document serves:

> **A thing is superfluous when deleting it changes no behaviour and loses no
> knowledge.** Behaviour is what the tests and the application exercise.
> Knowledge is what would have to be re-derived. Git remembers deleted code,
> so "we might want it later" is not knowledge — a written reason is.

---

## 67. Method

**§67.1** Four independent passes were run, because each is blind to what the
others find:

- **Reference scan.** Every `pub` item in the five crates, cross-referenced
  against every identifier in the workspace including tests, examples and
  fuzz targets. This is the only pass that sees dead `pub` items in the
  library crates, which `rustc` cannot lint because they are API surface.
- **Lint experiment.** The eleven module-scope `#![allow(dead_code)]`
  attributes were stripped and `cargo check -p pulpit --all-targets` re-run
  against a 0-warning baseline. See §69.
- **Copy-paste detection.** `jscpd`, 265 files, `--min-lines 25
  --min-tokens 120`.
- **Dependency audit.** Every declared dependency in every manifest,
  cross-referenced against `<crate>::` usage in that crate's sources.

**§67.2** The findings were produced against an unmodified tree: the lint
experiment (§69) was reverted before anything was written. The work recorded
in the **Done** paragraphs came afterwards, on the `simplify` branch.

**§67.3** A pass that finds nothing MUST be recorded as such (§74), because
the absence of a finding is itself a result, and re-running a clean pass is
waste.

---

## 68. Deletions that lose nothing

These are established by reference count, not by judgement. Each MAY be
deleted without a design discussion.

**§68.1 — Unreferenced demo video, 85 MB.** *(outstanding)*

```
examples/pulpit_01_navigation.mp4    25 MB
examples/pulpit_02_media.mp4         21 MB
examples/pulpit_03_annotations.mp4   36 MB
examples/pulpit_combined.mp4          6 MB
```

Nothing references these — not `examples/README.md`, not `examples/Makefile`,
not `examples/make-media-assets.sh`, not the site sources. The site uses
`docs-src/assets/tour-presenter.mp4` and `tour-reader.mp4`, which are a later
and separate recording. This is 45% of the 186 MB the repository tracks, and
it is in every clone permanently.

Reproduce: `grep -rl pulpit_01_navigation --exclude-dir=target --exclude-dir=.git .`

**§68.2 — `crates/pulpit/src/vendor/iced_aw/style/colors.rs`, 662 lines.**

The full CSS named-colour table. Declared at `style/mod.rs:4` and referenced
by nothing — not by the application, not by the colour picker it was vendored
alongside, not by its own tests. Alongside it:

- `vendor/iced_aw/core/renderer.rs` (20 lines) — `DrawEnvironment` is
  constructed nowhere.
- `vendor/iced_aw/glyphs.rs` — `up_open` and `down_open` are unused; the
  overlay imports only `cancel` and `ok`.

Together these take the vendored tree from 4,253 lines to about 3,570.

**Done**, but not for the reason first written here, and the difference
matters. `vendor/mod.rs` carries a standing policy *against* deleting unused
upstream code: "deleting them is a diff against upstream to maintain for ever,
and the code is not this crate's to tidy." That policy is right, and it would
have forbidden this deletion.

What makes these three the exception is that they are not leftovers of the
widget that *was* vendored. `iced_aw/README.md` described two widgets — a
colour picker and a time picker — and the time picker was never ported: there
is no `time_picker` module anywhere in the tree and no commit ever deleted
one. The colour table, `core::renderer` and the `up_open`/`down_open` arrows
("the time picker's digital column", in the glyph module's own words) are all
and only what that unported widget would have used. Keeping them preserves a
diff against a port that does not exist.

The README, `iced_aw/mod.rs` and `vendor/mod.rs` were corrected to say one
widget rather than two, and the exception is written into `vendor/mod.rs` so
the policy still reads correctly for everything else.

**§68.3 — `docs-src/.calepin/`, 41 tracked files, 264 KB.** *(outstanding)*

`.gitignore:5` is `.calepin/` and `Makefile:282` (`make clean`) runs
`rm -rf docs-src/.calepin`. The directory is therefore both ignored by policy
and destroyed by a routine target, yet 41 of its files are tracked — including
build-cache state (`index/fingerprint.xxh3`, `index/results.json`,
`index/dependencies-html.json`, `index/page-meta.json`). They were force-added
at `0a094a0`.

This is worse than dead weight: a tracked cache is a stale cache, and the
first `make clean && git status` after a website build shows 41 spurious
deletions.

**§68.4 — Four unused dependency declarations.**

| Dependency | Manifest | Evidence |
|---|---|---|
| `pkcs12` | `crates/pulpit-render/Cargo.toml:64` | No `pkcs12::` path anywhere; the `p12-keystore` feature superseded it |
| `resvg = "0.45"` | `crates/pulpit/Cargo.toml:67` | No `resvg::`, `usvg::` or `tiny_skia::` path anywhere |
| `anyhow` | `[workspace.dependencies]` | Declared by no member crate |
| `crc32fast` | `[workspace.dependencies]` | Declared by no member crate |

**Done.** All four removed. What that is worth was measured afterwards, and
it is not what this section first claimed:

- `pkcs12 = "0.1"` was the one that cost anything. `p12-keystore` depends on
  `pkcs12 0.2.0-pre.0`, so the direct `0.1` declaration put a **second,
  redundant copy** of the crate in the build. `Cargo.lock` goes from 846
  packages to 845.
- **`resvg` cost nothing**, contrary to the sentence that used to stand here.
  `iced_tiny_skia` and `iced_wgpu` both depend on `resvg 0.45.1`, so the
  direct declaration resolved to a copy already in the graph; `typst-render`
  pulls `0.47.0` besides. Removing it changes the manifest, not the build.
- `anyhow` and `crc32fast` were `[workspace.dependencies]` entries no member
  ever referenced, so they were never compiled on pulpit's account at all —
  though both are in the graph transitively, via `rav1e` and `flate2`.

The lesson is the one §67.3 asks for: a dependency audit that greps for
`<crate>::` finds unused *declarations*, which is not the same as unused
*compilation*. `cargo tree -i <crate>` is the check that tells them apart, and
it MUST be run before a removal is described as a saving.

**§68.5 — `crates/pulpit-media/examples/cast-bench.rs`.** *(outstanding)*

Referenced by nothing. The other four examples (`warm-bench`, `thumb-bench`,
`dump-corpus`, `say`) are each cited from a manifest, a sibling example or a
source comment; this one is not.

---

## 69. The blanket dead-code allowances

**§69.1** Eleven files carry a module-scope `#![allow(dead_code)]`. Five of
them are `mod.rs`, so the lint is disabled for **all of `layout/`,
`settings/`, `doc/`, `media/` and `platform/`**:

```
crates/pulpit/src/layout/mod.rs:15        crates/pulpit/src/doc/mod.rs:10
crates/pulpit/src/settings/mod.rs:12      crates/pulpit/src/media/mod.rs:13
crates/pulpit/src/platform/mod.rs:26
crates/pulpit/src/widgets/patch.rs:15
crates/pulpit/src/widgets/{common,slides,timing,navigation,notes}/model.rs:1
```

**§69.2** `cargo check --workspace --all-targets` is clean at 0 warnings.
Strip those eleven attributes and the binary alone produces **79 dead-code
warnings**, concentrated as follows. (A warning names one item or several, so
the warning count is not the item count; the exact figure is 132 items, and
§69.5 is where that number is put to work.)

```
12  layout/tree.rs             8  settings/keys.rs        7  widgets/timing/model.rs
 5  widgets/patch.rs           4  widgets/notes/model.rs  4  layout/model.rs
 3  layout/store.rs, platform/{services,null,input}.rs, widgets/navigation/model.rs,
    settings/diagnostics.rs
```

Whole types, not stray helpers:

- `platform::services::{Urgency, Notification}`, plus `notify` and
  `recent_documents` — the desktop-notification surface is entirely unused.
- `platform::null::NullPlatform` is never constructed, though `platform/mod.rs`
  describes it as the thing that lets the application be tested with no
  desktop at all.
- `LayoutStore::{directory, save_as_new, rename, import, export, saved_version}`
  — the layout persistence API is roughly half unreached.
- `Keymap::{resolve, bind, unbind, restore_missing_defaults, keys_for}` and
  `UNBOUND_BY_DEFAULT`, in an 1,822-line `settings/keys.rs`.
- `DiagnosticsBundle::{events, to_report, to_full_report}`.

**§69.3** The comment above each allowance is the same paragraph: these
modules were standalone library crates before the workspace was consolidated,
and they keep their complete, tested APIs. That is a defensible reason to
retain a given item. It is **not** a defensible instrument, because a
module-scope allowance also hides every *future* dead item in five
subsystems, permanently and silently.

**§69.4 — Rule.** A dead-code allowance MUST be attached to the item it
excuses and MUST carry the reason, as
`#[allow(dead_code)] // <why this is kept>`. `#![allow(dead_code)]` at module
scope is forbidden. The codebase already does this correctly:
`widgets/event.rs`, `widgets/context.rs` and `reader_link.rs` each name the
reader that justifies the item, and those are the pattern.

The one standing exception is `vendor/mod.rs`, which silences the whole
vendored `iced_aw` tree. That is deliberate and stays: the code there is
upstream's, and a per-item diff against it is a diff to maintain for ever
(§68.2 covers the narrow case that is not).

**§69.5 — What was done, and what it established.**

All eleven module-scope attributes were removed and replaced with 132 per-item
allowances, each carrying one of two reasons. Choosing between the two is what
made the exercise worth doing.

The paragraph above every one of those eleven attributes gave the same
justification: these modules were standalone crates before the workspace was
consolidated, and "the parts the application does not happen to call yet are
exercised by the tests beside them." That claim is checkable, and `rustc`
checks it for free — an item reached from a `#[cfg(test)]` module warns in the
`--bins` build but not in the `--tests` build, so the difference between the
two warning sets is exactly the set of items the claim is true for:

```sh
cargo check -p pulpit --bins  --message-format=json   # 132 dead item spans
cargo check -p pulpit --tests --message-format=json   #  50 dead item spans
```

**The claim holds for 82 items and fails for 50.** Those 50 are reached by
nothing at all — not the application, not their own tests, not an integration
test. They carry `// unreached, including by its own tests`; the other 82
carry `// reached by its tests, not by the application`. Both reasons are
true, which the blanket form's single reason was not.

`platform::services` is the clearest case: `Urgency`, `Notification`, `notify`
and `recent_documents` — the entire desktop-notification surface — are in the
unreached 50. So are `LayoutStore::{directory, save_as_new, saved_version}`,
`Instance::AlreadyRunning`'s `lock` field, and `Logging::log_directory`.

**§69.6** These 50 were **not** deleted, deliberately. §69 is a visibility
change, and deleting is a separate decision with a separate risk: an early
attempt at it here removed `SettingsStore::save` and fifteen other items that
`--tests` had reported as unreached, and the build broke, because a warning
set captured while the tree was mid-edit is not a fact about the tree. The
50 are now individually marked and individually greppable:

```sh
grep -rn "unreached, including by its own tests" crates/pulpit/src
```

Deleting them is a later pass, item by item, against a build that is green
first. What §69 guarantees is that a *new* dead item now warns instead of
disappearing into a module-wide silence — which is the whole complaint of
§69.3, and it is fixed.

---

## 70. The widget patch layer

**§70.1** `crates/pulpit/src/widgets/patch.rs` (233 lines) and the `*Patch`
enums with their `apply` methods in `widgets/{timing,notes,navigation,slides,
annotations}/model.rs` — about 400 lines in total — are unreachable. Its own
header says so: *"Nothing sends a patch today: the designer has no properties
panel, and every presentation property is a token."*

**§70.2** The header also states the case for keeping it: the rules it encodes
— which patch applies to which family, what is bounded, what is refused — are
the hard part, and re-deriving them would be a worse trade than carrying them.

**§70.3** That reasoning is sound as far as it goes, but it is the
`git-remembers-it` argument in disguise, and the rule in the preamble decides
against it: the knowledge is in the file, the file is in the history, and the
day a properties panel is built the file can be restored in one command. The
cost of carrying it is not the 400 lines; it is that the patch layer is the
largest single reason `widgets/*/model.rs` needed blanket allowances at all.

**§70.4 — Deleted.** The layer is gone: `widgets/patch.rs`, the `WidgetPatch`
and `PatchError` types, and the `ButtonsPatch`, `AnnotationPatch`, `NotesPatch`,
`SlidePatch`, `TimerPatch` and `ClockPatch` enums with their `apply` methods —
about 400 lines across six files.

It last exists in full at **`45d2121`**, and
`git show 45d2121:crates/pulpit/src/widgets/patch.rs` restores the hard part.
The condition for bringing it back is the one its own header named: a
properties panel in the designer that has something to send.

Three tests used a patch as a convenient way to set a field. They now set the
field through `Widget::config_mut`, which is what the production code
(`layout/builtin.rs`) already did, and they assert exactly what they asserted
before: `widgets/navigation/model.rs`, `layout/validate.rs`, and two cases in
`widgets/annotations/model.rs`. A fourth —
`a_patch_changes_one_thing_and_leaves_the_rest_alone` — tested the patch
mechanism itself and went with it.

---

## 71. Dead `pub` items the compiler cannot see

**§71.1** `pub` items in the four library crates are API surface, so `rustc`
never lints them. The reference scan (§67.1) found 28 with no caller anywhere
in the workspace, tests included:

```
pulpit-core      annotate::hit::crossed_by
                 annotation::{is_bounded, makes_an_annotation, MAX_HISTORY}
                 history::forward_depth
                 notes::{top_half, bottom_half}
                 state::next_preview_source
pulpit-display   reconcile::{windowed_on, fullscreen_on}
                 roles::is_explicit
pulpit-render    cache::set_budget
                 document::issued_ids
                 document::memory::with_pages
                 document::model::is_date
                 pdfwrite::PreparedByteRange
                 sign::CmsSignatureInfo
                 verify::SignatureDiscovery
                 supervisor::note_cache_stats
pulpit-media     supervisor::{set_focus, web_command, runtime_of, overlay_of}
                 diagnostics::from_probe
                 runtime::chromium::child_id
```

**§71.2 — Deleted, on a narrower basis than §71.1 claimed.** All 28 are gone.

But §71.1's basis — "no caller anywhere in the workspace" — is weaker than it
reads, and this must be written down before the next pass leans on it. All
five crates are published to crates.io (`.github/workflows/publish-crates.yml`
publishes `pulpit-core`, `pulpit-display`, `pulpit-render`, `pulpit-media`,
`pulpit` bottom-up), so a `pub` item in a library crate is API surface for
someone outside this repository, and "pulpit does not call it" is not the same
as "nothing calls it."

The deletions stand because the workspace is at **0.0.10**: pre-1.0, no
stability promise, and the public API of these crates is in practice defined
by what the application needs. Everything removed was an unused convenience —
`Region::{top_half, bottom_half}`, `WindowState::{windowed_on, fullscreen_on}`,
`RoleTarget::is_explicit`, `FrameCache::set_budget`,
`RuntimeSummary::from_probe` — or a struct nothing ever constructed
(`PreparedByteRange`, `CmsSignatureInfo`, `SignatureDiscovery`). None is
documented as a feature; none is hard to restore. **Once the workspace reaches
1.0, this section's method stops being sufficient on its own** and a removal
needs a deprecation cycle instead.

**§71.3 — Two findings the deletion corrected.**

*`reconcile::{windowed_on, fullscreen_on}`.* The hypothesis first written
here — that the desired-state vocabulary had drifted past what `reconcile()`
consumes — was wrong. Every construction of `WindowState`, in production and
in `tests/topology_script.rs` alike, uses struct-literal syntax; all four
fields are `pub`. The two functions were plain unused conveniences and nothing
about the state vocabulary is missing.

*`RenderSupervisor::note_cache_stats`.* Deleting it leaves
`RenderCounters::{cache, cache_budget_bytes}` written by nothing, so the
supervisor's diagnostics line permanently reads `cache: not reported`. That
was already true before the deletion — nothing ever called the setter — and it
is not a regression, but it names a real duplicate: the application reports
frame-cache statistics itself, in `app.rs`, under its own `## Frame cache`
heading, straight from `self.cache`. The supervisor's cache-reporting path is
tested but unfeedable and should be retired with its two fields in a later
pass.

**§71.4 — The `#[cfg(test)]` move in the original §71.3 was wrong and was not
made.** All four items —
`document/protocol.rs::KeyModifiers::SHIFT`, `sign::sign_document_file_with_tamper`,
`sign::credential_from_parts`, `RenderSupervisor::pump_blocking` — are reached
from `crates/pulpit-render/tests/`, which links the crate as an external
consumer. `#[cfg(test)]` does not apply to integration tests and would have
broken the build. They are public test-support API, not dead code, and they
stay. `sign_document_file_with_tamper` already carries `#[doc(hidden)]` and a
doc comment saying why it exists; that is the right treatment for this class,
and the pattern to follow.

---

## 72. Structural overlap, worth measuring before acting

**§72.1** `crates/pulpit-render/src/shm.rs` (301 lines) and
`crates/pulpit-media/src/surface.rs` (380 lines) are parallel shared-memory
wrappers. They are **not** duplicates: one is a single resizable region, the
other a ring of fixed slots with hold/release. The genuine overlap is
`RegionNamer`/`RingNamer` and the create–attach–mmap–unlink core, perhaps 100
lines.

**§72.2** Both already delegate the naming and path-safety policy to
`pulpit_core::ipc::shm`, which is exactly the arrangement the `ipc` note in
`CLAUDE.md` describes — the shared part is shared, and the two crates cannot
disagree about where regions live. Consolidating further would move the
mapping mechanics into `ipc` as well.

**§72.3 — Investigated. The consolidation is still not advisable; the
investigation found something else, and that was fixed.**

Reading the two side by side, the shared shape is thin and the divergence is
not. `SurfaceRing::create` and `SharedRegion::create` open a file in the same
world-writable directory — `/dev/shm` is `drwxrwxrwt` — and did so on
different terms:

| | `pulpit-render` (`SharedRegion`) | `pulpit-media` (`SurfaceRing`), before |
|---|---|---|
| Name | `pulpit-<pid>-<64-bit hash>`, seeded from `RandomState` | `pulpit-media-<pid>-<n>`, `n` counting from 0 |
| Create | `create_new(true)` — refuses an existing file | `create(true).truncate(true)` — adopts one |
| Mode | `mode(0o600)` | none, so the process umask, typically `0644` |

The render side's namer explains itself: the hash "gives a name an attacker
cannot predict from the pid alone." The media side had no such reasoning and
none of the three protections. A local user can read `/proc` for the pid,
compute `/dev/shm/pulpit-media-<pid>-0`, and either read the ring — decoded
video frames of whatever is on the audience screen — or pre-create the file so
the worker adopts and truncates theirs. The sticky bit does not help: it
prevents deleting somebody else's file, not creating one at a free name.

`SurfaceRing::create` now uses `create_new(true)` and `mode(0o600)`, and
`RingNamer::next_name` now hashes the ticket, the clock and the pid exactly as
`RegionNamer::next` does. The ring name reaches the worker in
`SessionSpec.ring_name` and nothing derives meaning from its shape beyond the
prefix the sweep reads, so the change is contained.

**§72.4** The mapping code itself was left alone, as §72.3 originally
concluded. The remaining true duplicate is `path_for`, six lines mirrored in
both crates, and it cannot move to `ipc` as it stands: it wraps
`ipc::shm::path_for`'s `Option` into each crate's own `ProtocolError`, and
`ipc` cannot know both error types. Not worth changing.

**§72.5 — Rule.** Two implementations of the same OS-level object MUST agree
on their safety properties — creation mode, name predictability, and whether
an existing file is adopted or refused — even when their data structures
differ and no code is shared. A divergence there is a bug in the weaker one,
not a style difference, and this section is why: it went unnoticed precisely
because the two were filed as "similar but deliberately separate."

---

## 73. Documentation drift

**§73.1** `CLAUDE.md` describes `pulpit-media` as driven "by an installed
Chromium-family browser over CDP". `crates/pulpit-media/src/worker/mpv.rs`
(889 lines) is a live second runtime, reached through `probe_libmpv` and
selected by `RuntimeId::LibMpv`. The code is correct; the sentence is stale.

**§73.2** `crates/pulpit-core/src/annotation.rs` opens by saying there is one
representation now, and marks are no longer temporary; `annotate/mod.rs` opens
by describing `annotation::Annotations` as holding "transient marks that
vanish with the slide". Both modules are live (146 and 162 references) and the
split is deliberate, but the two headers disagree about what the first one
holds. One of them is describing a state the code has left.

**§73.3 — A third drift, found while fixing the first.**
`pulpit-media::selection::default_order`'s doc comment said "An installed
Chromium-family browser plays all three content kinds, so it leads every
order." Its body does not: `AnimatedImage` and `Video` both put
`RuntimeId::LibMpv` first, and only `Web` leads with Chromium. The comment
described the code as it was before libmpv existed.

**§73.4 — Fixed.** `CLAUDE.md` now names both runtimes; `default_order`'s
comment now matches its body; and the `pulpit-media` entry in
`[workspace.dependencies]`, which justified default features with "the browser
worker is the only media runtime," now says what is actually true — that
`chromium-runtime` is the fallback every content kind can be played by, since
libmpv covers plain media but not interactive overlays.

§73.2's disagreement between the two annotation module headers is **not**
fixed. Both modules are live and the split is deliberate; which header is
stale is a question for whoever knows which of the two descriptions the code
is meant to satisfy, and guessing would replace a visible contradiction with
an invisible one.

**§73.5** Drift of this kind is superfluous in the same sense as dead code: it
is text that must be read and discounted. It SHOULD be fixed in the same pass
as the code it describes.

---

## 74. What was checked and found clean

Recorded so these passes are not re-run.

**§74.1 — Copy-paste.** `jscpd` over 265 files reports **0.04% duplication**:
two clones of about 35 lines each, among `residency.rs`,
`widgets/common/popover.rs` and `widgets/panel.rs`. The codebase is not
copy-pasted, and a DRY sweep would find nothing.

**§74.2 — Presentation layers.** `view.rs` and `layout_renderer.rs` are one
layer, not two competing ones: `view.rs` delegates at four call sites.

**§74.3 — Annotation modules.** `core::annotation` and `core::annotate` are
both live and deliberately split per `SPEC-document.md` §5.3. See §73.2 for
the header, not the code.

**§74.4 — The mpv worker.** Live. See §73.1.

**§74.5 — Feature flags.** All six (`pdfium`, `djvu`, `p12-keystore`,
`chromium-runtime`, `x11`, `wayland`) are referenced from source. None is
vestigial.

**§74.6 — Build warnings.** `cargo check --workspace --all-targets` is clean
at 0 warnings, before the §69 experiment and again after all the work above.
`make lint` — `cargo fmt --check` plus clippy with warnings denied — passes.
