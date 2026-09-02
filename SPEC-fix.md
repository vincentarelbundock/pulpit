# pulpit fix specification

Companion to `SPEC-signing.md`, `SPEC-reader-formats.md` and the retired
`SPEC-simplify.md`. Adds §75–§84. Section numbers are stable so source
comments and commits may cite them; gaps are deletions, not omissions.

**Status.** Nothing in this document has been carried out. Every section is
outstanding. When a section is done, replace its *(outstanding)* marker with
a **Done** paragraph saying what changed and what the doing changed about the
finding, as `SPEC-simplify.md` did.

This document is a **findings record** from a review of the whole workspace
against the tree at `00d40f6`, judged on four qualities in priority order:
robustness, simplicity, DRY, efficiency. Every finding names the file and line
where it was verified so a claim that has gone stale can be retired rather
than argued about. Findings are grouped by what to do about them, not by
crate.

The standing observation the whole document serves:

> **The pure cores are sound; the defects live at seams.** `reconcile()`,
> `ipc`, the annotation and gesture geometry, the frame cache, the xref
> resolver and the CMS checker each held up. Nearly every defect below is a
> place where two halves were written to a contract and one half stopped
> honouring it: the journal and the undo direction, the media supervisor and
> its workers, the render supervisor's restart and the app, PDF verify and
> PDF sign, the document manager's promotion state and the app. None of
> those seams has a test that crosses it.

---

## 75. Method

**§75.1** The workspace was read in full, not sampled: 174 290 lines across
five crates, split into ten areas, each read file by file with call sites
traced before any claim was written. `crates/pulpit/src/vendor/` was excluded.

**§75.2** Copy-paste detection: `jscpd crates --format rust --min-tokens 70
--min-lines 8 --ignore '**/vendor/**,**/tests/**'`. Result: 56 clones,
664 lines, 0.5 % of tokens. Literal duplication is not the problem; every
clone the detector found is cited below as a symptom of a structural one.

**§75.3** Convention checks, each reproducible with one `grep`:

- Clock reads in the pure crates (`Instant::now|SystemTime::now` in
  `pulpit-core/src` outside `ipc/`, and in `pulpit-display/src`): three hits,
  all inside `#[cfg(test)]`. Clean.
- `ipc` referenced outside `pulpit-core/src/ipc/`: only the `mod` line in
  `lib.rs`. Clean.
- `cfg(target_os)` above `pulpit::platform`: ten sites in
  `crates/pulpit/src/display.rs` (§77.9). Hits in `pulpit-media` and
  `pulpit-render` are discovery tables for installed binaries and libraries,
  which are process-boundary facts, and are not violations.
- Operations leaving the process returning `bool`: none. The `-> bool`
  functions in `platform/` are properties (`can_speak`, `is_held`,
  `available`), not operations.

**§75.4** Every finding marked **verified** below was confirmed by a second
reading of the cited lines after the area review reported it. Findings about
macOS and Windows adapters are from reading, not hardware, consistent with
the repository's own note that those adapters never ran on real displays.

**§75.5** A pass that finds nothing MUST be recorded (§84).

---

## 76. Defects that MUST be fixed before the next release

Each is a wrong outcome reachable from ordinary use or from a document the
user did not author. Each names the test that would have caught it, because
the fix is not complete until that test exists.

**§76.1 — The audience window never learns its scale factor.** *(outstanding,
verified)*

`Message::Resized` stores iced's logical `Size`; `audience_width()`
(`crates/pulpit/src/app.rs:15892`) casts it straight to pixels, and
`audience_scale()` (`app.rs:16935`) returns a hard-coded `1.0`. Only the
presenter asks `window::scale_factor` (`app.rs:3511`, `15948`). On a 2×
projector the "exact output-sized" audience frame is rendered at half
resolution and upscaled, which is the case the project exists to get right.

Fix: add `Message::AudienceScale(f32)`, request it on `WindowOpened{Audience}`
and on every audience `Resized`, store it, and multiply in `audience_width()`
and `audience_scale()` exactly as `presenter_scale_factor()` does.
Test: a scripted window event sequence with scale 2.0 MUST produce a
`RenderJob` whose width is twice the logical width.

**§76.2 — Crash recovery replays the redo of every undo.** *(outstanding)*

For `Ask::Undo` the worker's `Applied.undo` is the inverse of the undo, i.e.
the redo (`pulpit-render/src/document/mod.rs:1117`); `ReaderSession::applied`
correctly pushes it on `redo_stack`. The journal writes that same value as
`JournalEntry::Reversed` (`app.rs:7629`), and `restore_reader_edits`
(`app.rs:10449`) replays it with `Ask::Undo { operation }`, re-applying the
inverse of the undo. An edit the user undid comes back after a crash.
`PendingEdit.reversal` (`app.rs:9829`) already holds the operation that was
sent; journal that.
Test: the existing `undos_are_recorded_like_anything_else_so_replay_leaves_them_undone`
checks entry shape only. It MUST become an end-to-end assertion against a
fixture-backend document: draw, undo, recover, assert the mark is absent.

**§76.3 — Journalling stops silently after the first Save As.** *(outstanding)*

`Told::Saved` calls `journal.finish()` (`app.rs:7825`), which drops the file
and deletes it but leaves `self.reader_journal = Some(_)`. Every later
`append` hits `let Some(file) = self.file.as_mut() else { return Ok(()) }`
(`reader_journal.rs:329`) and reports success; `is_full()` stays false so
nothing warns. All edits after a save are unprotected for the rest of the
session. Fix: `finish()` re-creates the file and header (the source
fingerprint is unchanged because pulpit only writes copies), and `append` on
a journal with no file is an `Err`.
Test: save, edit, recover; the post-save edit MUST be replayed.

**§76.4 — A PDF link URI with a multibyte character panics the render
worker.** *(outstanding, verified)*

`strip_prefix_ignore_ascii_case` (`pulpit-core/src/overlay.rs:421`) does
`value[..prefix.len()]`, `str` indexing at a byte offset. `is_media_uri` is
called on every link URI from `pulpit-render/src/pdf/overlays.rs:39`, so
`https://école.fr` (byte 9 inside `é`) panics on document description; the
supervisor restarts the worker, which re-describes and panics again. Same
hazard in `decode` (`overlay.rs:689`, `&value[index+1..index+3]`).
Fix: compare `value.as_bytes().get(..n)` with `prefix.as_bytes()` via
`eq_ignore_ascii_case`; in `decode`, take the two hex bytes from the byte
slice through `std::str::from_utf8(..).ok()`.
Test: `is_media_uri("https://école.fr")` and `decode("%aé")` MUST return
without panicking. Rule: §83.2.

**§76.5 — Signing allocates new object numbers from `/Size` alone.**
*(outstanding, verified)*

