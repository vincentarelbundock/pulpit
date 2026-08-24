# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Signing no longer opens a dialogue.** Every question the Sign dialog asked
  had an answer already recorded in Settings or in the document, and a modal
  that only restates known answers is a click, not a safeguard. The field is
  the one you clicked; visibility, position and size come from the signature
  profile; the reason/location/contact boxes are gone. With one profile
  already unlocked this session, the only thing signing shows is the save
  dialog for the signed copy. A small panel appears only when there is a real
  question: which profile, when more than one is saved; its passphrase, when
  the session does not already hold the credential; and §33's override for an
  expired certificate. With no profile saved at all, signing refuses and names
  Settings → Signatures rather than offering a `.p12` picker that would forget
  the file again — importing an existing `.p12`/`.pfx` was already what that
  section is for.

  **Signing an annotated document asks once and writes one file.** The edits
  still have to reach the disk before a signature can be computed over them,
  but where that copy goes was never a question worth asking: it is written
  to a scratch file beside the document and deleted as soon as the signature
  has been made from it. Signing an edited document used to mean two pickers
  and two files — an annotated copy nobody asked for, and the signed one.

  A field you clicked that the document does not offer for signing is now
  refused by name, instead of the signature landing at a preset corner of some
  other page. Whether a signature is drawn on the page is a tick box in the
  profile editor, beside the position and size it has always had.

  **The signed copy is opened, and nothing asks about it.** Signing writes a
  new file beside the source, so leaving the unsigned original on screen read
  as a signature that never appeared. The copy now becomes the document on
  screen, with editing off so its signatures keep verifying — not offered as a
  choice, because the reader has just made that signature themselves and
  confirming it back to them is not a safeguard. The corner notice says both,
  and names **Allow editing** in the signature panel as the way to change it.
  §31.2's identity disclosure and §31.3's countersigning note live on that
  panel too, which outlives the notice.

- **The signed-document question is asked in the reader's terms.** Opening a
  PDF that already carries a signature used to offer "Append-only mode" or
  "Edit anyway" over a paragraph citing §28.4 — the name of the mechanism and
  a section number, neither of which says what you are allowed to do. It now
  says "This document is already signed" and offers two answers, each carrying
  its own consequence underneath it: **Read and sign only**, which keeps the
  existing signature intact, or **Allow editing**, which will have every viewer
  report the document as changed after that signature once you save. Nothing
  in between: a paragraph there could only say the same thing a third time.

### Fixed

- **Leaving fullscreen no longer leaves the page soft.** The reader's cell
  shrinks on the way out, so the sharp frames rendered for the full screen no
  longer fit it and the coarse previews do — and the frame chooser took the
  one that fit, whatever its quality. Nothing replaced it either: those same
  wide sharp frames were counted as already satisfying the narrower request,
  so no new render was ever asked for, and the page stayed soft for as long
  as it was looked at. Quality now outranks fit. A sharp frame wider than the
  cell is downsampled on its way to the screen and still looks like the page;
  a coarse one that happens to fit is upsampled and does not.

- **Home and End move the document being read.** They were bound to first
  slide and last slide whatever was on screen, so in a reading layout they
  walked the deck behind the document and did nothing visible — and mid-talk,
  End would have jumped the projector to the last slide. They now go to the
  first and last page of the document, and are recorded in history, so Back
  returns you to where you were reading.

- **The refusal that comes with a signed document points at a control that
  exists.** Declining to edit a signed document is offered once, when the file
  opens; the notice a drawing tool then gave sent the reader back to that
  offer, which was long gone. The choice now lives in the signature panel, for
  as long as the document is open.

