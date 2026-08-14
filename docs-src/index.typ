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
    "div",
    attrs: (
      style: "font-family: Helvetica, Arial, sans-serif; font-size: 3rem;"
        + " font-weight: 700; letter-spacing: 0.12em; line-height: 1;",
    ),
  )[PULPIT]
  #html.elem(
    "p",
    attrs: (
      style: "font-size: 1.5rem; font-weight: 600; line-height: 1.3;"
        + " margin: 0.75rem auto 0;",
    ),
  )[A Snappy and Snazzy PDF Projector]
  #html.elem(
    "p",
    attrs: (
      style: "font-size: 1.05rem; line-height: 1.6; margin: 0.5rem auto 0;",
    ),
  )[
    Interactive media · Speaker notes · Annotations · Custom layouts ·
    Jump to page · Timers · And more!
  ]
]

= Installation

#include "parts/install.typ"

#include "parts/usage.typ"

= Licence

MIT or Apache-2.0. PDFium is BSD-3-Clause and is _not_ vendored here.
