#set document(title: [Pulpit])
#metadata((tags: ("overview",))) <website-metadata>

#html.elem("div", attrs: (style: "text-align: center; margin-block: 0 1.5rem;"))[
  #html.elem(
    "img",
    attrs: (
      src: "assets/logo.svg",
      alt: "Pulpit",
      style: "width: 100%; max-width: 15rem; display: block;"
        + " margin: 0 auto -0.75rem;",
    ),
  )
  #html.elem(
    "p",
    attrs: (
      style: "font-size: 1.5rem; font-weight: 600; line-height: 1.3;"
        + " margin: 1.75rem auto 0;",
    ),
  )[Read, Annotate, and Present PDF]
]

Pulpit is a cross-platform, free, and open source application to read,
annotate, sign, and present PDF documents and slide shows. Its design goals
are to be simple, fast, and minimalist.

#let mode-card(title, tagline, features, video) = html.elem(
  "div",
  attrs: (
    style: "flex: 1 1 18rem; border: 1px solid var(--calepin-color-border);"
      + " border-radius: var(--calepin-radius-lg, 0.75rem);"
      + " background: var(--calepin-surface, transparent);"
      + " padding: 1.25rem 1.4rem 1.4rem;",
  ),
)[
  #html.elem(
    "p",
    attrs: (
      style: "font-size: 1.25rem; font-weight: 650; margin: 0;",
    ),
  )[#title]
  #html.elem(
    "p",
    attrs: (
      style: "color: var(--calepin-color-muted); line-height: 1.5;"
        + " margin: 0.35rem 0 0.9rem;",
    ),
  )[#tagline]
  #html.elem(
    "ul",
    attrs: (style: "margin: 0; padding-left: 1.1rem; line-height: 1.65;"),
  )[
    #for feature in features {
      html.elem("li", attrs: (style: "margin: 0.2rem 0;"))[#feature]
    }
  ]
  #html.elem(
    "video",
    attrs: (
      src: video,
      controls: "",
      playsinline: "",
      preload: "metadata",
      style: "width: 100%; height: auto; display: block;"
        + " margin: 1.1rem 0 0; border-radius: 0.5rem;",
    ),
  )[]
]

#html.elem(
  "div",
  attrs: (
    style: "display: flex; flex-wrap: wrap; gap: 1.25rem;"
      + " margin-block: 0 2.5rem; align-items: stretch;",
  ),
)[
  #mode-card(
    [Reader],
    [Read it, mark it up, fill it in, sign it.],
    (
      [PDF, DjVu, image folders, `.cbz` and `.cbt`],
      [Continuous scroll, two-page view],
      [Outline, thumbnails, full-text search],
      [Remembers your place in every file],
      [Ink, highlighter, eraser, text notes],
      [Typst markup and maths in notes],
      [Form filling, scripts and all],
      [Sign, and verify what is signed],
      [Reloads when the file is rebuilt],
    ),
    "assets/tour-reader.mp4",
  )
  #mode-card(
    [Presenter],
    [Two windows, and a projector that behaves.],
    (
      [Presenter and audience windows],
      [Speaker notes: split-page or pdfpc],
      [Timers, alarms, and blanking],
      [Swap displays with one key],
      [Hot-plug, mirroring, mixed DPI],
      [Custom layouts, exported as JSON],
      [Thumbnail jump menu and remotes],
      [Video and web overlays],
    ),
    "assets/tour-presenter.mp4",
  )
]

= Installation

#include "parts/install.typ"

#include "parts/usage.typ"

= Licence

MIT or Apache-2.0.