`IncrementalWriter::next_object_number` (`pulpit-render/src/pdfwrite/mod.rs:714`)
is `trailer_dict.size.max(1)`; `parse_trailer` (`mod.rs:1126`) leaves `size`
at 0 when `/Size` is missing or unparseable, so allocation starts at 1.
`sign/apply.rs:666` uses it directly. A `/Size` that is too small, which is
ordinary real-world damage the lenient `ObjectResolver` tolerates on the read
side, makes the signature dictionary, appearance XObject or new field
overwrite a live page or content stream in the signed output.
`verification_gate` checks only the source prefix and the signatures, so the
result is promoted.
Fix: in `assemble_revision` and `pdfoutline.rs:57` start at
`max(next_object_number(), highest xref entry + 1)`; `IncrementalWriter::open`
MUST refuse `size == 0`.
Test: a fixture with `/Size 3` and objects up to 40 MUST sign without any
existing object number being reused.

**§76.6 — Preflight ignores field inheritance, so certification fails
open.** *(outstanding)*

`resolver_and_fields` (`pulpit-render/src/verify/preflight.rs:115`) discards
the `FieldEntry { field_type, qualified_name, value_ref }` that
`find_field_tree_with` computed with inheritance and keeps object numbers;
`extract_field_info` (`preflight.rs:269`) re-tokenises each node and returns
`None` when the node has no `/FT` of its own. A `/Sig` whose `/FT` is on the
parent is invisible to `preflight_certify`, so certifying over a signed
document is permitted, and its FieldMDP lock is never read. Names are
per-node, so `/Include`/`/Exclude` locks on `form.sig2` never match `sig2`.
`verify::discover_signatures` handles both correctly; the two halves disagree.
Fix: return `Vec<FieldEntry>` and build `FieldInfo` from it; only
`has_seed_value` needs a further `resolver.dict_get`.
Test: a hierarchical field fixture MUST be refused by `preflight_certify`
and its lock MUST be honoured by `check_field_mdp_locks`. Rule: §78.

**§76.7 — A render-worker restart replays `Open`, and the replayed `Opened`
closes the live document.** *(outstanding)*

`kill()` (`pulpit-render/src/supervisor.rs:1183`) re-sends `Request::Open`
for every document; `handle()` forwards `Response::Opened` whenever
`index == first_alive()`. When worker 0 crashes, its replacement is worker 0,
so the app receives a second `RenderEvent::Opened` for the promoted document.
`app.rs:6308` re-runs notes-mapping detection, and `doc/manager.rs`
`on_candidate_opened` in `State::Idle` returns `DiscardCandidate{active}` →
`supervisor.close(active)` → every later render fails.
Fix: track documents whose `Opened`/`OpenFailed` has been reported and
suppress replayed answers. Test: `a_crashing_worker_is_restarted_and_rendering_resumes`
stops at the supervisor; it MUST be extended through `DocumentManager`.

**§76.8 — The media supervisor never closes a session it gives up on.**
*(outstanding)*

`recover` (`pulpit-media/src/supervisor.rs:992`) removes the `Session`, drops
its ring, and relaunches or falls back, but never sends
`MediaRequest::Close { session }` to the worker that still hosts it. The
chromium worker replies `Failed { session: Some(id) }` for `SetActive`/`Input`
errors and keeps the session (`worker/chromium.rs:1397`, `1415`), so the page
keeps screencasting and the worker keeps base64+JPEG-decoding every frame into
a permanently full ring. The 10 s `first_frame_deadline` path against a live
worker does the same. `worker/chromium.rs:1375` compounds it: `Open` for an
id already in `sessions` replaces it without `release_page`, leaking a tab
per transient restart.
Fix: `recover` sends `Close` before relaunch; the worker contract is made
explicit — a per-session `Failed` means the session is gone, or it is a
`MediaEvent::Warning`, never both; `Open` on a live id releases the old page.
Test: a scripted worker that answers `Failed` MUST receive `Close`.

**§76.9 — Bundle extraction bounds expansion by the declared size.**
*(outstanding, verified)*

`extract_bundle` (`pulpit-render/src/pdf/overlays.rs:187`) adds `declared` to
`expanded` and computes the ratio from it, then reads up to
`max_file_bytes + 1` real bytes. Neither the real `bytes.len()` nor the real
ratio ever feeds the total. An archive declaring 0 for every entry can write
`max_files × max_file_bytes` (2000 × 32 MiB) before any cap fires.
Fix: accumulate `bytes.len()` into `expanded` after the read and compute the
ratio from it. Test: an archive whose headers declare 0 bytes MUST be refused
once real expansion exceeds `max_expanded_bytes`. Rule: §83.3.

**§76.10 — The PDFium form-fill environment is never exited.** *(outstanding,
verified)*

`PdfiumDocument::close` (`pulpit-render/src/document/pdfium.rs:1168`) is the
only place `FPDFDOC_ExitFormFillEnvironment`, `release_form_page` and
`release_text_page` run, and it has no callers. The document worker sets
`self.document = None` (`document/worker.rs:73`), freeing the
`Box<FormEnvironment>` PDFium still points into; held pages leak; and
`PdfiumBackend::drop` later calls `FPDF_CloseDocument` with a live form
environment, which PDFium's header forbids.
Fix: `impl Drop for PdfiumDocument` doing what `close` does; delete `close`.

**§76.11 — Debug scaffolding on the annotation write path.** *(outstanding,
verified)*

`write_note_appearance` (`document/pdfium.rs:2082`) makes an extra
`FPDFAnnot_GetAP` round trip per note to feed `eprintln!("DEBUG setap …")`;
`create` (`document/pdfium.rs:3660`) reads `std::env::var("PULPIT_DEBUG_NOGEN")`
per annotation to skip `FPDFPage_GenerateContent`, an undocumented switch
that writes invisible annotations. Remove both.

**§76.12 — A malformed layout file panics at startup.** *(outstanding,
verified)*

`validate()` (`crates/pulpit/src/layout/validate.rs:376`) records a Blocking
"not canonical" issue and continues into `layout.compute`, where
`layout_node` (`layout/tree.rs:613`, `625`) indexes `split.sizes[*index]` per
child. `LayoutStore::load` (`layout/store.rs:136`) imports every file in the
layouts directory on launch, so one hand-edited `*.json` with a short `sizes`
array crashes pulpit until the user finds it.
Fix: return from `validate` once any Blocking structural issue exists;
`sizes.get(i).copied().unwrap_or(0.0)` as defence.
Test: a layout with `sizes.len() != children.len()` MUST load as invalid.

**§76.13 — The Clock widget's timezone is a cached `date +%z` subprocess.**
*(outstanding, verified)*

`prime_local_offset` (`crates/pulpit/src/view.rs:4301`) spawns `date` into a
`OnceLock`. On Windows `date` is a shell builtin, so `output()` fails, the
offset is 0, and every clock and alarm is in UTC. The offset never refreshes,
so a DST change during a session leaves the clock wrong until restart. A view
module owning a clock read is also the wrong layer. `chrono` with the `clock`
feature is already a dependency (`datefield.rs`).
Fix: `chrono::Local::now().num_seconds_from_midnight()` computed in `App`
and passed to the view.

