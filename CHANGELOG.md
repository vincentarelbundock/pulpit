# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Shapes: a box, an ellipse, a line and an arrow.** One tool in the document
  palette with four modes, drawn by dragging, in the pen's own colour and
  width — the same shape the highlighter's three nibs and the band's three
  kinds have. A box becomes a `/Square` and an ellipse a `/Circle`, the
  annotations PDF has for exactly them, so a marked-up document opens as
  shapes in Okular and Acrobat rather than as a picture. A line and an arrow
  are strokes of ink, because `/Line` keeps its geometry in arrays PDFium
  cannot write and a malformed annotation travels worse than an honest
  stroke; the arrowhead is drawn as part of the same stroke, so it is one
  mark to select, move, resize and erase. All four are movable and resizable
  afterwards like any other mark.

- **The stamp can be reached.** A check and a cross, placed with one click,
  centred on where you clicked and resizable afterwards. The tool existed and
  was not in the palette, and the two marks it puts down had no appearance at
  all — a check placed before this was an annotation in the file and on
  nobody's screen. It is never described as a signature.

- **The marks in a document are listed.** A third tab in the sidebar that
  already holds the outline and the search results — its own icon, no
  shortcut, and offered only for a document that can carry marks — lists every
  annotation in the file in page order: what it says, what kind it is and what
  page it is on. Pressing a row goes to the mark and picks it up; the trash
  beside it takes the mark out of the document as one edit, which is the only
  reasonable way to remove a mark from a page nobody is looking at. Marks the
  document arrived with are listed beside your own and say when pulpit will
  not rewrite them — `read-only`, `not editable here`, `malformed` — rather
  than offering a control that would refuse. The list walks the whole document
  a chunk of pages at a time and fills in as the answers arrive, and it
  follows the document's revision rather than a timer, so a mark deleted on
  the page leaves the list in the same instant.

### Fixed

- **A page can no longer stay blurry for the rest of the session.** The
  application asks for a page's sharp frame once and waits for the answer
  before ever asking again — and three supervisor paths could drop a render
  job with no answer at all: a shared region that could not be sized (a full
  `/dev/shm` at open, most plausibly, and only the big refined frames travel
  through the region — the coarse stand-ins always arrived), a submit below
  the generation floor, and a queue deduplication that discarded an
  identical-looking job from a *different* requester. A job swallowed that
  way left its frame permanently "in flight": every later plan skipped it,
  and the page — typically the first pages on screen when a document opened —
  kept its coarse stand-in for as long as it was looked at. Every job that
  enters the supervisor is now answered — rendered, failed or cancelled — so
  the next plan simply asks again, and the deduplication is gone: the rare
  genuine collision renders twice, which is cheap next to a page that never
  sharpens.

- **Exporting a document no longer writes through a planted symlink.** Saving
  a PDF creates a scratch file beside the destination first, so an interrupted
  save cannot leave half a document where a whole one was. That scratch file
  was created with a predictable name and an ordinary open, which follows a
  symlink and truncates whatever it finds — so in a directory another local
  user can write, the export could be redirected into a file pulpit was never
  asked to touch. It is now created exclusively, under a name that cannot be
  guessed from the process id, and a name somebody else has taken is a refusal
  rather than a write. The same seven steps had been written four times across
  the tree and had drifted apart; they are now one primitive that every writer
  uses, which also gives the settings, layout and session files the durability
  fsync after the rename that two of the four were missing. Permissions are
  unchanged: a file of the reader's own stays owner-only, and a document they
  asked us to export still takes their umask.

- **The media worker's shared-memory rings are no longer world-readable.** A
  ring carries decoded video frames of whatever is on the audience screen, and
  it lives in `/dev/shm`, which every local user can write to. The render
  crate had long created its regions with an unguessable name, `create_new`
  and mode `0600`; the media crate, doing the same job with a different data
  structure, had none of the three — its rings were named
  `pulpit-media-<pid>-0` upwards, adopted an existing file rather than
  refusing it, and took whatever the umask gave, usually `0644`. Another user
  on the same machine could read a ring, or pre-create one at the name the
  worker was about to use. The two now create their regions on identical
  terms. Found by comparing the two implementations rather than by a report;
  the rule it produced is in `docs-src/internals.typ` under *Superfluity*.