- **The scrollbar is pulpit's own, drawn over the surface it scrolls.** The
  thumb keeps a minimum length worth grabbing — on a 730-page document iced's
  own rule works out at under four points — and it is grabbable exactly where
  it is drawn, which is the part a purely cosmetic thumb could not give.
  Dragging the empty track jumps the thumb to the pointer and carries on as a
  drag. The three states it always had are unchanged: quiet at rest, clearer
  under the pointer, accented while dragged.

  This replaces the vendored fork of `iced_widget`, which existed only to make
  that minimum configurable. A `[patch.crates-io]` applies inside a workspace
  and nowhere else, so the fork made the `pulpit` crate unpublishable: on
  crates.io it resolved the real `iced_widget`, which has no
  `min_scroller_length`, and 0.0.7 reached the registry without the
  application. `vendor/` is gone and the arithmetic now lives in
  `widgets/scroll.rs`, where it is unit-tested rather than buried in a
  dependency — including the part iced gets away with and we could not: with a
  floor worth aiming at, the offset has to be mapped through the shortened
  track, or the thumb hangs past the bottom by whatever the floor added.

## [0.0.7] — 2026-08-24

### Fixed

- **pulpit did not build on macOS or Windows, and had not for a week.** The
  macOS link failed on `pipe2`, a Linux extension with no macOS symbol, and
  the two `O_*` constants beside it carried Linux values that `fcntl` on a BSD
  would have accepted while setting some other flag. The Windows build failed
  to compile at all: `main` imported the Unix-only `OsStrExt` unconditionally
  to read its arguments as bytes. Argument parsing now uses
  `OsStr::as_encoded_bytes`, which every platform offers and which still keeps
  a path that is not UTF-8 intact.

- A test that checks a signed file is written beside its source without
  overwriting anything was reading Windows paths as though they used forward
  slashes, so it believed every candidate name was free. The behaviour it
  covers was never wrong — the application asks the filesystem, which has no
  opinion about how a separator prints — but the test only ran once Windows
  compiled again.

- **The independent signing oracle had stopped running in continuous
  integration.** At the pinned tag pyHanko became a workspace whose root
  package carries no code, so installing it asked setuptools to package the
  repository's own top-level directories and it refused. The library and the
  command line tool are now taken from that same commit's `pkgs/`
  subdirectories — they have to come from one commit, because every package in
  that workspace reports version `0.0.0.dev1` until its build injects the real
  one. All thirteen signed fixtures validate again.

- A test that renders through PDFium now runs on the same single thread every
  other PDFium test uses. Rendering announces each page to the form-fill
  environment, which is what makes PDFium build its V8 isolate, and libtest
  gives every test its own thread — so that isolate was being built on a
  thread that then exited. The renderer test job crashed or hung on roughly
  half its runs.

### Changed

- **`pulpit-testkit` is no longer a crate.** The fixture builder, the AcroForm
  corpus and the cross-engine checks moved into `pulpit-render`'s own
  `tests/testkit`, and the corpus dumper became an example. The crate could
  never be published, but it was still declared with a version, which meant
  `cargo publish` demanded it from a registry that will never have it and
  refused to package `pulpit-render`. The test corpus is excluded from the
  uploaded crate outright.

- Releasing now waits for continuous integration. The release and crates.io
  workflows fire on a tag, and nothing had ever checked that the commit under
  that tag built; `make release` says so before creating the tag, and both
  workflows refuse a commit whose CI did not pass. Every job also has a time
  limit, after three runs that hung for six hours apiece.

## [0.0.6] — 2026-08-23

### Changed

- Filling a form field costs less on a long document. Reading one field by name
  no longer builds every other field in the file first, which a commit used to
  do twice.

- Worker, file-watch, and display-topology results now wake the interface as
  they arrive instead of waiting for a periodic UI-thread poll. Delivery work
  is bounded per event-loop turn, document opening and placement verification
  no longer block the interface, and large search/outline snapshots are
  shared across view rebuilds.

