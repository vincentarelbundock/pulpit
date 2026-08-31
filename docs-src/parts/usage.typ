
= Layouts

The screen is not hard-coded. It is a layout: a tree of splits and cells with
a widget in each cell, rendered proportionally into whatever window it lands
in. Two built-in layouts ship with the application, namely Reader and
Presenter, and either can be duplicated into an editable copy.

The two are modes rather than variants of one another: a document is either
being read or being presented, and mounting the other layout is what moves
the open file between them. `l` cycles through the layouts and `Shift+L`
opens the library. The choice is the file's, not the application's, so a
document you switch to Presenter opens that way next time.

== Formats

Pulpit opens several kinds of thing, and they all become the same thing once
open: a document with a fixed number of pages, each with a fixed size.

#table(
  columns: 2,
  table.header([What you open], [What you get]),
  [`.pdf`], [The full reader: text, links, outline, search, annotations,
    forms and signatures.],
  [A folder of images], [Its pages are the images directly inside it, in
    natural name order — `img2` before `img10`. Subfolders are ignored.],
  [One image file], [The folder it sits in, opened on that image. Pulpit
    says which folder it resolved to and how many pages that is, before you
    navigate anywhere.],
  [`.cbz`, `.cbt`], [A comic archive, read exactly like the folder it
    contains — including chapter subfolders, which are flattened and read in
    sorted path order. Nothing is unpacked to disk.],
  [`.djvu`, `.djv`], [A DjVu book — the format most scanned and archived
    books come in. Needs djvulibre installed on the machine; see below.],
)

Images are read by extension: `png`, `jpg`, `jpeg`, `gif`, `bmp`, `webp`,
`tif`, `tiff`, `qoi`, `tga`, `ico`, `pnm`, `pgm`, `ppm` and `pbm`. An
animated GIF or WebP shows its first frame — a presenter window is not a
media player. SVG is not in the list: it is vector content needing a full
renderer, which is a different decision.

Anything that is not a PDF is *read-only*. There is no text layer to search
or select, no annotations, no form fields and nothing to sign, and Pulpit
dims those controls rather than letting you press one and be refused. Notes
are always slides-only, whatever your default mapping is, so a photograph is
never cut down the middle with half of it treated as speaker notes.

Everything else about the application is unchanged: the overview grid is a
contact sheet of the folder, aspect fit handles pages of different shapes,
and a folder or archive that changes on disk reloads the way a rebuilt deck
does. On a reload a page is identified by its *file name* rather than its
number, so adding a picture earlier in the order does not change the one on
the projector.

`.cbr` is not supported and never will be: RAR needs the unrar library, whose
licence this project cannot carry. `.cb7` is not supported yet. Both are
refused by name, so you are told what the format is rather than that your
file is damaged — repacking either as a `.cbz` opens here. PostScript, XPS,
DVI, EPUB and the other reflowable formats are not supported; the reasoning
for each is in the internals document.

== Reading

A PDF opens in the Reader: one continuous document, with a side rail for the
outline and a search box that covers the pages, the outline and the speaker
notes at once. Long documents stay responsive because page text arrives from
the render worker a chunk at a time, while the outline and notes, already in
hand, answer immediately.

- Outline and thumbnails in the side rail (`Ctrl+B`), for documents that
  carry one.
- Search (`Ctrl+F` or `/`), stepping through matches with `F3` and
  `Shift+F3`, or `n` and `N`.
- Page view: zoom in and out, actual size, fit page, fit width, rotate, and
  a two-page spread.
- Internal links are followed by clicking, and back and forward return you
  where you were, which is not the same as turning back a page.
- Where you were is remembered per document: page, zoom and side rail come
  back the next time you open that file.
- The *Open document* section of Settings says what the open file is: title, author,
  subject and keywords, what produced it and when, page count and page size,
  PDF version, and whether it is encrypted. Where a document is encrypted,
  every permission it declares is listed as allowed or refused — which is
  what decides whether an edit or a print will be taken. A key the document
  left empty is left out rather than shown as a blank row, and the last
  section says what Pulpit will do with the features the file declares:
  transitions it cuts through, scripts it does not run.

Several copies of Pulpit can run at once, so a file clicked while a window is
already open gets a window of its own rather than being refused. Each copy
keeps its own crash-recovery record, and a copy that stops running leaves its
unsaved edits to be offered back by the next one that opens that file.

Pulpit watches the file and reopens it when it changes, which is what makes
it usable beside `typst watch` or a LaTeX loop. A watch event is a hint,
never proof: the file must settle before it is read, the rebuilt document is
opened and rendered out of sight, and it replaces what is on screen only once
a complete frame of it exists. Your page, timer and blanking survive the
swap, and the page is clamped if the document got shorter.

=== DjVu

Pulpit also opens DjVu books, the format most scanned and archived books are
distributed in. Both `.djvu` and `.djv` are recognised. Pages turn, render,
zoom and fit exactly as a PDF's do, the overview grid works, and the reader
remembers where you were.