- **A typeset text mark no longer disappears when it is dragged.** Every edit
  to an annotation clears the picture the engine is holding for it, and a
  stamp is a kind nothing redraws by itself — so moving a mark whose
  appearance pulpit could not rebuild left an annotation in the file that
  nothing draws. Pulpit now records which mark it placed, redraws it wherever
  it lands, and holds rather than drags the marks it cannot draw again:
  another producer's stamp, and a text mark's rendered picture, which is
  rewritten by editing its source rather than by moving it.

- **Shared memory is reclaimed after a crash with a media overlay playing.**
  Pulpit sweeps the shared-memory files left by a previous run whose process
  is gone, and the sweep could not read the names the media overlays use — so
  it reclaimed the renderer's and skipped theirs. The rings a crashed
  presentation left behind stayed in `/dev/shm` until the machine was
  rebooted; on a 4K overlay that is tens of megabytes each time. The sweep now
  reads every naming scheme pulpit writes, and the names come from one place
  so a future one cannot be missed.

### Changed

- **One row of sidebar tabs, and no Pages view.** The sidebar's icon row is
  now the only level — Outline, Fields (where the document has a form),
  Search, Annotations — replacing the second row of text tabs that used to
  sit inside the outline pane, a split within the split. The page-thumbnail
  view is gone with it: the overview page already shows every page as a
  thumbnail, and a second, smaller copy of it in a rail earned its column
  width twice.

- **Launch gets out of its own way.** Everything optional the session wants
  to know — which browsers and media runtimes are installed, whether a
  speech synthesiser and voices are on disk, the desktop's appearance and
  motion preferences, the full capability snapshot, even the wall clock's
  UTC offset — is now probed a beat *after* the presenter window opens,
  instead of contending with the compositor for CPU, disk and the dynamic
  loader while the first frame was being built. The window is built from a
  conservative capability snapshot that only ever under-claims, and adopts
  the full answer moments later; a probe still in flight when a
  suspend/resume refreshes the same preference is recognised as stale and
  dropped. What used to be five serial D-Bus connections before there was a
  window to close is now zero, with the one bounded bus trip on a helper
  thread. The log carries startup marks (`stage`, `elapsed_ms`) so a launch
  regression can be measured rather than felt.

- **Dead code is visible again, and about a thousand lines of it are gone.**
  Five subsystems — `layout`, `settings`, `doc`, `media` and `platform` — had
  the dead-code lint switched off for the whole module, so nothing unused in
  them had warned for a long time and nothing new would. The lint is back on
  everywhere; the 132 items it finds each carry their own note saying whether
  a test still reaches them, and 50 of them turn out to be reached by nothing
  at all, which is not what the blanket exemptions claimed. Deleted outright:
  the widget patch layer, which nothing had sent a patch through and which the
  designer has no panel to send one from; 28 unused items across the four
  library crates; a vendored CSS colour table and two other modules left over
  from a widget that was never ported; and four dependency declarations, one
  of which was compiling a second redundant copy of a crate. Nothing about how
  pulpit behaves changes. The rules the exercise produced — how an allowance
  is written, and what a clean audit may and may not claim — are in
  `docs-src/internals.typ` under *Superfluity*.

- **The worker machinery is written once.** Message framing, process
  spawning, the wake-up doorbell and shared-memory naming had grown four
  separate copies across the render, media and application crates, which is
  how the leak above went unnoticed in one of them and how a safety check that
  stops a worker process spawning further workers came to be skipped at one of
  its four sites. There is now one of each, and a test that fails if a new
  worker is added without that check. Nothing about how pulpit behaves changes
  — this is the kind of tidying that stops the next bug rather than fixing a
  visible one.