**§76.14 — Erasing a not-yet-named stroke orphans it and misattributes the
next id.** *(outstanding)*

`erase_at` (`pulpit-core/src/annotation.rs:1003`) drops a stroke whose `id`
is `None` from the view and records nothing; the comment says the caller
sends the delete when the name arrives, but no caller does (`app.rs:13704`
drains only `take_erased`). The stroke stays in the file. `name_stroke`
(`annotation.rs:1115`) then assigns the arriving id to the *first* unnamed
stroke: if the presenter drew B after erasing in-flight A, B gets A's id, and
a later erase of B deletes A from the document while B stays forever.
Fix: `erase_at` records erased-unnamed strokes in a pending queue that
`name_stroke` consumes first, turning the arriving id into an immediate
`Delete` for the caller to send.
Test: draw A, erase A before its answer, draw B, deliver both answers; the
document MUST contain exactly B under B's id.

**§76.15 — Cross-reference stream entries are collected without a cap, and
the repair scan is quadratic.** *(outstanding)*

`parse_xref_stream_section` (`pulpit-render/src/verify/objects.rs:955`)
pushes every `(u32, XrefEntry)` before `MAX_XREF_ENTRIES` is consulted in
`XrefIndex::build`. A one-byte `/W [0 1 0]` row and an `/Index` of 32 pairs
each `count = 2_000_000` yields 64 M pushes (~2 GiB) from a few hundred KB of
deflated zeros. Separately, `scan_object_definitions` (`objects.rs:1307`) and
`parse_dict_or_stream` (`objects.rs:1503`) fall back to
`find_bytes_from(.., end_of_file)` per failing object, so a file of repeated
`N 0 obj }` costs O(n²) byte compares, reachable from document open.
Fix: enforce the cap inside both section parsers' push loops; memoise the
"no `endobj`/`endstream` after offset X" result.
Test: a hostile xref stream MUST be refused within the cap; a 10 MB file of
unterminated objects MUST open or refuse in bounded time. Rule: §83.3.

---

## 77. Defects that SHOULD be fixed

Wrong outcomes that are rarer, platform-specific, or degrade rather than
break. None threatens the three rules directly.

**§77.1 — Display adapters that never ran on hardware.** *(outstanding)*

- macOS `place()` (`pulpit-display/src/macos.rs:416`) returns `Applied` for
  an asynchronous `toggleFullScreen:` sequence it cannot know succeeded;
  AppKit restores its saved frame at the end of the exit transition. The X11
  adapter already models this as `Pending` + `verify_placement`; macOS SHOULD
  do the same.
- Windows (`windows.rs:313`) divides each monitor's physical origin by its
  own DPI, producing an incoherent logical desktop under mixed DPI: spurious
  `OverlappingOutputs` every reconcile and wrong `Geometric` tie-breaks.
  Keep geometry in the one space Win32 gives (physical, which `place()`
  already uses).
- Windows (`windows.rs:609`) toggles `WS_EX_TOPMOST` through
  `SetWindowLongPtrW`, which has no effect; only `SetWindowPos` with
  `HWND_TOPMOST`/`HWND_NOTOPMOST` changes it. `saved_styles` (`windows.rs:256`)
  is keyed by HWND value and never cleared on destroy, and HWNDs are recycled.
- Wayland (`wayland.rs:251`) reports the integer `wl_output.scale`, wrong
  under fractional scaling, and `scale_checks` then warns on every such
  output. Derive `mode.width / logical.width` when both are known.
- X11 (`x11.rs:220`) reports `physical_size_mm: Some((0, 0))` for
  EDID-less outputs; the other adapters map that to `None`.

**§77.2 — Role assignment mixes raw and logical monitor indices.**
*(outstanding)*

`automatic_roles` (`pulpit-display/src/reconcile.rs:263`) compares raw
indices from `resolve_role` and `snapshot.builtin()` against logical targets.
With outputs A(0), B mirroring A(1), C(2), explicit audience = B and no
builtin, both roles collapse to 0 → `SharedDisplay` although C was free. Map
explicit resolutions through `logical_target` before `automatic_roles` and
delete the second mapping at `reconcile.rs:393`. Capture the scenario with
`pulpit-topology` as a regression script.

**§77.3 — `resolve()` gives up as `Ambiguous` before trying the weaker
candidate.** *(outstanding)*

`snapshot.rs:256`: on ≥2 strong matches, `record.fallback` is never consulted.
Twins whose EDID serial descriptor is the placeholder `"0"` (`x11.rs:594`
turns any non-empty text into `Stable`; only the binary serial is checked for
0) get the same identity and the `Connector` fallback that would separate them
is never tried. Intersect with the next candidate first; treat all-zero
serial text as absent.

**§77.4 — The X11 `PlacementTrust` latch is unobservable by the app.**
*(outstanding)*

`x11.rs:335` flips `capabilities()` to `TILING` after a refused placement,
but `display.rs:100` reads capabilities once at connect and `reconcile`
excludes them from its `unchanged` check. Every later topology change emits a
`Place` that is refused and toasted. Either re-read capabilities on every
reconcile or drop the flip and let `Refused` be the signal.

**§77.5 — Scenario files do not round-trip a make with whitespace.**
*(outstanding)*

`scenario.rs:113`/`151`: `to_text` writes `make=Dell Inc.`; `parse` splits on
whitespace and rejects `Inc.`. Wayland reports full vendor strings, so
`pulpit-topology` on Wayland produces a file `Scenario::parse` rejects,
defeating the capture loop. Quote values on output and tokenise with quote
awareness.

**§77.6 — Application teardown asymmetries.** *(outstanding)*

- WM-closing the audience window (`app.rs:3538`) resets window state but does
  not release `audience_claim`, restore `roles.audience_fullscreen` or drop
  `placement_retries`; `stop_audience` (`app.rs:14511`) does all of these.
  Extract `audience_gone()` and call it from both.
- `put_down_document` (`app.rs:6894`) sets `signing = None` but leaves the
  other eight signing fields; `signing_saving_since` keeps `is_live()` true
  forever and the `.pulpit-signing-<pid>.pdf` scratch survives. The key
  ladder has no rung for the Sign dialog, so `Ctrl+O` reaches the keymap
  through it. Call `end_sign_flow()` there and add `Rung::SignDialog`.
- Reader snapshot PDFs under `$TMPDIR/pulpit-reader-<pid>/` survive `quit()`
  (`app.rs:5671`); the last one is a full copy of a possibly private
  document. `quit()` MUST call `reset_reader_rendering()` and remove the
  directory; sweep stale directories whose pid is not live.
- `flush_settings` and `save_session` spawn a thread per write
  (`app.rs:14596`); `quit()` then writes synchronously. A late flush thread
  can win via rename, and a late session thread can recreate the snapshot
  after `clear()`. Join the in-flight writer in `quit()`.

