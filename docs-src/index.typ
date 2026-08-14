#set document(title: [Pulpit])
#metadata((tags: ("overview",))) <website-metadata>

#html.elem("div", attrs: (style: "text-align: center; margin-block: 2rem;"))[
  #html.elem(
    "img",
    attrs: (
      src: "assets/logo.svg",
      alt: "Pulpit",
      style: "width: 45%; max-width: 20rem;",
    ),
  )
  #html.elem(
    "div",
    attrs: (
      style: "font-family: Helvetica, Arial, sans-serif; font-size: 3rem;"
        + " font-weight: 700; letter-spacing: 0.12em; margin-top: 0.5rem;",
    ),
  )[PULPIT]
]

*Pulpit: a snappy and snazzy PDF projector with interactive media, speaker
notes, annotations, jump page, timers, and more!*

It runs a presenter window on your display and an audience window on the
projector, and it treats connection, disconnection, mirroring, swapping,
suspend/resume and mixed DPI as the _main_ engineering problem — not as
polish.

```sh
pulpit path/to/deck.pdf
```

= Status

The presenter is complete and in use: two-window presentation, display
reconciliation, layouts, speaker notes, PDF links, presenter annotations,
session recovery, and media overlays for animated images and interactive HTML.

Four things are deliberately left undone: the upstream Iced contribution
(without which portable targeted fullscreen on Wayland is impossible),
physical-hardware qualification, packaging beyond Nix, and screen-reader
support — which is blocked on Iced exposing an accessibility tree at all.

= Where to go next

- #link("install.html")[Installation] — Nix, other distributions, and PDFium.
- #link("usage.html")[Usage] — keys, presenter layouts, and speaker notes.
- #link("internals.html")[Internals] — architecture, invariants, the platform
  boundary, and the display-control findings the design rests on.

= Licence

MIT or Apache-2.0. PDFium is BSD-3-Clause and is _not_ vendored here.