## [0.0.10] — 2026-08-26

### Added

- **Text can be selected, copied, and read aloud.** A new *Select text* tool —
  digit `8` in both palettes, wearing the I-beam — sweeps the page's own text
  exactly as the highlighter does and leaves no mark: what it holds outlives
  the drag, lit up on the page in both modes and for the audience.
  `Ctrl+C` (`Cmd+C` on macOS) copies it, and the read-page key reads the
  selection instead of the page while one is held — the menu row now says
  "Read page or selection". The selection is put down by the things that
  would make it stale: a new gesture, a change of tool, a page change,
  Escape.

- **Pulpit can read a document aloud.** `R` reads the whole document, turning
  pages as it goes; `Shift+R` reads the current page; either key pauses its
  own reading, and `Shift+←`/`Shift+→` step by sentence. Speech follows the
  document's language — a page confidently in another tongue switches to a
  voice for it, or offers to fetch one — and a pinned language, the speaking
  speed and the voice live in Settings ▸ Speech. Voices are downloaded on
  demand from a catalog of 139 across 45 languages, each verified against a
  checksum pinned in the binary before first use; nothing is bundled, nothing
  new is linked, and a session that cannot speak says why instead of greying
  out. The synthesiser and the audio player are installed programs driven as
  child processes, so stop is immediate and quitting pulpit silences it.
  (Rotating pages moved from `Shift+R` to `Ctrl+Shift+R` to make room.)

- **Pages turn on their own.** `P` starts an unattended loop and stops it: the
  poster in the corridor, the lobby screen, the deck left running before the
  room fills. The dwell, whether it starts again at the first page, and
  whether a hand on the controls holds it are in Settings, because the person
  who sets a screen up in the morning is not the person who launched it.

  It is not a mode and not a layout. Autoadvance turns the page in whichever
  viewer is up — presenting or reading, fullscreen or in a window, either
  entered before the other or after — and in whatever that viewer has open: a
  deck, a book, a scan, a comic archive, a folder of images. There is one
  place in pulpit that means "forward", and this is a clock pressing it, so
  there was nothing to teach it about formats.

  What it will not do is turn a page out from under someone. Anything that
  owns the keyboard — a mark being written, the overview grid, the search
  pane, a form field with the caret in it, a dialog, the settings page — holds
  the loop while it is up, and with holding on, a key, a click or the wheel
  puts it aside until you press `P` again. The dwell is the page's, not the
  renderer's: a page that took half a second to draw still gets its full time
  on the screen, and a laptop that spent an hour with its lid shut owes one
  page turn rather than seven hundred.

  A running loop asks the session to stay awake, which it used to do only for
  a fullscreen audience window — an unattended screen that blanks itself after
  ten minutes is not unattended. Without wrapping it stops at the last page
  and says so, rather than ticking at a page that will not turn.


- **Ctrl+P prints.** Pulpit could not print at all: the only `Print` in the
  tree was the one it refused when a form's own script asked to print itself.
  That refusal stays — a script asking is not the reader asking — and beside
  it there is now a print path.

  Where your desktop has a print dialog of its own, that is the dialog you
  get — the same one every other application on the machine opens, with your
  printers, page ranges, copies, duplex, paper sizes and trays in it. Pulpit
  asks one question first, because no system dialog can ask it: whether the
  paper carries what you have done to the document, or what is saved on disk.
  Then it hands the file over and your desktop takes it from there. On Linux
  that dialog is the desktop portal's; on macOS it is the standard print
  panel, with a preview of the pages in it.

  Where there is no system print dialog, pulpit asks the questions your
  spooler will actually honour — which pages, how many copies, which
  printer — because otherwise nobody asks and you find out at the printer.
  Windows is that case today: its shell print verb takes a file and nothing
  else, so a page range or a second copy is refused there by name rather than
  printing the whole document and letting you discover it on paper.

  Printing "with my marks and entries" prints what is on the screen, not what
  was last saved: the annotations you have drawn and the form fields as you
  have filled them, including the characters still in the field the caret is
  in. Pulpit writes a temporary copy carrying them, sends that, and deletes
  it. That copy is never offered as your document, and for a signed document
  it is not the signed one.

  A document whose permission bits ask that it not be printed says so, and
  prints if you say so. Quietly ignoring the request and quietly obeying it
  are both worse than showing you what the document asked for: every other
  reader on your machine will print the same file, so pretending to enforce
  it would be a pretence.

  Printing is a capability, not an assumption. A session with no spooler says
  so plainly instead of appearing to print into nothing. On Linux, waiting at
  a print dialog no longer stops the rest of pulpit: the job goes out on its
  own thread, so both windows keep drawing while you choose a printer. On
  macOS the panel is modal, as AppKit requires, and the audience window holds
  the frame it had until you are done with it. Cancelling a print dialog says
  nothing at all, because cancelling is a decision rather than a fault.