- **The landing page and keyboard reference have been reorganised around the
  work users are doing.** Empty startup is now a mode-neutral welcome page
  with a prominent Open button, compact getting-started and presenting keys,
  and a link to the Pulpit website. Opening a PDF still uses first-page shape
  detection to choose Reader or Presenter.

  The complete reference uses seven semantic groups, larger type, and one
  quiet keycap per action. Acrobat and standard reader keys come first, one
  Vim/Zathura alternative appears in parentheses, and `PageUp`/`PageDown`
  remain visible beside the arrows. Hardware aliases for common remotes still
  work but no longer crowd the keyboard reference.

- **Keyboard shortcuts are a smaller fixed vocabulary for now.** Redundant
  navigation and editing aliases have been removed; Reader and Presenter use
  the same page actions. Shortcut customisation and the unknown-scancode
  binding prompt have been removed, and legacy persisted keymaps are discarded
  during settings migration so the visible reference always matches input.

### Fixed

- **A 116-byte PDF could hold the application for minutes on open.** The
  cross-reference subsection count and the xref stream `/Index` count were
  both taken from the file and never checked against how many entries could
  actually follow, so a tiny document drove the parser while a set grew
  without bound. Verification runs on every document open, in the main
  process, so this was reachable by sending someone a deck. Both counts are
  now bounded by what the remaining bytes can encode, and value parsing
  carries a depth limit so nested arrays cannot overflow the stack.

- **A tampered document could read as unsigned.** A signature field that
  failed to decode vanished from the report instead of reading as broken.
  Both failure paths now produce an unclear report, which the interface
  already renders as broken, while fields definitively typed as something
  other than `/Sig` are excluded so a malformed text box raises no false
  alarm. The revision chain also survives a nested trailer dictionary, and
  the signature container's extent comes from the tokenizer rather than a
  byte search a decoy could mislead.

- **Signing quietly rewrote non-ASCII text before certifying it.** Values
  parsed out of a document were laundered through a lossy UTF-8 conversion.
  Document bytes now travel verbatim, kept separate from pulpit's own UTF-8
  text, which is still legitimately transcoded, and field names are compared
  after decoding, so a field named Café can be found and signed.

- Rendered slides were world-readable in `/dev/shm` under predictable names
  that would be adopted rather than refused, and they leaked — 2 GB had
  accumulated over four days. Regions are now created exclusively, mode 0600,
  under unguessable names, and each process sweeps regions belonging to dead
  pids once at startup.

- **A crash lost every form field you had filled in.** Values typed into a
  form went into the undo history and moved the document's revision, but were
  never written to the recovery journal — so a recovery offered "N unsaved
  edits", put back the ink strokes, and silently dropped the fields. Filling a
  field is now recorded like any other edit, including which rows a
  multiple-selection list chose.

- **A field holding more than 16 KB of text read as empty.** A long comment box
  came back blank, which also meant a required field you *had* filled in was
  listed as still empty when saving. Long values are now read in full and cut
  to what pulpit carries, and a cut one is reported as cut rather than shown as
  a value it is not; a value pulpit only half read is no longer offered for
  editing, because writing it back would throw away the rest.

- **Saving straight from a field you were still typing in could write the old
  value.** Uncommitted characters live in the engine's editing view rather than
  in the document, so anything that saved without first taking the focus off
  the field wrote what was there before. The engine now closes the field's
  editor before it serialises, so this holds however the save was reached.

- **Fields the document hides were offered as places to type.** A widget marked
  Hidden or NoView is drawn by nothing, but Tab still walked to it and the
  pre-save review still asked you to go and fill it in — scrolling the page to
  a blank patch. Hidden fields are still listed, and are no longer somewhere
  the caret can be sent.

- Setting a value on a push button, a signature field or a field of unknown
  type reported success for a write that did nothing. It is refused, and says
  why.

- A form's own JavaScript can read the real date, and pulpit's documentation
  said otherwise. The behaviour is unchanged — closing the browser engine's
  clock is not reachable from here — but the claim, and the test that appeared
  to prove it, have been corrected to say what is true.

- **Reader mode had no working key.** `r` was bound to the timer reset *and*
  to reader mode, and since resolution takes the first match, the documented
  `r` shortcut never fired. The timer reset moved to `Shift+T` and `r` now
  means what it says.