**§77.7 — Media coordination gaps.** *(outstanding)*

- `rebuild()` on the same generation (`crates/pulpit/src/media/coordinator.rs:440`)
  retains sessions by `OverlayId`, but ids are numbered by occurrence in page
  order (`pulpit-core/src/overlay.rs:826`) and pages arrive in render
  priority order, so ids shift as pages land and the forgotten session keeps
  running until `retire_generation`. Give `rebuild` the supervisor and close
  the orphans; longer term derive ids from something order-independent.
- A failed `StagingRoot::create()` (`coordinator.rs:605`) leaves every
  embedded overlay `awaiting` forever with no re-fetch and no message. Mark
  them `Blocked` with the reason.
- The mpv worker never consumes mpv events (`pulpit-media/src/worker/mpv.rs:463`);
  a decode failure looks like silence for 10 s + restart + 10 s before
  fallback. Bind `mpv_wait_event` and emit `Failed`/`Ended`.
- The protocol version handshake is enforced by nobody
  (`pulpit-media/src/supervisor.rs:919`; `protocol.rs:82` claims otherwise),
  and `handshake_deadline` is read nowhere.
- `Worker::shutdown` via `Drop` sleep-polls up to 5 s on the UI thread when
  a worker is discarded mid-run (`supervisor.rs:292`). Drop on a detached
  thread on the in-run paths.
- `speech/download.rs:93` has no read timeout and no byte ceiling; a
  stalled network parks the thread past the cancel button.

**§77.8 — Document session and protocol edges.** *(outstanding)*

- `document/session.rs:188` blocks on the document worker with no deadline;
  a `while(true){}` field script under V8 wedges the reader thread and every
  queued `Ask`. Use a bounded wait, then mark the session lost.
- `DocumentRenderRequest::validate` (`document/protocol.rs:654`) allows
  16384² but `write_message` refuses > 80 MiB, so an in-bounds request kills
  the worker instead of being `Refused`. Bound `rgba_bytes()` against the wire.
- `DirtyRect::union` (`document/form.rs:132`) assumes the orientation
  `is_empty` explicitly refuses to assume; two inverted rectangles yield the
  gap, not the cover. Normalise in `invalidate`.
- `ask()` questions (`pulpit-render/src/supervisor.rs:766`) are
  fire-and-forget; a crash loses Navigation/Capabilities/Links with no event
  and the app never re-asks (`app.rs:16754`). Track and replay from `kill()`.
- Give-up is instant and permanent (`supervisor.rs:1204`): no delay between
  respawns, dead slots count toward `config.workers`, and after both slots
  give up nothing renders again even for a good deck. Use `Backoff{until}`.
- `list_tar` (`pulpit-render/src/images/archive.rs:168`) bounds entry count
  after the whole listing is collected. Refuse inside the loop.
- DjVu `open` and `page_info` (`djvu/backend.rs:594`, `306`) wait with no
  cancel or attempt bound while `page_text` in the same file has one.

**§77.9 — Convention and correctness residue.** *(outstanding)*

- `crates/pulpit/src/display.rs:93–205` holds the only `cfg(target_os)`
  above `platform/`. It is adapter selection, not a view, so it is not the
  bug the rule targets, but it makes the grep dirty. Move `detect_backend`,
  `identify_window` and `native_window_id` to `platform/display.rs`.
- `platform/capabilities.rs:217–242` and five doc comments contain U+00E2
  plus C1 control bytes where em-dashes were (`grep -c $'\xc3\xa2'` → 11).
  These go into the bug-report bundle. A test SHOULD assert `report()` lines
  are printable.
- `timing/model.rs:626`: a snoozed cue outlives the alarm's deletion and goes
  stale after suspend. Store the origin on the snoozed record.
- `designer.rs:982`: the assigned `LayoutId` is itself an undoable edit, so
  Undo after first save forks the layout on disk. Keep `id` outside the
  undoable state.
- `typst_annotation.rs:196`: `tiny_skia` premultiplied pixels are written as
  straight alpha (`document/pdfium.rs:2556`), fringing every Typst mark.
  Demultiply first.
- `reader_link.rs:781` vs `reader.rs:567`: the unfilled-required-field rule
  is written twice and has drifted (`is_reachable()` in one, not the other).
  Move to `FormField::is_unfilled_required()` in `document/model.rs`.
- `reader.rs:2318`: one `Applied` answer stamps every unstamped retained
  preview, so a second in-flight stroke is taken down by a frame that does
  not contain it. Stamp only the marks the answer created.
- `signing.rs:856`: certificate validity crosses from pulpit-render as a
  formatted string and is re-parsed; parse failure means "valid".
  `verify::CertificateSummary` already carries `i64` seconds; `CredentialSummary`
  SHOULD too.
- `layout/store.rs`: `into_layout` never runs `Widget::sanitise` although
  `widgets/mod.rs:601` promises import does; `format_version` truncates
  through `as u64 as u32`; `delete` swallows `remove_file` errors.
- `pdfwrite/mod.rs:740`, `sign/apply.rs:853`: generation numbers are dropped
  when re-emitting existing objects (`"{n} 0 obj"`), so a catalog `5 1 R`
  is rewritten as `5 0 obj` under a trailer that still says `5 1 R`.
- `verify/preflight.rs:762`: a `/Reference` array with more than one
  transform reads only the last `/TransformMethod`, so a FieldMDP lock beside
  a DocMDP entry is ignored.
- `pulpit-core/src/ipc/shm.rs:34`: `path_for` accepts `"."` and `".."`;
  stale-region reclaim is Linux-only (`shm.rs:133`), so other platforms leak
  rings on every crash.
- `speech/speaker.rs:273`: `cache.clear()` on `len() > 8` can discard a
  result the control thread is about to collect. Evict all but the key just
  inserted.

---

## 78. One parser for PDF objects

*(outstanding)*

**§78.1** The finding. `verify::objects::Lexer` is a complete, bounded parser
(`PdfValue`, depth cap, array cap, stream extents). `sign::apply::parse_value`
(`sign/apply.rs:1439–1585`) is a second full recursive-descent parser over
`PdfTokenizer` tokens into `PdfObject`, with its own depth cap, its own
`n g R` detection, its own hex decoding and a copy of `Lexer::parse_name`.
Every reader above the resolver is a third parser, a key/value state machine
over tokens: `extract_signature_field`, `extract_sig_dict_info`,
`extract_subfilter_and_mod_date`, `extract_docmdp_level` (`verify/mod.rs`);
`extract_field_info`, `find_dict_value`, `extract_docmdp_p_level`,
`extract_fieldmdp_from_sig_dict` (`preflight.rs`); `parse_trailer`
(`pdfwrite`). `ObjectResolver::object_bytes` exists only to re-serialise an
objstm object to text so a tokenizer can re-parse what the Lexer already
parsed. The trailer is read five ways. The file comments record four past
divergences between these readers, and §76.5, §76.6 and the last three items
of §77.9 are the same class.