- **DjVu books open.** `.djvu` and `.djv` are read the way a PDF is: pages
  turn, render, zoom and fit, the overview grid is a contact sheet of the
  book, and pulpit remembers where you were. Scanned books are exactly the
  case document mode was built for, and they were the one common format it
  could not open.

  DjVu is a capability of your machine rather than of the build you
  downloaded. Pulpit carries no DjVu library of its own and never will —
  PDFium is the single exception, because it is the reason the application
  exists. It looks for an installed djvulibre each time it opens a book, so
  installing one is the whole setup: nothing is configured and nothing is
  rebuilt. A machine without one is told the file is a DjVu and how to
  install the library for its platform, which is a different thing from being
  told the file is damaged. A missing DjVu library never stops a PDF or a
  folder of images opening, and a missing PDFium never stops a DjVu.

  A DjVu is read-only: no annotations, form fields, text selection, search or
  signing. Those are PDF semantics, and pulpit refuses them by name rather
  than faking them. A book shows as *View only* and the tools that do not
  apply are not offered, rather than refusing when pressed. To mark up a
  scan, convert it first — `ddjvu -format=pdf book.djvu book.pdf` ships with
  djvulibre.

- **There is somewhere to find out what a document is.** *Properties…* in the
  hamburger opens a dialog holding the file's own account of itself: title,
  author, subject and keywords, what created it and what converted it, when it
  was made and last changed, its page count, its page size named as the sheet
  it is, and the PDF version it declares. A key the document left empty is
  left out rather than shown as a blank row, and a date that does not parse is
  shown as the document wrote it rather than dropped.

  The permissions are the part that decides things: where a file is encrypted,
  its handler is named and each of the eight operations it declares is listed
  as allowed or refused — in words, not by colour — so it is clear before an
  edit or a print is attempted whether the document will take it. A last
  section says what pulpit will do with what it found: the compatibility
  level, every standing warning, and the capability findings a presenter would
  rather read before the talk than meet as a toast during it.

  The strings in there were written by whoever produced the file, so they are
  bounded and flattened on the way out of the engine exactly as an
  annotation's text is: no producer can lay out the dialog that shows them.
  Nothing is asked of the document until the dialog is opened, so a deck going
  onto a projector pays nothing for it.

- **The select band can copy the page, not only pick marks off it.** The
  rubber band now has a kind, chosen in its own options the way the
  highlighter's colour is: *Marks* holds the annotations it encloses, as it
  always has; *Image* copies the region to the clipboard as a picture; *Text*
  copies the text the region covers. The image is rendered fresh at twice the
  page's own scale rather than lifted off the screen, so what you paste does
  not depend on the zoom you happened to be reading at, and the text query
  bounds an area rather than following the reading order — which is how one
  column comes off a two-column page. The kind is one setting across both
  modes, like the pen's colour, and a session that cannot carry an image on
  its clipboard says so instead of copying nothing. Table extraction is
  deliberately not offered.

