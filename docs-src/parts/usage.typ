
= Keys

#table(
  columns: (1.2fr, 2fr),
  stroke: none,
  inset: 0.55em,
  [*Key*], [*Action*],
  [`→` `↓` `Space` `PageDown`], [next slide (audience follows)],
  [`←` `↑` `PageUp` `Backspace`], [previous slide],
  [`Home` / `End`], [first / last],
  [`Tab` / `Shift+Tab`], [look ahead / back on your screen only, leaving the audience where it is],
  [`Enter`], [show the slide you were looking at to the audience],
  [`Esc`], [go back to the slide the audience is on],
  [`b` / `w`], [blank black / blank white],
  [`p` / `r`], [start-pause / reset the timer],
  [`s`], [swap presenter and audience displays],
  [`f`], [toggle audience fullscreen],
  [`o` / `F5`], [open / reload],
  [`q`], [quit],
)

Presenter remotes usually emit `PageUp`/`PageDown`, media keys or browser
back/forward, all of which are bound by default. A remote whose keys the toolkit cannot
name is still usable: press the key and the presenter window offers to bind
it, storing the raw scancode in `settings.toml`.

= Navigation

Four ways to move through the deck:

- *Keys*, listed above. Arrows, `Space` and `PageUp`/`PageDown` move the
  audience with you. `Tab` and `Shift+Tab` instead look ahead or back on your
  own screen while the audience stays put, which is how you check what is
  coming without showing it. `Enter` then shows the slide you landed on, and
  `Esc` returns you to the one the audience is seeing.
- *Back and forward buttons*, a widget you can place in any layout cell, with
  or without words beside the arrows.
- *Slider*, a draggable track across the whole deck.
- *Jump menu* (`o`), the whole deck as thumbnails, so you land on a slide by
  eye rather than by number. Picking one closes the menu.

Press `?` at any time to open the complete keyboard reference. Acrobat and
standard reader keys are shown first, with one Vim/Zathura alternative in parentheses;
`PageUp` and `PageDown` remain visible beside the arrows. Keyboard shortcuts
are deliberately fixed for now, so the reference is the application contract
rather than a settings editor.

With no document open, Pulpit shows a mode-neutral welcome page instead of a
Reader or Presenter layout. It leads with *Open a PDF*, teaches compact
getting-started and presenting subsets of the same fixed shortcuts, and links
to the online documentation. Opening a file from there still uses the shape
of its first page to choose Reader or Presenter automatically.

= Annotations

The palette offers five controls:

- *Pointer*: a dot that follows the pointer. Its options also arm
  *Spotlight*, which lights a circle and dims the rest of the page.
- *Ink*: freehand strokes that stay until the slide changes, black by
  default.
- *Highlighter*: a broad translucent stroke that leaves slide content
  readable.
- *Eraser*: removes the stroke or label it touches.
- *Text* (`T`): places a typewritten label, black by default.

Select the text tool, click the slide, and type into the translucent expanding
field; `Enter` commits the label and `Ctrl+Enter` inserts a line, and
`Ctrl/Cmd+V` pastes text.

Text labels are complete Typst 0.15.1 documents. Markup, math,
set rules, functions, tables, and other built-in Typst features render live
after a short typing pause. Labels follow Typst's math syntax exactly: for
example, multiplication is
`$e=m c^2$`, with whitespace between variables.

= Windows

Pulpit runs two windows.

The *Presenter Window* is the one you look at: slides, notes, timers and
controls, arranged by the active layout. It opens on its own when Pulpit
starts.

The *Audience Window* is the one the room looks at: the current slide and
nothing else. You start it when you are ready, with *Start ▾* beside the
hamburger, a split button whose *Start* half uses the saved audience display,
while the arrow lists the connected displays so one click both picks the
projector and starts the window. *Stop* removes it again.

Two starting modes:

- *Fullscreen*: the window takes the chosen display immediately. A
  five-second delayed start is offered too, which leaves you time to switch to
  the projector workspace during the count.
- *Windowed*: the window opens as an ordinary window so you can drag it onto
  the right display or desktop position yourself, then press `f` to make it
  fullscreen where it sits. This is the reliable route on compositors that
  place windows themselves.

= Layouts

The presenter screen is not hard-coded. It is a *layout*: a tree of splits and
cells with a widget in each cell, rendered proportionally into whatever window
it lands in. Four built-in layouts ship with the application, and any of them
can be duplicated into an editable copy.

The *Layout: …* button in the presenter window opens the layout library, and a
layout opens from there into the designer.

Layouts *import and export as JSON*. A custom layout is a file in
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

= Speaker notes

Notes come in two formats.

The first is a *split page*: each PDF page is twice as wide as the slide, with
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

The second is *pdfpc*, where the notes are text stored inside the PDF rather
than drawn on a page. Anything that writes that format works, including
beamer with the `pdfpc` package, and
#link("https://vincentarelbundock.github.io/mosaic")[Mosaic], which embeds
the notes in the PDF it produces.

Whichever format a deck uses, the one in force is shown in the presenter
window.