Two things are worth knowing before you open one.

*You need djvulibre installed.* Pulpit does not carry a DjVu library of its
own; it uses the one already on your machine, so the format works on a
computer that has it and is refused, by name, on one that does not. The
#link("#djvu")[installation section] gives the one-line install for each
platform. Nothing needs configuring afterwards and nothing is rebuilt —
Pulpit looks for the library each time it opens a DjVu, so installing it is
enough.

*A DjVu is read-only.* Annotating, filling forms, selecting text, searching
and signing are PDF features, and Pulpit does not pretend to have them for
other formats. A DjVu is shown as *View only*, and the tools that do not
apply are not offered rather than refusing when you press them. To mark up a
scan, convert it first: `ddjvu -format=pdf book.djvu book.pdf` ships with
djvulibre and does it in one step.

=== Forms

Pulpit fills AcroForm PDFs: text fields, check boxes, radio groups, drop-down
and list boxes. It ships a PDFium built with a JavaScript engine, so a
form's own scripts run as its author intended. Calculated totals recompute,
dates and currency format themselves, and keystroke validation rejects what it
should. A form filled here comes out the same as a form filled in the readers
those scripts were written for, rather than subtly wrong.

What a script asks the viewer to do is reported to you and never performed.
A form that wants to submit itself to a URL, mail itself, print, or read the
file's path on disk is answered honestly and its request is shown. The data is
not sent, and the path a hostile form would like to know is not disclosed.

Edits are journalled as you make them, so a form half filled in when something
goes wrong is recoverable rather than retyped.

=== Signatures

Pulpit signs PDFs and reports on signatures they already carry.

To sign, save a credential once under *Settings → Signatures*, importing an
existing `.p12` or `.pfx`, then click the field you want to sign. The
profile carries the visibility, position and size, so signing asks nothing it
already knows: with a credential unlocked, the only thing you see is where to
save the signed copy. That copy is what opens afterwards, with editing off so
its signatures keep verifying.

Signing appends a new revision rather than rewriting the file, and the result
is read back from disk and checked before it is accepted. A signature Pulpit
cannot verify in the file it just wrote is never presented to you as one it
can.

On the reading side, the signature panel says what is actually known, and is
careful about the difference:

#quote(block: true)[
  Pulpit verifies that a signature is intact and that it matches the
  certificate embedded in it. It does not check whether that certificate is
  genuine. Other software may or may not accept this signature.
]

So a signature is reported as intact and attributed to the name in its
certificate together with that certificate's fingerprint, never as simply
"valid". Weak algorithms and short keys are reported rather than hidden, and a
signature that is present but damaged is shown as broken rather than quietly
omitted, which would read as an unsigned document.

Opening a document that is already signed asks what you want to do with it:
read and sign only, which leaves the existing signature intact, or allow
editing, which will have every viewer report the document as changed after
that signature once you save.

== Presenting

Mounting the Presenter layout runs two windows.

The Presenter Window is the one you look at: slides, notes, timers and
controls, arranged by the active layout. It opens on its own when Pulpit
starts.

The Audience Window is the one the room looks at: the current slide and
nothing else. You start it when you are ready, with *Start ▾* beside the
hamburger, a split button whose *Start* half uses the saved audience display,
while the arrow lists the connected displays so one click both picks the
projector and starts the window. *Stop* removes it again.

Two starting modes:

- Fullscreen: the window takes the chosen display immediately. A
  five-second delayed start is offered too, which leaves you time to switch to
  the projector workspace during the count.
- Windowed: the window opens as an ordinary window so you can drag it onto
  the right display or desktop position yourself, then press `f` to make it
  fullscreen where it sits. This is the reliable route on compositors that
  place windows themselves.

The projector is the one thing a second copy cannot have: two audience
windows on one screen leave the window manager flipping between them, which
is a flickering screen in the middle of a talk. So the first copy to start an
audience window holds it, another copy that tries is told which one has it,
and stopping the audience window hands it back.

Connecting a projector, disconnecting it, mirroring, swapping the two screens
and mixed DPI are treated as the ordinary case rather than as an edge one, and
the audience never sees a worse frame than it already had: the last complete
frame stays until a complete replacement exists.

=== Speaker notes

Notes come in two formats.

The first is a split page: each PDF page is twice as wide as the slide, with
the slide on one half and the notes on the other. Pulpit shows the audience
the slide half and keeps the notes half for you. This is what beamer produces
with

```tex
\setbeameroption{show notes on second screen=right}
```

A doubled page is recognised on open, with the slide taken to be the left
half, which is what beamer writes. If your notes are on the left instead,
*Swap halves* in the presenter window flips it.

Notes can also live on pages of their own, either alternating with the slides
or gathered after them, and you choose that in the presenter window.

The second is pdfpc, where the notes are text stored inside the PDF rather
than drawn on a page. Anything that writes that format works, including
beamer with the `pdfpc` package, and
#link("https://vincentarelbundock.github.io/mosaic")[Mosaic], which embeds
the notes in the PDF it produces.