- **The highlighter underlines and strikes out.** Highlight was the only text
  markup pulpit could make, and the other two every reviewer reaches for are
  the same mark with a different subtype. They are not three tools: the
  highlighter now has a kind in its options, the way the band has one, and the
  colour control applies to whichever is chosen — a rail already holding six
  tools did not need two more.

  A retained underline is drawn as a rule sitting on the text rather than as a
  translucent wash over it, because a strikeout is opaque and an underline is a
  line; inheriting the highlight's translucency would have made all three look
  like the same smudge. They import as well as export: a document that arrives
  carrying an `/Underline` or a `/StrikeOut` is understood, drawn and erasable,
  rather than a mark pulpit can only decline to touch.

  Squiggly is deliberately not offered.

### Fixed

- **A copied region pastes into a file manager.** On Wayland the select
  band's image now goes out three ways at once: as `image/png` for anything
  that pastes pixels, and as a freshly written PNG file under `text/uri-list`
  and `x-special/gnome-copied-files` for anything that pastes files — which
  is what Thunar and its siblings ask for, and why the copy used to paste
  into GIMP and into no directory anywhere. Pasting into a file manager now
  drops a PNG there; image editors still receive the pixels directly.

- **Hover hints inside tool panels appear.** The popover the panels hang
  from never forwarded its contents' overlays, so every tooltip on a control
  inside one was silently swallowed. The panels also now close when the
  window loses focus, since a click into another window is a click off the
  panel that arrives with no press to see.

- **Text written on a slide is kept.** A label typed at the lectern was drawn
  on the screen and then thrown away at the next page turn: nothing ever
  committed it, so it never reached the file. It now becomes an annotation in
  the document when it is finished, exactly as a stroke does — and it is
  finished by Enter, by starting another label, by reaching for another tool
  and by pressing undo, each of which used to lose it silently. Escape still
  makes nothing, which is what escape is for.

- **A highlight made while presenting stays on the screen.** The mark was
  committed correctly and then drawn by nobody: a slide is rendered without
  annotations, and the overlay knew how to draw ink and nothing else. It now
  draws every kind the document holds for the page — highlights, notes and
  labels as well as ink — so a mark made in either mode, or one that was in
  the PDF before pulpit opened it, is on the slide and on the projector.

### Changed

- **Choosing a tool option is choosing the tool.** Picking a colour, a band
  kind or the pointer's dot-or-spotlight closes the panel it was picked in
  and arms the tool concerned: the hand that chose red for the pen means to
  write with it. The sliders — width, size, radius — still hold the panel
  open, because they are adjustments rather than answers, and an option
  picked for the tool already in hand leaves its open gesture and held marks
  exactly where they are.

- **The band's kinds are pictures with hints.** The three choices are drawn
  as a lasso gathering marks, a picture and text in a dashed frame, each
  saying its one word — Annotations, Image, Text — on hover instead of
  wearing it as a label.

- **The theme's colours read as a grid.** The settings palette sits two
  roles to a row, each cell a name over its swatch and hex field, with the
  role's description on the name as a hover hint. Seven stacked fields
  scrolled past a screen to say what the grid says at a glance.

- **Both modes offer the same annotation tools.** Presentation gains the
  sticky note and the rubber band, so every mark document mode can make can be
  made at the lectern; the pointer and the spotlight stay presentation's,
  because they make no mark at all. The band holds what it encloses and the
  delete key takes all of it in one press — one undo, however many marks.
  Moving and resizing a held mark stays document mode's.

  The tool digits now mean the same thing in both modes: 1 holds, 2 draws, 3
  highlights, 4 writes, 5 leaves a note, 6 erases, and 7 points. A colour
  chosen for a tool in one mode is that tool's colour in the other.

  The eraser and "clear this slide" reach every mark on the slide rather than
  only the ink, so a highlight or a note on the page can be taken back from
  presentation too.

