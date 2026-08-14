#set document(title: [Pulpit])
#metadata((tags: ("overview",))) <website-metadata>

#title()

#align(center)[#image("assets/logo.svg", width: 45%)]

*Pulpit is the PDF presenter that does not screw up your projector.*

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