- **`Enter` never committed a previewed slide.** It was bound to advance, and
  the commit was written against `"Return"` — a key name the toolkit does not
  emit — so both bindings were dead. `Enter` now commits the preview; page
  navigation stays on its dedicated keys.
- A keymap check now asserts that no key is bound to two different actions,
  and that every default binding resolves to the action it was written for.
  Both bugs above were the same missing guard.
- The duplicate `Escape` entry for cancelling a preview has been removed.

### Added

- **Documents can be signed, and existing signatures verified.** Pulpit signs
  a PDF as an incremental revision using a credential from a reusable
  signature profile; the passphrase is typed once and the unlocked credential
  is remembered for the session. An unsigned document's own empty signature
  fields are offered as targets ahead of a new invisible field, and clicking
  a signature field on the page starts the flow with that field selected. A
  visible signature is drawn inside the field's own box, on whatever page the
  document puts it, with correct coordinates on cropped and rotated pages.
  Signing saves unsaved edits first, and the signed copy can be opened from
  the result step.

  Signing, verification and countersignature work on documents built with
  cross-reference streams and object streams — what LaTeX, Chrome and
  "optimized" PDFs produce — and on the merged field/widget dictionaries and
  UTF-16BE field names Acrobat writes. Every signed shape validates under the
  pyHanko oracle.

- **A fullscreen Reader layout.** A third built-in, alongside Presenter and
  Reader: the Reader's own tree — the control band, the outline rail, the page
  — mounted with the chrome hidden and the whole page in view rather than
  fitted to the width. `f` and `Escape` both bring the band and the rail back
  without leaving the layout, so returning to fullscreen is one key away.
  Layouts can now carry what mounting them asks of the reading surface, which
  means a copy of this layout opens the way the original does.

- The foundation of document mode: pulpit can open an ordinary PDF, mark it
  with **native PDF annotations** and write the result out through Save As.
  A completed ink gesture becomes an `/Ink` annotation in the open document —
  not a flattened mark and not an overlay kept beside the file — and survives
  saving and reopening with its identity, geometry and style intact, in any
  standards-compliant viewer. Text highlighting writes a true `/Highlight`
  whose `/QuadPoints` describe the marked text runs, free text writes
  `/FreeText`, notes write `/Text`, and checks and crosses write `/Stamp`.
  The source file is never written: it is refused outright as a save
  destination.

  Underneath: canonical page geometry that survives every page rotation and
  crop box, stable annotation identity through `/NM`, one revision and one
  undo entry per user action with undo *restoring* an annotation rather than
  recreating it, and whole-or-nothing transactions so an eraser sweep that
  fails part-way leaves nothing applied. The AcroForm hazard corpus — 55
  documents that are each wrong in one named way — moved into the new
  development-only `pulpit-testkit` crate and runs against the engine.

  Document mode is a *layout*, not a second application: the new **Reader**
  and **Reader + Fields** built-ins sit beside the presenter layouts in the
  same designer, over the same store, and a PDF opening into one never
  changes which presenter layout is selected.

  Document mode is reachable: press **r** to move between reading a document
  and presenting it. Mode is which layout is mounted, not which document is
  loaded — nothing is closed and no revision changes — and the two modes
  remember their layouts apart, so choosing a presenter variant never changes
  what a PDF opens into. The reader scrolls continuously, zooms freely, and
  asks the document worker only for the pages in front of you.

  Behind it, an open PDF is held by a document worker: one document, one
  execution context, one process, which is another role of this same binary
  rather than a second one to install. Pages are rendered by the process that
  holds the mutated document, so a frame drawn after a commit contains the
  commit. A worker that goes leaves presentation mode alone.

  This is `SPEC-document.md` up to and including its ink milestone.
  Deliberately not here yet: form filling, which the specification now routes
  through PDFium's own form-fill environment and which is gated behind a spike;
  the live stroke preview, so a mark appears when its frame does rather than
  while it is being drawn; and crash recovery for unsaved annotations.