**§78.2** The rule. Within `pulpit-render`, untrusted PDF bytes MUST be parsed
by `ObjectResolver`/`Lexer` and nothing else. A reader that needs a
dictionary value calls `resolver.resolve(n)` → `Dict`. `PdfTokenizer` MAY
survive for the one thing the Lexer cannot do, the `/Contents` token span,
until `Lexer::parse_hex_string` records spans.

**§78.3** The work.

- `xref_section_object_numbers` returns the whole `XrefSection` (entries,
  `prev`, `xref_stm`, end offset) so `RevisionMap::build` stops calling
  `parse_xref_extent` and `find_prev`.
- preflight and apply read `resolver.resolve(n)` → `Dict`; add a
  `PdfValue → PdfObject` conversion or give `Lexer` an ordered dict; delete
  `apply::parse_value`, `tokenize`, `decode_pdf_name`.
- `extract_signature_field` receives a `FieldEntry` already resolved with
  inheritance and re-tokenises the same dictionary to "refine" the same
  three keys; the refinement can only differ when two parsers disagree.
  Parse the dictionary once, read `/ByteRange`, `/SubFilter`, `/M` and the
  `/Reference` array from the `Dict`; the three `in_byte_range`/`in_reference`
  booleans become one enum or vanish.
- `/Reference` is parsed as an array of dictionaries and each `SigRef`
  evaluated on its own (fixes §77.9's multi-transform item).
- `pdfwrite`'s unused typestate machine (`SigningSession`, `TbsDocument`,
  `PreparedByteRangeDigest`, `DigestSpans`, `emit_placeholders`,
  `fill_signature_reservation`, `mod.rs:134–260`, `544–610`) has no callers
  outside its tests; `assemble_revision` instead writes `Raw` placeholders
  and `locate_placeholders` (`apply.rs:1199`) searches appended bytes for the
  literal `/ByteRange [`. A page dictionary carrying `/Contents <00>` makes
  the first match land in the wrong object. `append_objects` already records
  `obj_offsets`; compute reservation offsets from the signature object's
  layout and delete the typestate types.
- `ObjectResolver::new` runs about ten times per signing
  (`plan_revision`, `is_encrypted`, `count_signatures`, `preflight_sign`,
  `find_field_object`, `find_catalog_ref`, three `parse_object_dictionary`,
  `page_object_at`, two in `check_page_index`); `IncrementalWriter::open`
  copies the whole source (`mod.rs:699`) while the caller holds it. Build one
  resolver in `sign_document_file_inner` and pass it down; make the writer
  borrow `&'a [u8]`.

**§78.4** Expected effect: roughly 1 500 lines removed, the jscpd clones at
`verify/mod.rs:1258/1347` and `preflight.rs:580/739` gone, and the class of
"two readers disagree" defects closed. A test MUST assert that discovery and
preflight read the same set of signature fields from the hierarchical fixture
of §76.6.

---

## 79. Application structure

*(outstanding)*

**§79.1 — Park-on-a-flag fields.** `speech_nav`, `bookmark_added`,
`sign_resume_pending`, `print_spool_pending`, `form_clipboard_text`,
`area_clipboard_text`, `resume_after_form_commit`, `search_focus_pending`
(`app.rs:1190–1663`) exist because `pump_reader`/`pump_renderer`/`poll_media`
return `bool` and `on_tick` (`app.rs:5930–6020`) drains each by hand, costing
a tick of latency and eight `is_live` clauses. Every caller of the pumps can
return a `Task`. Add `deferred: Vec<Message>` on `App`, push from anywhere,
drain once in `update()` after `dispatch`.

**§79.2 — Nested `update()`.** 41 sites call `self.update(..)` recursively
(`app.rs:2815–2846` and onward), so `Key → Keymap → update(Do) → on_action →
update(Nav)` runs `sync_annotation_layers`,
`size_the_cache_for_what_is_mounted` (which reads `std::env::var` each time,
`app.rs:15879`), `sync_presenter_fullscreen` and `latency.record_update`
three times per keypress, and the nested timings pollute the worst-update
ledger. `MenuAction` already does it right (`self.dispatch(*message)`,
`app.rs:3968`). Replace the recursive calls with `dispatch`; cache the budget
override in a field set in `App::new`.

**§79.3 — Render bookkeeping copied four times.** `app.rs:8197`, `13454`,
`16544`, `16734` (and `8175` vs `16693`) each do `next_request_id`, `submit`,
`pending.push`, `submitted_at.insert`; both planners do collect-obsolete →
`HashSet` → `retain` → `cancel` → `remove`. These are the jscpd hits in
app.rs. Extract `submit_render(key, job)` and `cancel_renders(ids)` so the
"a request is in exactly these three maps" invariant lives once;
`take_pending` (`app.rs:15625`) is its third half.

**§79.4 — Subsystem state as structs.** `App` is ~180 flat fields
(`app.rs:1030–1832`). Printing is five fields encoding one state machine
(`print_dialog`, `print_scratch`, `print_pending`, `print_in_flight`,
`print_spool_pending`) → `enum PrintJob { Idle, WritingScratch, ReadyToSpool,
InFlight }` removes the "which flag wins" comments at `app.rs:7367` and
`11005`. Signing is nine fields beside an existing `SigningFlow` enum. Speech,
search (7), overview (6), thumbnails (8), media (9) similarly. Group each into
a struct and move its `impl App` block to `app/print.rs`, `app/sign.rs`,
`app/reader_forms.rs`, `app/thumbnails.rs`, `app/media.rs`, `app/search.rs`.
No behaviour change; the invariants become local. This SHOULD follow §79.1–3,
not precede them.

**§79.5 — Smaller parallel arms.** `EditSelected`/`PageDoubleClicked`
(`app.rs:10055` vs `10074`) build the same `ComposingMark`; `PickDate` and
`commit_focused_time` (`9985` vs `9355`) build the same `SetField`
transaction; the Undo/Redo arm and `restore_reader_edits` (`9804` vs `10452`)
push the same `PendingEdit`; `page_showing`/`showing_page`/`slide_showing`/
`slide_for_page` (`13204–15615`) are four spellings of one lookup, two of them
O(slides). `run_document_actions` returns a `Task` that is always `none()`
and `app.rs:6635–6645` builds a `DocumentInfo` and discards it.

**§79.6 — The document manager's promotion state is bypassed.**
`on_candidate_opened` (`doc/manager.rs:299`) returns `RenderFirstFrame` and
enters `AwaitingFirstFrame`; the app's arm (`app.rs:6749`) calls
`on_first_frame` immediately. The state is entered and left in one tick, the
module doc is false, and `on_candidate_failed`'s `AwaitingFirstFrame` arm is
unreachable. `on_candidate_opened` also reads `Instant::now()`
(`manager.rs:312`) while every other transition takes `now`. Either wire the
real first-frame event or delete the state and rename the action `Promote`.

