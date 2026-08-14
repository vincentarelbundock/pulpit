// The pulpit example deck, written in Typst with Mosaic's standard default
// theme: a title slide and one slide per media kind.
//
// Build with `make -C examples mosaic.pdf`.

#import "@local/mosaic:0.0.2" as m

#show: m.setup.with(
  title: [Pulpit],
  subtitle: [A Snappy and Snazzy PDF Projector],
  authors: [],
)

// A poster wrapped in its own link: the link rectangle and the visible poster
// coincide by construction, so the overlay lands exactly where the poster is.
#let overlay(uri, poster, width: 48%) = align(center)[
  #link(uri)[#image(poster, width: width)]
]

#m.slide[
  #align(center + horizon)[
  #image("logo.svg", width: 22%)

  #v(0.4em)
  #text(size: 1.6em, weight: "bold")[Pulpit]

  #v(0.2em)
  #text(size: 1.1em)[A Snappy and Snazzy PDF Projector]
  ]
]

#m.slide[
  == GIF

  #overlay(
    "run:media-assets/bouncing.gif?autostart&loop",
    "media-assets/bouncing-still.png",
    width: 60%,
  )
]

#m.slide[
  == Video

  #overlay(
    "run:media-assets/clip.mp4?autostart&mute",
    "media-assets/poster.png",
    width: 60%,
  )
]

#m.slide[
  == HTML + JS

  #overlay(
    "run:media-assets/bouncing-balls.html",
    "media-assets/balls-poster.png",
    width: 50%,
  )
]