### Added

- **PDF forms can be filled, in place, by PDFium itself.** Clicking a field
  puts the caret in it and typing puts characters in it — with the field's own
  font, size, quadding, comb spacing and multiline wrapping, because the code
  doing the editing is the code that generates the appearance. pulpit draws no
  field editor of its own and never writes a value from outside the page, and
  that is the point: a second implementation of "what a filled field looks
  like" is a second implementation that will disagree with the first, and it
  disagrees exactly where the person filling the form can see one thing and
  the file will show everyone else another.

  A filled form saves and reopens with its values, in pulpit and elsewhere.
  Checkboxes and radio buttons are pressed, choice lists are chosen from,
  backspace deletes, and a committed value is one revision and one undo entry
  in the same history as the annotations. A read-only field is shown and
  refuses to change. The source file is never written.

  The AcroForm hazard corpus — 55 documents each wrong in one named way — now
  checks the fill promises it has always carried and not only survival, and
  all 23 of them are kept: comb and auto-sized fields, a field whose value is
  outside the basic multilingual plane, a multiline field, overlapping widget
  rectangles, export-value pairs, a multiple-selection list, and the read-only
  fields that must be shown and must refuse to change.

  Documents that carry JavaScript get none of it. The form-fill environment
  refuses every callback through which a PDF could reach outside itself: no
  script platform, no URL navigation, no email, no upload, no download, no
  file access, no document-driven menus. A form is a thing you type values
  into, and none of that is needed to type a value into one.

### Changed

- **Presenter marks are now annotations in the document.** A stroke drawn
  during a talk is committed to the open PDF when the pen comes up, as a
  native `/Ink` annotation — the same kind of mark document mode makes, in the
  same file, editable in both. It can be selected, moved and deleted
  afterwards, and undo runs one history across both modes in the order things
  were actually done.

  What this replaces: marks used to live in a per-slide cache in memory and
  were written out, if at all, by stamping them into a copy of the deck as
  page content — a second, private representation of the same thing. Both the
  cache and the stamping path are gone, along with the "Save an annotated
  copy" command, which is now simply the document's Save As: the marks *are*
  the document's annotations, so saving the document saves them.

  The unfinished gesture is unchanged and still never reaches the file. The
  pen follows the hand with no worker in the loop; the pointer, the spotlight
  and a half-typed label stay out of the PDF entirely. What did change is that
  a document pulpit cannot annotate can no longer keep marks at all — you are
  told once, when the first mark is made, rather than finding out afterwards.

  A mark also lands where it was drawn on a split-page deck, where the slide
  is half a physical page: the conversion between what the projector shows and
  where that is on the paper is one function, used in both directions, tested
  through a real PDF at every page rotation and crop.

- A mark made with one of the named ink colours comes back named after a trip
  through a PDF, rather than as an anonymous colour that happens to have the
  same value. A PDF stores three numbers and has no field for which swatch was
  chosen, so every named colour used to turn "custom" the first time it was
  read back, and the palette stopped showing which one was armed.

- Choosing which display the audience window uses is no longer claimed as a
  capability on any platform, and the compositor-specific adapter that
  implemented it on Niri has been removed. On a Wayland session the audience
  window goes fullscreen on whichever output it is already on and the user is
  told so, exactly as in any other tiling compositor. Under Niri it used to
  move itself, through that compositor's `niri msg` IPC — a second path
  through reconciliation that only one desktop exercised, in exchange for
  saving a single manual window move. Window placement on X11, Windows and
  macOS is unchanged.

## [0.0.4] — 2026-08-15

### Fixed

- Browser overlays are no longer stretched and cropped on a high-density
  display. The viewport asked of the browser was bounded per axis, so a large
  overlay on a 2× display had only its long edge shrunk to the 4096-pixel
  limit and reached the browser with a different aspect ratio than the
  rectangle it is drawn into — the picture arrived distorted, with its edges
  outside the frame. The bound now applies to both axes at once, and a frame
  whose shape still does not match its viewport is fitted with bars rather
  than stretched to fill it.