---

## 80. Duplication to collapse

*(outstanding)* Each item names the one owner.

**§80.1** `forward_to_child!` (`crates/pulpit/src/widgets/mod.rs:751`) lacks
`update`, `draw` and `overlay` arms, so `residency.rs:242–306`,
`widgets/panel.rs:35–102`, `widgets/common/popover.rs:84–137` and
`widgets/scroll.rs:636–651` hand-write them. These are the two largest jscpd
clones (46 and 35 lines). Owner: the macro.

**§80.2** Six hand-rolled "walk `/Annots`, keep widgets, `CloseAnnot` on both
branches" loops in `document/pdfium.rs` (`336`, `747`, `823`, `893`, `940`,
`4093`) while `on_annotations` (`2321`) exists. Owner: a new `on_widgets`.
`field_script_reaches_out` (`2850`) re-implements
`additional_action_script` (`2809`); the jscpd pair `1450/1494` and
`pdf/pdfium.rs:342–403` are two `hits_from_pdfium_matches(quads_of(..))`
closures; `composite_form_fields`' fallback (`637`) re-implements
`pdf/pdfium.rs:1540`.

**§80.3** Backends repeat the wrapper's prologue. `PdfDocument::find_text`
and `check_page` (`document/mod.rs:445`, `701`) already clamp ranges,
short-circuit empty queries and truncate hits; `djvu/document.rs:133`,
`document/pdfium.rs:4454`, `images/document.rs:126` and `memory.rs:281` do it
again. `djvu/backend.rs:699` and `pdf/pdfium.rs:1152` carry the same
`find_text` driver with different clamping and error policy. Owner: the
wrapper; a `search_pages` helper in `pdf/search.rs` for the two backends.

**§80.4** `ImageDocument` (`images/document.rs:246`, `306`) reimplements
`ImageBackend` (`images/backend.rs:68`, `115`) rather than wrapping it as
`DjvuDocument` wraps `DjvuBackend`; the two `measure` routines have already
drifted on what an unmeasurable first page means. Owner: `ImageBackend`.

**§80.5** Reader relayout-anchored-to-page is written six times
(`reader.rs:3879`, `3892`, `4301`, `4346`, `4426`, `4265`), three
re-clamping `offset_x` and three not; `take_crop`/`auto_crop` also duplicate
`crop_restore`. Owner: `relayout_anchored(page)` and `crop_to(region)`.

**§80.6** Core geometry written three times each: point-to-segment distance
(`annotate/hit.rs:194`, `annotate/stroke.rs:581`, `annotation.rs:1682`);
point-in-region (`document.rs:1462`, `overlay.rs:328`, `annotation.rs:1385`);
outline parent-path walk (`navigation.rs:201`, `248`, `267`). Owners:
`PagePoint::distance_to_segment` in `page.rs`, `Region::contains` in
`notes.rs`, `Outline::level_mut(parents)`.

**§80.7** Display identity: ladder assembly copied in `x11.rs:187`,
`windows.rs:327`, `macos.rs:257`, `scenario.rs:203`; identity-or-fallback
match in `x11.rs:394`, `455`, `macos.rs:402`, `windows.rs:585`,
`reconcile.rs:335`, all spelling the private `Monitor::matches_exactly`
(`snapshot.rs:187`). Owners: `identity::ladder()` and a `pub`
`matches_exactly`. The adapters MUST NOT be made generic; two six-line
`snapshot()` bodies do not justify a trait.

**§80.8** Signing enums mirrored variant-for-variant: `PlacementPosition`/
`PlacementSize` (`signing.rs:415`) vs `settings::StoredSignaturePosition/
Size`; `appearance_rotation` (`signing.rs:524`) maps `PageRotation` onto
`sign::AppearanceRotation`, which can *be* `PageRotation` since pulpit-render
depends on pulpit-core; `subject_common_name` (`signing.rs:539`) equals
`common_name` (`signature_profiles.rs:210`).

**§80.9** Convention-test rosters. `theme/mod.rs:951`, `995`, `1040` are
three hand-maintained `include_str!` lists that have drifted: `annotations`,
`media`, `chrome` are missing from the colour/spacing tests, and the omitted
files are where convention breaks live (`widgets/media/model.rs:30` types
transport glyphs as text although `Icon::Play`/`Pause` exist;
`media::SPACING = 10.0` is on no scale). One `THEMED_VIEWS` roster; replace
`Action::glyph()` with `Action::icon()`.

**§80.10** View helpers. ~12 `if selected { selected_button } else {
tool_button }` sites across document/annotations/search views and `view.rs`
while `selectable()` (`view.rs:1725`) exists; `navigation_overflow` and
`document_tools_overflow` (`widgets/document/view.rs:2568` vs `2609`) are the
same trigger+hint+Popover; `annotations/view.rs:354` re-implements
`common::color::wheel`; `Padding::from([4.0, 8.0])` appears 15+ times.
`designer_view.rs:944` `canvas_node` is a second layout traversal that
disagrees with `layout_renderer::node` on `hug_extent`, `gap` and
`collapse_empty`, so the editor does not show what the presenter renders.

**§80.11** Platform. `linux.rs:452 which` vs `cups.rs:29 available` (the
latter called twice per snapshot); the process-token inhibitor is written in
`linux.rs:578` and `macos.rs:324`. Atomic file writes exist three ways:
`paths::write_atomically` (correct), `settings/store.rs:152` (hand-rolled
although `atomic::Visibility::Private` exists), `doc/shape.rs:402`
(predictable `.tmp` sibling, no fsync, the pattern `paths.rs:733` documents
as replaced). `config_directory().join("settings.toml")` and
`.join("signatures")` duplicate `Directories::settings_file()` and
`signing_credentials()`, which carry `#[allow(dead_code)]`.

**§80.12** Worker bootstraps. `main.rs:360–470` has three document-worker
entry points differing only in engine and log label; `bin/worker.rs:98` and
`main.rs:307` are two render-worker bootstraps, and the test binary's
`FailureInjectingBackend` forwards only 6 of 14 `PdfBackend` methods, so
tests through it cannot exercise links, search or attachments.

**§80.13** Hand-declared libc. `pulpit-media/src/runtime/chromium.rs:697` and
`worker/mod.rs:132` declare `pipe2`, `fcntl`, `dup2`, `poll` and copy the
constant tables, while `libc` is already a workspace dependency used by
`pulpit` and `pulpit-display`.

---

## 81. Dead and bypassed machinery

*(outstanding)* Established by reference count. Each MAY be deleted without a
design discussion; the ones marked *decide* need one sentence of intent first.

