// The complete pulpit example deck, written in Typst with Mosaic's
// standard default theme.
//
// Build with `make -C examples mosaic.pdf`.

#import "@local/mosaic:0.0.2" as m

#show: m.setup.with(
  title: [pulpit example deck],
  subtitle: [Presentation basics and media overlays],
  authors: [pulpit],
)

// A poster wrapped in its own link: the link rectangle and the visible poster
// coincide by construction, so the overlay lands exactly where the poster is.
#let overlay(uri, poster, width: 48%) = align(center)[
  #link(uri)[#image(path(poster), width: width)]
]

#let uri(value) = align(center, text(size: 0.62em, raw(value)))

#m.slide(layout: "title")

== One simple idea

#m.note[Open by naming the problem, not the tool: everyone in the room has sat through a talk where the deck was the point.

Keep this slide to about a minute.]

- Present a standard PDF slide deck.
- Keep the audience focused on the current slide.
- Move forward when you are ready.

== A clear structure

+ Introduce the topic.
+ Develop the main argument.
+ Finish with a memorable conclusion.

#m.slide(layout: "content", columns: 2)[== Two useful views
  #m.note[This is the slide to demo on, not to read from. Point at the laptop, then at the projector.

  If the second display has not come up yet, this is the moment to notice.]
][
  #m.components.card(role: "accent", width: 100%)[
    *Presenter*

    Controls the pace and previews what comes next.
  ]
][
  #m.components.card(role: "accent", width: 100%)[
    *Audience*

    Sees a clean, focused version of the current slide.
  ]
]

== Keep it readable

- Use short headings.
- Prefer a few strong points per slide.
- Leave enough space for every idea to breathe.

#m.components.callout(role: "warning", title: [Remember])[
  Slides support the presentation; they are not the presentation.
]

== Ready to present

#m.note[Land here on time. If the media section has to be cut, cut the HTML slide — the video makes the same point.]

#align(center + horizon)[
  #text(size: 1.2em)[Open the PDF, check the display, and begin.]
]

== How a deck declares media

A media overlay is a link around a poster:

#align(center)[
  #raw("link(\"run:clip.mp4?autostart\")[image(\"poster.png\")]")
]

#m.note[Stress that nothing here is a pulpit extension. A deck written for pdfpc five years ago plays unchanged.]

`run:` is the convention pdfpc and Impressive already read, so a deck written
for them needs no changes. The link rectangle is the overlay region, and the
poster is what every other PDF reader shows.

== Animated GIF

The poster is the GIF's own first frame; pulpit replaces it with the
running animation.

#overlay(
  "run:media-assets/bouncing.gif?autostart&loop",
  "media-assets/bouncing-still.png",
)

#uri("run:media-assets/bouncing.gif?autostart&loop")

== Video

The clip sits beside the document, which keeps the PDF small and the asset
editable without rebuilding the deck.

#overlay(
  "run:media-assets/clip.mp4?autostart&mute",
  "media-assets/poster.png",
)

#uri("run:media-assets/clip.mp4?autostart&mute")

== Interactive HTML

The same convention extends to a page: the HTML file's own directory is served
to it, so its stylesheet and script resolve exactly as on disk. On the slide it
becomes live and clickable.

#overlay(
  "run:media-assets/bouncing-balls.html",
  "media-assets/balls-poster.png",
  width: 40%,
)

#uri("run:media-assets/bouncing-balls.html")

== Incremental reveal continuity

Each reveal step repeats the identical link, and pulpit collapses the
repetitions into one overlay — so the video keeps playing rather than
restarting as the bullets appear.

#m.note[Every frame: keep talking over the clip rather than about it.]

#m.steps.on(2)[#m.note[Pause here. Let the clip run a few seconds so the room can see for itself that it did not restart — this is the whole point of the slide.]]

#grid(columns: (1fr, 1fr), gutter: 1em, align: top,
  overlay(
    "run:media-assets/clip.mp4?loop&mute",
    "media-assets/poster.png",
    width: 100%,
  ),
  m.steps.reveal[
    - The video starts on the first step.
    - It does not restart here.
    - Nor here: one overlay, one session.
  ],
)

== What pulpit reads

#align(center)[
  #set text(size: 0.62em)
  #table(
    columns: (1.25fr, 2fr),
    align: (left, left),
    inset: (x: 0.65em, y: 0.35em),
    table.header([*Written in the deck*], [*What it declares*]),
    [`run:clip.mp4`], [video beside the document],
    [`run:spin.gif`], [animated image beside the document],
    [`run:page.html`], [interactive HTML],
    [`?autostart`], [start on commit (also spelt `?autoplay`)],
    [`?loop`], [repeat],
    [`?mute`], [silence (video)],
    [`?start=12.5`], [seek, in seconds (video)],
  )

  #v(0.8em)
  A `run:` link to anything that is not a media file is never executed and
  never becomes an overlay.
]
