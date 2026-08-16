# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Keyboard shortcuts are vim-first, and modifiers are now real.** Navigation
  answers to `j`/`l` and `k`/`h` as well as the arrows, space and the paging
  keys — nothing conventional was taken away, because no presenter remote
  emits a letter and the person driving the laptop is not always the person
  who chose the bindings. `G` goes to the last slide.

  Everything that leaves the deck takes the primary modifier the way every
  other application does: `Ctrl+O` to open, `Ctrl+R` to reload, `Ctrl+Q` to
  quit, `Ctrl+F` for audience fullscreen. A bare `q` next to `w` was a talk
  ended by a typo. Undo and redo of a stroke follow the editing convention
  rather than the vim one — `Ctrl+Z` and `Ctrl+Shift+Z`, with `u` and
  `Ctrl+Y` alongside — because a stroke is an edit.

  Displaced by the navigation keys: the slide overview moves from `j` to
  **`o`**, and the layout library from `l` to **`Shift+L`**. The timer is `t`
  and resets with `Shift+T`. Stepping focus through a slide's links loses its
  default keys — it is rare enough not to earn a bare letter — and stays
  bindable for anyone whose decks lean on internal navigation.

  Underneath, a binding now carries its modifiers as data instead of gluing
  them onto the key name as `"ShiftZ"`. That spelling could not express two
  modifiers at once, and it could not tell a modifier that is load-bearing
  from one that is incidental: `Shift`+`b` must still blank the screen for a
  presenter resting a finger on shift, while `Ctrl`+`Q` must never reach a
  binding written for a bare `q`. Stored keymaps using the old spelling are
  migrated as they load.

### Fixed

- **Reader mode had no working key.** `r` was bound to the timer reset *and*
  to reader mode, and since resolution takes the first match, the documented
  `r` shortcut never fired. The timer reset moved to `Shift+T` and `r` now
  means what it says.
- **`Enter` never committed a previewed slide.** It was bound to advance, and
  the commit was written against `"Return"` — a key name the toolkit does not
  emit — so both bindings were dead. `Enter` now commits the preview; Next
  keeps its six other keys.
- A keymap check now asserts that no key is bound to two different actions,
  and that every default binding resolves to the action it was written for.
  Both bugs above were the same missing guard.

### Added

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