- `settings/keys.rs:597–669`, `901–967`: ~300 lines migrate a keymap that
  `Settings.keymap` `#[serde(skip)]` and the schema-2 migration
  (`store.rs:204`) ensure is never loaded. `Action::ALL` (`keys.rs:273`) is a
  hand list whose own doc records it drifting; an exhaustive `match` count
  makes a new variant a compile error.
- `widgets/registry.rs`: `WidgetId`, `WidgetKind::id()`, `from_id()` and
  `REGISTRY[i].id` are a "Phase 3" persistence surface used only by their own
  tests (`#[allow(dead_code)]` ×4); `REGISTRY` is a second 26-row table beside
  `CATALOG`. `registration()`/`definition()` are linear scans with `expect`
  called per frame from `minimum_size()`. *Decide*: land id persistence or
  drop `WidgetId`; fold `plan` into `WidgetDefinition`; index by
  `kind as usize`. `CATALOG`, `plan.rs` and `sample.rs` earn their place.
- `RuntimeId::{WebKitGtk, WebView2, WkWebView}` have no implementation and
  flow through selection, diagnostics and `worker_command`, which fabricates
  a program name so the launch can fail. *Decide*: delete or collapse to one
  `SystemWebView` placeholder.
- `SurfaceRing::held/hold/release/is_held` (`pulpit-media/src/surface.rs:108`),
  `handshake_deadline` (`supervisor.rs:49`).
- `Action::Unfullscreen` and `WindowMode::Hidden` (`reconcile.rs:130`;
  arms in `x11.rs:418`, `macos.rs:450`, `windows.rs:661`) are never
  constructed by `reconcile`; the `_randr_types` import guard (`x11.rs:602`).
- `WindowPolicy` and `InputPolicy` (`platform/window.rs:850`,
  `platform/input.rs:67`) are boxed traits with one impl each and five dead
  methods.
- `Debouncing.last_stamp` (`doc/manager.rs:119`); `run_document_actions`'
  `Task` return; `SignatureLine::InkMark` (`signing.rs:968`) under
  `#[allow(dead_code)]`; `OnMount` (`layout/model.rs:201`) never constructed
  non-default and not in `LayoutFile`; `first_page_size` duplicating
  `page_sizes[0]` (`pulpit-core/src/document.rs:1516`); `PageOverlay.page`
  and `.poster` (`overlay.rs:308`) derivable; `History::current_mut`
  (`layout/history.rs`) dead until §82.5 uses it.
- `shm.rs::AttachedRegion::read_only` (`pulpit-render`) has no callers and
  lacks the size ceiling the other constructors have.
- `speech/download.rs:472` `a_file_that_fails_its_pin_is_discarded_and_reported`
  never calls `fetch`; delete or make it real so the coverage it implies is
  not believed.
- Stored-in-lockstep state: `snooze_minutes` and `target_seconds` each in
  `TimerControls`, `AlarmControls` and `settings.timer` (`timing/model.rs:387`,
  `546`); `Annotations` `drawing`/`erasing` bools plus `typing: Option<usize>`
  for one gesture (`annotation.rs:783`) → `enum Gesture`; `Staged` in the
  media coordinator (`coordinator.rs:186`) is three `Option`s for the
  three-state enum `Need` already names; `Reading { state: SpeechStateInner
  { state } }` (`speech/cursor.rs:161`).

---

## 82. Hot-path efficiency

*(outstanding)* Ordered by how often the path runs.

**§82.1 — Per redraw.** `App::theme()` rebuilds `iced::Theme::custom`
(`theme/mod.rs:65`, `app.rs:2467`) every redraw of every window, allocating
the name and regenerating ~30 derived colours; build once per settings or
appearance change. `presenter()` (`view.rs:285–400`) builds the whole layout
body and then replaces it with `shortcut_reference_page` when no document is
open (`view.rs:384`); hoist the check. `Layout::widgets()`/`cells()`
(`layout/model.rs:24`) allocate a `Vec` per call, ~8 calls per frame;
`PrimaryViewer::of` → `has_capability` → `capabilities()` → `occupies()`
allocates two more per widget. Provide non-allocating iterators.

**§82.2 — Per redraw, reader.** The page surface copies every visible
page's geometry three times per frame, once inside the layout closure:
`reader.facet()` (`reader.rs:4465`) clones retained-stroke previews into
`ReaderPage`, `PageSurface::from` (`widgets/document/view.rs:225`) clones
`reader.visible`, `sheet()` (`view.rs:466–560`) clones `found`, `retained`,
`selection`, `dead_fields` per page, and `sheets()` runs inside `responsive`,
i.e. every layout pass. Make the vectors `Rc<[T]>`, hold `Rc<[ReaderPage]>`
in `PageSurface`, let the preview layers take references.

**§82.3 — Per pointer sample.** `pointer_moved` (`reader.rs:2356`) evaluates
`self.interaction.gesture().cloned()` to test for `Transforming`, copying the
accumulated `Vec<InkPoint>` on every move during a stroke: quadratic over the
stroke, on the path `budgets::unfinished_ink_follows_input_without_waiting_for_anything`
exists to protect. Match by reference; nothing needs cloning.

**§82.4 — Per tick.** `poll_speech` clones `SpeechSettings` at `app.rs:2514`
and eleven other sites to satisfy a borrow that disjoint field borrows already
permit. `voice_library` (`view.rs:2493`) runs `Directories::detect()` and
`speech::browsable` per redraw while Settings is open; `space_report`
(`designer_view.rs:852`) rebuilds `built_in_layouts()` per frame although
`Designer::recommendations()` was memoised for this; `Sketch` clones the
whole `Node` tree per library card per pass with no `canvas::Cache`.

**§82.5 — Per pointer sample, designer.** `drag_divider_to`
(`designer.rs:846`) calls `History::edit` (clone whole `Layout`, push) plus
`revalidate()` on every move; a 200 px drag is 200 undo entries and 200 full
validations. Snapshot in `start_divider_drag`, mutate via `current_mut`,
commit once in `end_divider_drag`.

**§82.6 — Per edit.** One annotation delete re-walks and re-summarises the
page ~6 times (`document/pdfium.rs:3594`, `3722`, `mod.rs:851`): `annotation(id)`
= `locate` + full `annotations(page)`; `delete` then runs `before_image`
(`locate` + `annotation` + `capture_appearance`) and `locate` again. Eraser
sweeps are O(deletes × annotations) in PDFium string reads. Add
`with_annotation(id, f)` beside `on_annotations` (also §80.2).
`adopt_texts` (`pulpit-core/src/annotation.rs:1160`) re-mints every label's
id on each adopt, and the Typst cache (`typst_annotation.rs:357`) is keyed by
id, so every `Applied` answer recompiles every label on the slide. Derive the
id from the annotation name as `overlay.rs::stable_overlay_id` does.