- **Every key descends one ladder.** Which surface owns a press — a label
  being typed, held marks, the overview grid, a text box with the caret, the
  open panels innermost first, the document viewer, and finally the
  shortcuts — was decided by a long chain of hand-ordered checks; it is now
  one written-down priority order, walked top rung first. Nothing moves on
  the keyboard: the same key still means the right thing in each place, and
  Escape still closes the nearest open thing before the next one out. What
  changes is that the order is now something the tests hold still, so a new
  panel or popup cannot quietly steal a key from an old one.

  On macOS, Command and Control are no longer the same key to pulpit: ⌘Q
  quits and ⌘F finds, as they should, and a Control combination is not
  mistaken for them. Elsewhere nothing changes — Ctrl remains the commanding
  modifier, and saved keymaps carry forward as they are.

  Ctrl+B and Ctrl+F now reach the outline and the search from any text box,
  not only the search box: a commanding chord is a command wherever the
  caret is, while a bare letter in a field stays a letter.

## [0.0.9] — 2026-08-25

### Changed

- **Search is fast enough to type into.** Every keystroke in the find box
  restarts the document scan, so each of the things that made one scan slow
  was being paid once per letter — and on a long deck that read as an
  application thinking rather than a search running.

  A running scan now keeps the fast tick, and the next run of pages is asked
  for the moment the previous answer lands rather than on the following tick;
  a five-hundred-page deck used to spend seconds waiting for a timer instead
  of for the worker. Typing settles for a tenth of a second before a scan
  starts, so a word is one scan and not six, while the box, its options and
  the hits from your speaker notes stay live as you type. Three runs of pages
  are in flight at once and the first covers four pages rather than
  thirty-two, so the first hits arrive in a round trip. The scan starts at the
  page you are on and wraps: the hit somebody searching from page 300 wants is
  usually near page 300, not on page 1. A scan whose query you have already
  typed past is dropped rather than run to completion in front of the one you
  are waiting for.

  Underneath, each page's text is extracted once and kept. Building a page's
  text layer is the expensive half of searching it, and it was being rebuilt
  for every query; the second query over a document now asks the PDF engine
  for nothing at all on the pages that do not match, and only for the
  rectangles it draws on the pages that do. Searching a document you are
  reading is unchanged in what it finds — page text, notes and bookmark titles
  now go through one matcher rather than three — and every mark still lands on
  the text it matched.

### Fixed

- **Search results follow the document when it changes.** A result is a place
  on a page, so when the file you are watching is rebuilt, the results found
  in the old one describe where the words used to be: the list points at text
  that has moved, and the highlights on the page mark bare paper. The page
  count went stale with them, so a rebuild that added pages had pages the
  search would never look at. A reload now runs the query again over the
  document that arrived, keeping what you typed — a deck is rebuilt while you
  are looking for something in it. Opening a different document clears the
  search instead of keeping the previous file's results to draw over the new
  one's pages.

- **Windows: a second copy can name the one that is running, and a released
  claim cleans up after itself.** The file a running instance uses to claim
  the projector was opened in a way that locked the instance out of reading
  its own record. A second copy was refused correctly but could only say that
  *something* held the claim, never which process, and an instance that closed
  cleanly left its claim file behind for good.

## [0.0.8] — 2026-08-24

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

- **The timer carries its own controls.** Starting, holding and resetting it
  needed the keyboard or a trip through the settings panel, which is a long
  way to reach for a thing the presenter looks at every minute. The timing
  panel's footer is now two lines: what the reading is doing, and beneath it
  a row with play/pause, reset and the gear that was already there. The glyph
  says which way the press will go — `play` while the timer is stopped,
  `pause` while it runs. Two lines rather than one so the buttons keep their
  place: a caption that changes from "counting up" to "counting down · 20:00"
  would otherwise slide them sideways under a finger already on its way down.
  The alarm line beneath the clock is built from the same footer, so the pair
  still read as a pair.

### Removed

- **The menu's separate "Diagnostics…" entry.** It sent the reader to
  Settings, exactly as "Settings…" three lines above it does, and what it
  promised is a section of that page. Two entries for one destination is a
  menu that has to be read twice to learn they are the same place.

