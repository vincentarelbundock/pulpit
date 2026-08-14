# Example decks

There are two versions of the same four-slide example deck — one in Beamer and
one in Typst with Mosaic's standard default theme: a title slide, then one
slide each for an animated GIF, a video and an interactive HTML page.

A third file, `stress-test-730.pdf`, is not an example to read but a scaling
fixture: a 730-page real-world lecture deck, 30 MB, used to exercise page-count
scaling, the frame cache's byte bound and worker restart under memory pressure.
It is third-party material — see `LICENSES/README.md` for its provenance and
the terms it comes under.

## `mosaic.pdf` — Typst and Mosaic

`mosaic.typ` presents the same slides as `beamer.tex`. Its posters are
ordinary Typst links around images:

```typ
#link("run:media-assets/clip.mp4?autostart&mute")[#image("poster.png")]
```

Build it with `make mosaic.pdf`. It requires Typst 0.15 or newer and the local
Mosaic 0.0.2 package from `~/repos/mosaic` (`make install` in that repository).

## `beamer.pdf` — the example deck

`beamer.tex` is the same deck in ordinary Beamer, in the Warsaw theme. Open it
with `make launch DECK=examples/beamer.pdf`, or the Typst version with
`make launch DECK=examples/mosaic.pdf`. The test suite reads this Beamer deck
as its standard-deck fixture.

It is ordinary beamer. A media overlay is a plain link around a
poster image:

```latex
\href{run:media-assets/clip.mp4?autostart&mute}{\includegraphics{poster}}
```

`run:` is the convention [pdfpc](https://pdfpc.github.io/) and Impressive
already read, so a deck written for either needs no changes to work here — and
one written for pulpit still opens, prints and presents correctly in any
other PDF reader, which shows the poster.

The link rectangle *is* the overlay region, so the overlay lands exactly where
the poster sits. There is no pulpit package to load and no pulpit URI
scheme to learn.

| Frame | Declares | Demonstrates |
|---|---|---|
| 1 | — | the title slide, with `logo.svg` |
| 2 | `run:media-assets/bouncing.gif?autostart&loop` | an animated GIF |
| 3 | `run:media-assets/clip.mp4?autostart&mute` | video |
| 4 | `run:media-assets/bouncing-balls.html` | an interactive HTML page |

### What pulpit reads

| Written in the deck | What it declares |
|---|---|
| `run:clip.mp4` | video beside the document |
| `run:spin.gif` | animated image beside the document |
| `run:page.html` | interactive HTML; the file's directory is served to it |

Parameters are `autostart` (also spelt `autoplay`), `loop`, `mute`,
`start=<seconds>` and `poster=<id>`. Unknown parameters are ignored with a
diagnostic rather than guessed at.

A `run:` link to anything that is not a media file is **never executed** and
never becomes an overlay. hyperref turns `run:` into a PDF `/Launch` action;
pulpit reads the filename out of it and refuses to do anything else with
it.

### The one piece of TeX bookkeeping

`&` separates query parameters, and in TeX it is an alignment tab. `beamer.tex`
therefore defines its URIs inside a `\catcode`&`=12` group. Without that the
parameters are silently eaten and the video plays with defaults — which is
exactly what `playback_intent_survives_the_latex_to_pdf_round_trip` in
`crates/pulpit-render/tests/media_deck.rs` exists to catch.

### Building

```sh
make            # build every example PDF
make assets     # regenerate media-assets/ from scratch
make verify     # check the built PDF's links and assets (needs pypdf)
make clean      # remove LaTeX by-products
```

The Beamer deck requires `latexmk` or `pdflatex`; the Mosaic deck requires
Typst 0.15 or newer and Mosaic 0.0.2 installed as a local package. Regenerating
the assets additionally needs ImageMagick 7 (`magick`) and `ffmpeg`; the
generated assets are checked in, so this is only needed when changing them.
`logo.pdf`, the title slide's logo for Beamer, is rasterised from `logo.svg`
with `magick`; the Typst deck reads the SVG directly.

`crates/pulpit-render/tests/media_deck.rs` runs the real discovery
pipeline against `beamer.pdf` and is the authoritative check — it needs no
Python and runs in the ordinary test suite.