Whichever format a deck uses, the one in force is shown in the presenter
window.

== Custom layouts

The *Layout: …* button in the presenter window opens the layout library, and a
layout opens from there into the designer.

Layouts import and export as JSON. A custom layout is a file in
`<config>/layouts/<id>.json`, written atomically: exporting one is copying it
out, importing one is copying it in, and the file itself is the interchange
format. The shape, with a `format_version`:

```json
{
  "format_version": 1,
  "name": "Conference Layout",
  "design_ratio": "sixteen-nine",
  "root": {
    "type": "leaf",
    "id": 0,
    "widget": {
      "kind": "timer",
      "style": { "variant": "standard", "scale": 1.0, "alignment": "center" },
      "config": { "timer": { "warning_minutes": 5 } }
    },
    "padding": 8.0,
    "background": "none",
    "border": "none",
    "empty_behavior": "show-blank-panel"
  }
}
```

`border` remains in format 1 files for compatibility, but is no longer
rendered. New built-in layouts write `"none"`; the split gutter owns visual
separation so adjacent cells cannot produce doubled edges or an outer frame.

A widget carries its kind, the style every widget has, and the configuration
only its family can have. The two must agree: a file claiming a title holds
notes options is refused rather than repaired.

On import Pulpit runs the full validation, renumbers node ids so they cannot
collide, and appends a numeric suffix if the name is taken. An imported layout
is always a custom layout, even if it was exported from a built-in.

= Keys

Press `?` for the complete reference inside the application. Conventional
reader keys are primary; the one familiar Vim/Zathura alternative is shown in
parentheses.

#include "keys.typ"

Presenter remotes usually emit `PageUp`/`PageDown`, media keys or browser
back/forward, all of which are bound by default. A remote whose keys the
toolkit cannot name is still usable: press the key and the presenter window
offers to bind it, storing the raw scancode in `settings.toml`.

Keyboard shortcuts are deliberately fixed for now, so the reference is the
application contract rather than a settings editor.

= Navigation

Four ways to move through a document:

- Keys, listed above. In Presenter mode, arrows and `PageUp`/`PageDown`
  move the audience with you.
- Back and forward buttons, a widget you can place in any layout cell, with
  or without words beside the arrows. These follow your traversal, meaning
  where an internal link or a jump took you, rather than simply stepping a
  page.
- Slider, a draggable track across the whole document.
- Overview (`o`), every page as a thumbnail, so you land by eye rather than
  by number. Picking one closes the menu.

With no document open, Pulpit shows a mode-neutral welcome page instead of a
Reader or Presenter layout. It leads with *Open a PDF*, teaches compact
getting-started and presenting subsets of the same fixed shortcuts, and links
to the online documentation.

= Annotations

The palette offers five controls, and the four tools sit under the digits in
the order it draws them:

- Pointer (`4`): a dot that follows the pointer. Its options also arm
  Spotlight, which lights a circle and dims the rest of the page.
- Ink (`1`): freehand strokes, black by default.
- Highlighter (`2`): a broad translucent stroke that leaves content
  readable.
- Eraser (`3`): removes the stroke or label it touches.
- Text (`T`): places a typewritten label, black by default.

Select the text tool, click the page, and type into the translucent expanding
field; `Enter` commits the label and `Ctrl+Enter` inserts a line, and
`Ctrl/Cmd+V` pastes text.

Text labels are complete Typst 0.15.1 documents. Markup, math,
set rules, functions, tables, and other built-in Typst features render live
after a short typing pause. Labels follow Typst's math syntax exactly: for
example, multiplication is
`$e=m c^2$`, with whitespace between variables.

== The select band

The select tool pulls a rectangle over the page, and its options say what the
rectangle is for. The choice sits in the tool's own panel, next to where the
highlighter keeps its colour, and it holds in both Reader and Presenter mode:

- *Annotations*, the default: hold every annotation the band encloses, to
  move, resize or delete them together. The only kind that touches the
  document.
- *Image*: copy the region to the clipboard as a picture. It is rendered
  freshly at twice the page's own scale rather than lifted off the screen, so
  what you paste does not depend on the zoom you happened to be reading at.
- *Text*: copy the text the region covers. This bounds an area and takes what
  falls inside it, which is how you get one column off a two-column page —
  a different question from dragging through text, which follows the reading
  order from one character to another.

A band set to copy leaves the selection alone, and a band set to hold
annotations never reaches the clipboard. Copying an image needs a session that
can carry one; on a headless session Pulpit says so rather than copying
nothing. On Wayland the copied image is also offered as a file, so pasting
into a file manager drops a PNG into the directory; an image editor still
receives the pixels directly.

Table extraction — dropping row and column dividers over a region and getting
it back as a spreadsheet — is deliberately not offered.

Marks can be exported, and in Presenter mode `v` decides whether the room sees
them or only you do.
