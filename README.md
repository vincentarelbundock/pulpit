# pulpit

<img src="logo.svg" alt="pulpit" width="240">

A focused PDF presenter in Rust. It runs a presenter window on your display and
an audience window on the projector, and it treats connection, disconnection,
mirroring, swapping, suspend/resume and mixed DPI as the *main* engineering
problem — not as polish.

> **pulpit: A Snappy and Snazzy PDF Projector**

It opens PDFs, folders of images, and `.cbz` / `.cbt` comic archives. Anything
that is not a PDF is read-only: pages turn and render, and the controls that
need a PDF underneath them are dimmed rather than refusing when pressed.

Installation, keys, layouts, and internals are documented on the website:
<https://vincentarelbundock.github.io/pulpit>

## Licence

MIT or Apache-2.0, at your option. PDFium is BSD-3-Clause and is *not*
vendored here. Every licence text lives in `LICENSES/`, and
`LICENSES/README.md` says which one covers which part of the package.
