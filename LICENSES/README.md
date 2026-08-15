# Licences

Pulpit itself is offered under **MIT OR Apache-2.0**, at your option:
`LICENSE-MIT` and `LICENSE-APACHE` in this directory. That covers everything
in this repository except the third-party work listed below.

Pulpit also carries other people's work, and this is the list a package has to
carry with it. Three kinds of thing appear here, and they are not the same
kind of obligation:

* **Vendored source** — third-party code copied into this repository and
  compiled into the binary. Its licence text ships with the source and with
  every binary package.
* **Bundled binaries and assets** — files shipped beside the binary.
* **Cargo dependencies** — crates fetched at build time and linked in. They
  are not reproduced here one by one; `cargo license` or `cargo about` over
  `Cargo.lock` is the authoritative list, because the lock file is what
  actually decides them.

## What covers what

| Part of the package | Licence | Full text |
| --- | --- | --- |
| Pulpit's own source, in `crates/`, `scripts/`, `packaging/`, `docs-src/` | MIT OR Apache-2.0 | `LICENSE-MIT`, `LICENSE-APACHE` |
| The two vendored `iced_aw` widgets, in `crates/pulpit/src/vendor/iced_aw/` | MIT, © 2020 Kaiden42 | `ICED_AW-LICENSE` |
| The Lucide icons, in `crates/pulpit/assets/icons/` | ISC, © 2026 Lucide Icons and Contributors | `LUCIDE-LICENSE` |
| Fonts embedded by the `typst-assets` Cargo dependency | OFL-1.1 / GUST / Bitstream Vera terms | `TYPST_ASSETS-NOTICE`, a copy of the crate's `NOTICE` |
| PDFium, fetched into `lib/` and shipped as `lib/pulpit/libpdfium.so` | BSD-3-Clause, plus MIT for the packaging | `lib/PDFIUM-LICENSE`, after `make pdfium` |
| Everything resolved from `Cargo.lock` | MIT / Apache-2.0 / BSD / ISC | generated, see below |
| `examples/stress-test-730.pdf` | © Gerth Stølting Brodal, Aarhus University; no reuse grant | none — see below |

---

## Vendored source

### iced_aw — colour picker and time picker

* Upstream: <https://github.com/iced-rs/iced_aw>, version 0.14.1
* Licence: **MIT**, © 2020 Kaiden42 and the iced_aw contributors
* Full text: `ICED_AW-LICENSE`
* Provenance and the list of changes made:
  `crates/pulpit/src/vendor/iced_aw/README.md`

Two widgets were copied rather than the crate depended on; the reasoning is in
that README. The code is modified — module paths, the icon-font call, one
let-chain, and formatting — as the MIT licence permits, and the changes are
recorded.

---

## Bundled binaries and assets

### PDFium

* Build: <https://github.com/bblanchon/pdfium-binaries>, fetched by
  `scripts/fetch-pdfium.sh`; the version is recorded in `lib/PDFIUM-VERSION`
* Licence: **MIT** for the packaging, © 2014-2025 Benoit Blanchon; PDFium
  itself is **BSD-3-Clause**, © 2014 The PDFium Authors, and carries further
  third-party licences of its own
* Full text: `lib/PDFIUM-LICENSE`, installed as
  `share/doc/pulpit/PDFIUM-LICENSE`

PDFium is not committed to this repository, so its licence text is not in this
directory either: it arrives with the download. `make pdfium` fetches both,
`make bundle` ships both, and a package built without that step carries no
PDFium and needs no PDFium notice.

### Lucide icons

* Upstream: <https://lucide.dev>
* Licence: **ISC**, © 2026 Lucide Icons and Contributors
* Full text: `LUCIDE-LICENSE`

The `.svg` files under `crates/pulpit/assets/icons/` are compiled into the
binary with `include_bytes!`, so this notice travels with every build.

### Fonts from typst-assets

* Upstream: <https://crates.io/crates/typst-assets>, the pinned version in
  `Cargo.lock`
* Licence: **OFL-1.1** (Libertinus, New Computer Modern's regular face and
  others), **GUST Font License** (New Computer Modern), **Bitstream Vera
  terms** (DejaVu), among others — the notice itself is the authoritative map
* Full text: `TYPST_ASSETS-NOTICE`, a verbatim copy of the crate's `NOTICE`

The `fonts` feature embeds these fonts into the binary, and the OFL requires
its text to accompany any distribution of the fonts, so this notice ships in
every package alongside the other licence texts. When the pinned
`typst-assets` version changes, refresh the copy from the crate's `NOTICE`.

---

## Third-party test material

### `examples/stress-test-730.pdf`

* What it is: a 730-page, 30 MB, 960×540 deck — the whole semester of
  *Introduction to Programming with Scientific Applications* concatenated into
  one PDF. The title page names Gerth Stølting Brodal, Department of Computer
  Science, Aarhus University; the file was produced by pypdf, which is
  consistent with the course site's own combined download.
* Why it is here: it is a real presentation deck rather than a book set in
  slide format, so it exercises page-count scaling, the frame cache's byte
  bound and worker restart under memory pressure in a way the small example
  decks cannot.
* Licence: **none granted.** The slides are freely downloadable from the
  course site, but the site asserts plain copyright — "© Gerth Stølting
  Brodal, Aarhus University, Department of Computer Science" — and no
  Creative Commons or other reuse grant accompanies them. Free to download is
  not the same as free to redistribute.

It is committed to this repository as a permanent fixture, unmodified and with
its authorship intact, and this entry is the attribution that travels with it.
It is not part of Pulpit and is **not** covered by Pulpit's MIT OR Apache-2.0
grant: cloning this repository conveys no licence to redistribute the deck or
to reuse the slides for any other purpose.

Two consequences follow. It must not be shipped in a release artefact — the
binary packages carry `crates/`, `lib/` and the documentation, never
`examples/stress-test-730.pdf`. And if the copyright holder asks for it to be
removed, it comes out, so nothing load-bearing should be built on its
continued presence: a test that reads it should skip when it is absent, the
way the PDFium tests already do.

---

## Cargo dependencies

Resolved from `Cargo.lock` at build time. The tree is overwhelmingly
MIT / Apache-2.0 / BSD / ISC; nothing in it is copyleft, and nothing is
statically linked under terms that would require offering source for Pulpit
itself.

To regenerate the exact list for a release:

```sh
cargo install cargo-about
cargo about generate --format json > third-party-crates.json
```