**§82.7 — Per render, images.** `crop` (`images/decode.rs:375`) clones the
whole decoded image for a `Region::FULL` render and the result is copied
again when no scaling is needed: two full-frame copies for a no-op. Use
`crop_imm` as a borrowed `SubImage`. `measure_entries` (`images/archive.rs:203`)
inflates every entry in full (up to 256 MiB) to read a header; a 400-page
comic inflates ~2 GB on open. Read a 64 KiB prefix first.

**§82.8 — Per dispatch.** `supervisor.rs:1273 dispatch` allocates and sorts
`order` over the whole queue once per dispatched job; `cache.rs:445
enforce_budget` is O(entries × pinned) with `Vec::contains`; `Outline::len()`
(`navigation.rs:72`) allocates a `Vec` of up to 4096 references to count;
`sign_target_candidates` (`app.rs:12171`) reads the whole PDF and runs two
preflight parses on the event loop.

---

## 83. Standing rules

These are the rules that stop the findings above from coming back. They
belong in `docs-src/internals.typ` once adopted; they are stated here so the
fixes can cite them.

**§83.1 — A seam gets a test that crosses it.** When a contract is shared by
two modules (a journal and its replayer, a supervisor and its worker, a
verify path and a sign path, a manager state and the app arm that drives it),
there MUST be one test that drives both sides together. A test of one side's
shape (§76.2, §76.7, §76.8) is not that test.

**§83.2 — Untrusted `str` is never byte-sliced.** Text that arrived from a
file (URIs, names, labels, s-expressions) MUST be indexed through
`as_bytes()`, `get(..)`, `char_indices()` or `strip_prefix`, never
`&s[a..b]` with a computed offset (§76.4).

**§83.3 — Bound the work, not the result.** A cap on a parsed count, size or
expansion MUST be applied inside the loop that does the work, against the
bytes actually consumed, not against a header's claim and not after the
collection is complete (§76.9, §76.15, §77.8 `list_tar`).

**§83.4 — One parser per format per crate.** Untrusted PDF bytes in
`pulpit-render` are parsed by `ObjectResolver` and nothing else (§78.2).
A second tokenizer over the same bytes is a defect, not a convenience.

**§83.5 — A failure reply is a state transition.** A worker's `Failed`
either removes the thing it names on both sides or is a `Warning`; a
supervisor that forgets a session MUST tell the worker (§76.8, §77.7).

**§83.6 — No flag for the next tick.** Work that becomes possible inside
`update()` is queued as a `Message` and drained in the same turn (§79.1).
A field whose only purpose is to be read by `on_tick` is a defect.

**§83.7 — `cfg(target_os)` above `pulpit::platform` is zero, including
`display.rs`.** The check is `grep -rn target_os crates/pulpit/src | grep -v
/platform/` and it MUST print nothing (§77.9).

**§83.8 — A documented guarantee has code behind it.** A module comment that
says "is shut down", "is released", "is bounded" or "is never" MUST name the
function that does it, and that function MUST have a caller (§76.10, §77.7
handshake, §79.6).

---

## 84. Passes that found nothing

Recorded so they are not re-run.

- **Rule 1.** `reconcile()` is pure, clock-free and idempotent by
  construction; swap, fullscreen, topology, native-id arrival and resume all
  funnel through it; `tests/topology_script.rs:141` asserts idempotence for
  every scenario × capability profile.
- **Rule 2.** Every `coordinator.native(role)` is resolved immediately before
  the native call; `PlacementRetry` stores a `MonitorIdentity`; adapters hold
  connections, atoms and counters, never window or output ids. The Windows
  power-request `HANDLE` is stored as text across turns; there is no other
  way to release it.
- **Rule 3.** `audience_frame_key` falls back to `last_audience`;
  `set_color_mode` re-mints handles in place; cache clear on `Promote` of a
  different deck is deliberate and documented; DjVu partial decodes are
  waited out.
- **`ipc`.** Framing checks length before allocation, refuses zero, maps EOF
  to `Closed`; the fork-bomb marker is set on every spawn path; `Doorbell`
  coalesces correctly; `shm` naming and sweep are used by both worker crates,
  and neither `pulpit-render/src/shm.rs` nor `pulpit-media/src/surface.rs`
  is a leftover copy. The jscpd clone `pulpit-media/src/protocol.rs:762` ↔
  `pulpit-render/src/protocol.rs:355` is the intended per-protocol
  `MAX_MESSAGE_BYTES` binding, not framing.
- **Render protocol and cache.** `Rendered` declared bytes are cross-checked
  against `width×height×4` before any read; region exclusivity holds;
  `FrameKey` omitting the crop region is safe because `set_zoom` advances the
  generation; worker-side below-floor `Render` drops are unreachable.
- **PDFium FFI.** Every `Load*` is null-checked and paired with `Close*` on
  all branches including cancel; `PauseState` is `#[repr(C)]` header-first;
  `BOUND` prevents a second bind per process.
- **DjVu FFI.** `Api::bind` keeps the `Library` alive beside its fn pointers;
  every returned pointer is null-checked; `is_cons` guards `car`/`cdr`; all
  FFI runs under one lock; drop order is documents → format → context.
- **Image decoding.** Dimensions are bounded from the header before pixel
  allocation on both file and archive paths; `read_within(limit + 1)` is
  applied to actual bytes on every read path.
- **CMS.** `signing_time` is caller-supplied; `generate.rs` takes `SystemTime`
  parameters; validity is transcribed, never compared to "now".
  `raw_signed_attrs` cannot confuse the sid; `resolve_primitive` refuses
  unknown algorithms rather than falling back; the digest comparison is not
  constant-time and does not need to be, both operands being public.
  Signature dictionaries are accepted only at `Confidence::Resolved`.
- **Hostile-xref handling.** `/Prev` cycles, chain cap, decode budget, `/W`
  width cap, `/Index` caps, predictor overflow, objstm-in-objstm, `/Length`
  past EOF are all bounded and each has a unit test; four cargo-fuzz targets
  exist.
- **Reader.** `reader_journal.rs` syncs header and every entry, stops at the
  first unparsable line, orders by revision, gates on `is_live`;
  `reader_link.rs` returns exactly one `Told` per `Ask`; `form_flow.rs`
  invariants hold and are tested; `session.rs` writes atomically;
  `autocrop::ink_bounds` validates length with checked multiplication.
- **View.** Overlay slots are positionally stable; `scrub_layer` memoises by
  panel size; `overview` virtualises; `grid_plan`, `Zoom::scale`,
  `Column::lay_out` guard NaN and non-finite; `store.rs::save` is atomic;
  `timing/model.rs` has no clock reads outside tests.
- **Platform.** The three OS adapters differ in substance, not skeleton; a
  generic with hooks would add indirection for ~20 shared lines. Every
  `PlatformServices` operation returns `Outcome`. `doc/watcher.rs` handles
  rename-replace and event storms; `instance.rs` decides by flock.
- **Speech.** The concept is not represented three times: core decides,
  media drives, `speech.rs` is the seam. The one mirror
  (`capabilities::Speech` ↔ `Availability`) is justified by the boundary.