- The release workflow now actually publishes the Homebrew Cask. It rendered
  `Casks/pulpit.rb` into a clone of the tap and then tested for changes with
  `git diff` *before* staging, which does not see an untracked file, so every
  release through 0.0.3 reported "the cask is already at …" and pushed
  nothing. `brew install --cask vincentarelbundock/tap/pulpit` answered "No
  casks found". The step stages first, and afterwards reads the cask back out
  of the tap through the GitHub API and fails the release if the tagged
  version is not being served.

### Changed

- macOS install instructions no longer pass `--no-quarantine`, which Homebrew
  removed in 4.5. A cask install costs the same one-time trip through System
  Settings → Privacy & Security as the disk image does; no flag skips it.

## [0.0.2] — 2026-08-14

### Changed

- The application crate is named `pulpit`, not `pulpit-app`, so `cargo install
  pulpit` installs the `pulpit` binary. The crate directory moved to
  `crates/pulpit/` and its redundant `[[bin]]` block is gone — the package name
  already names the binary.

### Fixed

- Every crate now carries the `description` and `repository` metadata
  crates.io requires. Without them `cargo publish` refuses the upload, which
  the 0.0.1 publish would have hit as soon as its registry token was set.

## [0.0.1] — 2026-08-14

The first published release. A complete presenter: two-window presentation,
display reconciliation, presenter layouts and their designer, speaker notes,
PDF links, presenter annotations, session recovery, and media overlays for
animated images and interactive HTML.

### Added

- The macOS disk image carries one universal `Pulpit.app`: the binary and the
  bundled libpdfium both have an arm64 and an x86_64 slice, so Intel Macs are
  supported by the same download. `make app-universal` builds it and CI
  asserts both slices are present.

- Every package now says where media comes from. The Nix build pins libmpv
  and a Chromium-family browser, the Homebrew cask installs mpv alongside the
  app, and the `.deb`, `.rpm` and AUR packages recommend mpv beside the
  browser they already recommended. Nothing is bundled and nothing is
  required: pulpit `dlopen`s libmpv and spawns the browser as a child, so a
  deck with no media needs neither.

### Fixed

- Slide changes no longer flicker. The audience window holds its last frame
  instead of dipping through a coarse render, presenter panels change texture
  only for a meaningful improvement instead of blinking through the whole
  coarse-to-refined ladder, navigation no longer cancels renders it still
  needs, and stepping backward is prefetched as thoroughly as stepping
  forward.

### Changed

- Deck thumbnails are rendered in a single pass at one width — sharp enough
  for the slider's preview card, chosen per document so the whole deck fits
  the budget — and never change afterwards, replacing the two-level
  coarse-then-refine warming.

- Renamed the package, its crates, its environment variables and its URI
  scheme from `teleprompt` to `pulpit`.
- Reorganized the documentation: prose sources now live in `docs-src/` as
  Typst and are compiled to `docs/` with Calepin (`make website`). The root
  `ARCHITECTURE.md` and the `docs/*.md` files were folded into
  `docs-src/internals.typ`, `docs-src/install.typ` and `docs-src/usage.typ`.
- Rewrote the `Makefile` with self-documenting targets (`make help`) and added
  `website`, `serve`, `version`, `bump` and `release`.
- Collected every licence text in `LICENSES/`, with a `README.md` there saying
  which licence covers which part of the package. Replaces the root
  `THIRD-PARTY-LICENSES.md` and the licence files that sat inside the vendored
  directories.
- `AGENTS.md` is now a symlink to `CLAUDE.md`, so agent guidance cannot drift
  between the two.
- `logo.svg` is used across the package: the README, the website logo and
  favicon, and the application icon in `packaging/pulpit.svg`, which is now
  the same artwork on a badge instead of unrelated placeholder shapes.