- **The notice about the audience window covering the presenter view.** A
  toast is drawn on the presenter window and nowhere else, so a notice about
  that window being covered sits under the thing covering it. It is still
  written to the diagnostics bundle.

### Fixed

- **Pages are fitted to the window the compositor gave, not the one pulpit
  asked for.** The application listened only for resize events, and a window
  that is placed at its final size and then left alone never sends one — the
  ordinary case on a tiling compositor, where the whole session then fitted
  its pages to a size no window ever had. The size a window reports when it
  opens is the same fact as a resize, and is now read as one.

- **Entering fullscreen fits the page to the fullscreen window.** The reader
  trusts a page surface's own report of its height over the layout's estimate,
  which is right until the surface is replaced by one that never reports: a
  page fitted to its window gives its scrollable nothing to scroll, and a
  scrollable with nothing to scroll publishes no viewport at all. Fullscreen
  was therefore fitted to the window it was entered from. A remount now
  retires the departed surface's report, and the layout's own measurement
  takes the cell back until some surface speaks again.

- **The timer's overtime pulse fades instead of stuttering.** It asks for a
  frame the same way the alarm's does, but only the alarm was on the list of
  things that keep the presenter animating, so overtime arrived in about five
  steps.

- **A browser that is merely starting is no longer mistaken for a wedged
  one.** Every command to the media browser had the same ten-second budget,
  including the ones that bring it up — process start, profile creation, GPU
  and renderer init, the first paint of a page. That is seconds of real work
  on an idle machine and several times that on a loaded one, so on a busy
  machine an overlay could fail to start with "the browser did not answer in
  time" while nothing was actually wrong. Bring-up now has a budget of its
  own, three times the other. Steady-state commands keep the short one: past
  ten seconds a browser mid-presentation is wedged, not slow, and that is the
  worst moment to find out slowly.

- **Leaving fullscreen no longer leaves the page soft.** The reader's cell
  shrinks on the way out, so the sharp frames rendered for the full screen no
  longer fit it and the coarse previews do — and the frame chooser took the
  one that fit, whatever its quality. Nothing replaced it either: those same
  wide sharp frames were counted as already satisfying the narrower request,
  so no new render was ever asked for, and the page stayed soft for as long
  as it was looked at. Quality now outranks fit. A sharp frame wider than the
  cell is downsampled on its way to the screen and still looks like the page;
  a coarse one that happens to fit is upsampled and does not.

- **The navigation keys move the document being read.** Home, End and the
  arrows were bound to first slide, last slide, next slide and previous slide
  whatever was on screen, so in a reading layout they walked the deck behind
  the document and did nothing visible — and mid-talk, End would have jumped
  the projector to the last slide. They now move the document: Home and End to
  its first and last page, the arrows one page at a time, the same split the
  Back and Forward controls have always made. Home and End are recorded in
  history, so Back returns you to where you were reading; stepping is not,
  which is what it already meant for a deck. PageUp and PageDown still scroll
  by a screenful, and every key means what it always did wherever slides are
  the primary view.

- **The panel over the write that precedes signing no longer flashes.** The
  edits have to reach the disk before a signature can be made from them, and
  nothing may change the document while they do — a stroke drawn then would be
  missing from the signed copy with nothing saying so. The surface is still
  blocked from the first millisecond; what waits is the sheet explaining why,
  which now appears only if the write takes longer than a moment. Most writes
  are over before it is shown at all.

- **The hand still works in a signed document.** Append-only mode refused the
  page-surface press and release outright, but those are the gesture, not what
  it does: with no tool armed a press is the hand, which pans the page,
  follows a link, drags out a crop marquee or selects text. None of that
  touches the document, and refusing it took the hand away from every signed
  document — which is most of what reading one consists of. What a gesture may
  go on to commit is now refused where it commits, at the two choke points
  every change passes through. That is both narrower and wider than the old
  list: reading is untouched, and it also covers what never came through that
  list at all, such as typing into a field reached with Tab.

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
