# pulpit reader format specification

Companion to `SPEC-document.md` and `SPEC-images.md`. Adds §53–§66.

`SPEC-images.md` covers one tier: raster images decoded in-process, presented
as a directory. This document covers everything else Okular reads, and its
main purpose is to say **which of those pulpit should do, which it should
refuse, and why** — so that "why not EPUB?" has a written answer rather than
being re-litigated.

**Class A (§54) is implemented**: `.cbz` and `.cbt` are read, and `.cbr` and
`.cb7` are refused by name. Nothing else here is, and sections marked **Not
planned** are decisions rather than backlog.

---

## 53. Three classes, not one list

**§53.1** The formats divide by *page model*, not by file type, and the
division decides everything else:

- **Class A — archives of images.** `.cbz .cbt .cb7 .cbr`. A container around
  content pulpit already renders. Extends `SPEC-images.md` almost unchanged.
- **Class B — paginated, native-library.** DjVu, XPS, PostScript, DVI. Pages
  exist, have fixed sizes, and render independently. Fits `PdfBackend` as it
  stands. Blocked on packaging, not architecture.
- **Class C — reflowable.** EPUB, Mobipocket, FB2, CHM, Markdown, ODT. **No
  page count or page size exists until a viewport is chosen.** This is a
  different document model wearing a file extension.

**§53.2** A format MUST be placed in a class before any code is written for
it. The classes have different backend contracts, different failure modes and
different answers on whether they ship at all.

---

## 54. Class A — archives of images

**§54.1** An image archive is presented exactly as `SPEC-images.md`'s
directory: entries in natural sort order (§40.4), one image per page, file
name as page identity (§43).

**§54.2** The archive **replaces the directory as the source**, so the
document is one file again. Reload therefore returns to `SourceStamp::File`
(§44.3) and the digest machinery of §42.3 is unnecessary: an archive is
rewritten atomically or it is not rewritten.

**§54.3** Entries MUST be filtered by the §41.2 extension set, and directory
entries inside the archive MUST be flattened rather than recursed into — a
`.cbz` with chapter subfolders is common and its reading order is still the
sorted full path.

**§54.4** Archive entries MUST be bounded before extraction: entry count,
per-entry uncompressed size, and total uncompressed size. A zip bomb reaches
this code path from an untrusted download and §47.2's pixel bound is applied
*after* decompression, too late.

**§54.5** Entries MUST NOT be extracted to disk. They are read into memory,
decoded, and fed to §47.1's decoded-image cache like any other page.

**§54.6** `.cbz` and `.cbt` (zip, tar) are pure Rust and in scope. `.cb7` (7z)
is in scope if a maintained pure-Rust decoder is available at the time, and is
otherwise deferred rather than taking a native dependency.

**§54.7** **`.cbr` is not planned.** RAR needs `unrar`, whose licence forbids
using it to create a RAR compressor and is not a licence this project will
carry or ship. Okular gets away with it because a distribution packages it
separately. If a `.cbr` is opened, pulpit MUST say the format is unsupported
and name RAR — not fail as a corrupt archive.

---

## 55. Class B — paginated formats behind native libraries

**§55.1** All Class B formats fit the existing `PdfBackend` contract without
modification: `open`, `metadata`, `page_size`, `render`, with the honest
`Unsupported` defaults for text, links and attachments.

**§55.2** The architecture is therefore **not** what blocks them. The cost is
a pinned native library on five platforms, its hash rotation, its CVE
surface, and a `missing_<library>_message` for each. That tax recurs forever;
the backend is written once.

**§55.3** **No Class B library is ever bundled.** PDFium is bundled because
it is the reason the application exists and every package installs it. A
Class B backend MUST bind an *installed* system library at run time, and MUST
report `Unsupported` naming the missing library when it is absent — the same
shape as `pulpit-media` preferring an installed ffmpeg and an installed
Chromium over shipping either.

**§55.4** This makes Class B formats a **capability of the machine**, not of
the build, which is consistent with the standing rule that the application
asks what the session can do rather than what it is.

**§55.5 DjVu.** Roughly 2MB, mature, page-oriented, and the format most likely
to be worth doing: scanned books are exactly the "document" case document mode
was built for. First candidate if Class B is ever started.

**§55.6 XPS.** Page-oriented and structurally close to PDF. Low value —
almost nothing produces XPS that does not also produce PDF — and it is listed
here only so the answer is written down.

**§55.7 PostScript.** libspectre is thin glue over **Ghostscript**: 20–40MB
with fonts, and **AGPL-3.0** against this project's `MIT OR Apache-2.0`.
Under §55.3 it would be discovered rather than bundled, which keeps the
licences separate, but a presenter without Ghostscript gets nothing. Given
that essentially every `.ps` in circulation converts to PDF cleanly, the
better answer is to **tell the presenter to convert it**, and pulpit SHOULD
say so by name when a `.ps` is opened.

**§55.8 DVI.** **Not planned.** Rendering DVI requires resolving fonts
through a TeX installation; without one there is nothing to draw. A machine
with a TeX installation can produce a PDF, which pulpit already opens.

---

## 56. The Class B backend contract

Applies if and when any Class B format is implemented.

**§56.1** Binding MUST be lazy and per-document, through §45.2's router. A
missing DjVu library MUST NOT prevent the worker from opening a PDF or an
image, and vice versa.

**§56.2** A Class B backend MUST NOT be trusted with untrusted input any more
than PDFium is. It runs in the same supervised worker process, under the same
crash and hang recovery, and a malformed file taking the worker down MUST
leave the audience frame standing.

**§56.3** Page count and page sizes MUST be read without rendering, as in
§46.1. A backend that can only learn a page's size by rasterising it is not
ready to be used.

**§56.4** `render_into` SHOULD be overridden where the library can rasterise
into a caller-supplied buffer, so the shared-memory mapping is written
directly.

---

## 57. Class C — reflowable formats

**§57.1** EPUB, Mobipocket, FB2, CHM, Markdown and ODT have **no intrinsic
pagination**. A page exists only once a viewport width, a font size and a line
height have been chosen. This is not a rendering detail; it removes the thing
every layer above the backend is keyed on.

**§57.2** The presenter window and the audience window are **different sizes**.
If pagination follows the viewport, "page 7" names different content in each
window, and the two displays show different text with no way to reconcile
them. That is a direct violation of the standing rule that the audience frame
is a faithful rendering of the presenter's position, and it is the reason this
class is hard rather than merely unimplemented.

**§57.3** Therefore, if Class C is ever implemented: **pagination MUST be
computed once, at a pinned layout width, and MUST NOT depend on either
window's size.** The resulting pages are then scaled to fit each window
exactly as PDF pages are. The document gets a fixed page count and fixed page
sizes, and everything above the backend — the frame cache, the overview grid,
`thumbnails.rs`, notes mapping, page identity — keeps working unchanged.

**§57.4** The pinned width is a **property of the document**, chosen at open
and recorded. Changing it re-paginates, which MUST be treated as a new
document through the ordinary candidate/promote path, not as a live reflow.
Reflowing under a presenter mid-talk is the failure this rule prevents.

**§57.5** Page identity is the **page ordinal at the pinned width**, and it is
stable only as long as the width and the source are unchanged. Class C
therefore has *weaker* identity guarantees than Class A or B, and a re-paginate
MUST re-anchor on the nearest preceding structural landmark (chapter, heading,
anchor) rather than on the ordinal.

**§57.6** Class C is **not planned for the presenter**. A talk is not given
from an EPUB. It is plausible only for document mode, and only after §58's
runtime question is answered.

---

## 58. The Class C runtime, if it happens

**§58.1** Class C formats are HTML underneath — EPUB and CHM literally,
Markdown and FB2 by trivial conversion, ODT by a lossy one. The route is
therefore **one backend that paginates HTML**, not five layout engines.

**§58.2** The renderer MUST be the **already-external Chromium** that
`pulpit-media` discovers, under §55.3's rule: discovered, never bundled.
Bundling a browser is 150–200MB and is not on the table.

**§58.3** The document path MUST NOT reuse the media path's transport.
`pulpit-media` runs `Page.startScreencast` at `format: "jpeg", quality: 80`
because it is streaming *motion*, where JPEG artefacts are invisible and frame
pacing is what matters. A document page is a **still** — text, at rest, read
closely — and JPEG-80 on small text is visibly wrong. Class C MUST use
`Page.captureScreenshot` with a lossless format, one capture per page render,
and MUST NOT declare `Limitation::CompressedFrames`.

**§58.4** The Class C browser MUST run under at least the isolation
`pulpit-media` already establishes: an inherited debugging pipe with no
listening socket, a private `--user-data-dir`, and a restrictive
`Content-Security-Policy`. EPUB and CHM are untrusted downloads containing
scripts and remote references.

**§58.5** JavaScript in a Class C document MUST be disabled and network access
MUST be denied (`connect-src 'none'`). A book does not need either, and both
are how an untrusted document reaches the network from a presenter's machine
on a conference wifi.

**§58.6** The pagination measurement itself is a scripted layout query, which
means the pinned-width decision of §57.3 and the isolation of §58.5 interact:
the measuring script is pulpit's own, injected, and is the only script that
runs.

---

## 59. Text, search and notes by class

**§59.1** Class A has **no text layer**. `find_text` MUST report unsupported,
never an empty result (§48.2).

**§59.2** Class B DjVu and XPS may carry text. A backend that exposes it
SHOULD implement `find_text` over the same matcher the PDF path uses, so a hit
found in the presenter is the hit found in the reader. A backend that cannot
MUST report unsupported.

**§59.3** Class C has text by construction, and the hard part is not finding
it but mapping a hit to a rectangle on a paginated render — which only exists
at the pinned width (§57.3).

**§59.4** All three classes pin `NotesMapping::SlidesOnly` for the reasons in
§46.4. None of them carries a `.pdfpc` sidecar.

---

## 60. Document mode across the classes

**§60.1** Every class reports `Unsupported` for all `DocumentBackend`
operations, as §48.1 requires of images: annotations, form fields, text
selection, save, signing.

**§60.2** This is the largest limitation of the whole design and it MUST be
stated to the presenter rather than discovered by pressing a control that
refuses. A reader that cannot annotate a scanned DjVu book is a materially
less useful reader, and that is the trade being made in exchange for not
carrying a mutation model per format.

**§60.3** Annotation of Class A and Class B documents is **not planned**. It
would require a per-format sidecar — the one thing `SPEC-document.md` A1
explicitly refuses for PDF, on the grounds that a second copy of the
annotations can drift from the document. Introducing exactly that for other
formats would undo the invariant rather than extend it.

---

## 61. What is refused, and how

**§61.1** An unsupported format MUST be refused **by name**, saying what it
is and what would be needed: "RAR archives are not supported" (§54.7),
"PostScript needs Ghostscript; converting to PDF is usually easier" (§55.7),
"this build has no DjVu library installed" (§55.3).

**§61.2** A refusal MUST NOT be reported as a corrupt file. "pulpit cannot
read this kind of file" and "this file is damaged" are different facts, and
telling a presenter the second when the first is true sends them looking for a
problem that does not exist.

**§61.3** Format detection for refusal messages MAY sniff content, unlike the
listing rule in §41.1. Naming the format correctly is worth reading sixteen
bytes.

---

## 62. Ordering

If any of this is ever built, the order is fixed by risk, not appetite:

**§62.1** Class A `.cbz`/`.cbt` first. Pure Rust, no new dependency, and it
reuses `SPEC-images.md` almost entirely — mostly a source adapter and §54.4's
bounds.

**§62.2** Class B DjVu second, and only if a real need appears. It is the
proof that §55.3's discovered-not-bundled rule and §45.2's router work for a
second native library, and it is the format with actual users.

**§62.3** Class C last, if ever, and only for document mode. §57.2 is a real
architectural conflict and it should not be attempted while anything else is
outstanding.

---

## 63. Testing

**§63.1** Class A: archive listings, flattening, natural sort across nested
paths, and each of §54.4's bounds refusing rather than allocating. No native
dependency, so all of it runs in CI.

**§63.2** Class B: cannot run in CI without the library installed, so it
follows the PDFium precedent — tests skip with a message, and the skip is
visible rather than silent, since a green run that skipped the meaningful
tests is the failure mode that matters.

**§63.3** Class C: pagination at a pinned width MUST be a golden test — same
input, same width, same page count — or §57.5's identity guarantee is
unverifiable.

---

## 64. Summary of decisions

| Format | Class | Decision |
|---|---|---|
| `.cbz` `.cbt` | A | In scope, first |
| `.cb7` | A | In scope if a pure-Rust decoder exists, else deferred |
| `.cbr` | A | **Not planned** — unrar licence (§54.7) |
| DjVu | B | Deferred; first Class B candidate if needed (§55.5) |
| XPS | B | Deferred; low value (§55.6) |
| PostScript | B | Deferred; advise conversion to PDF instead (§55.7) |
| DVI | B | **Not planned** — needs a TeX installation (§55.8) |
| EPUB, Mobi, FB2, CHM, Markdown | C | **Not planned for the presenter**; document mode only, after §57 (§57.6) |
| ODT | C | **Not planned** — lossy conversion, and its producers export PDF |

---

## 65. Standing constraints these decisions rest on

**§65.1** Never bundle a format library (§55.3). PDFium is the single
exception and it is the reason the application exists.

**§65.2** Never let a format's absence break another format (§56.1).

**§65.3** Never let pagination depend on a window size (§57.3).

**§65.4** Never introduce an annotation sidecar (§60.3).

**§65.5** Never report "unsupported" as "corrupt" (§61.2).

---

## 66. Re-litigating this

These are decisions, not conclusions from measurement, and the numbers behind
them are in `SPEC-images.md` §52 and in the sizing done alongside it. A
concrete user need for a specific format is a good reason to revisit §64; an
observation that a format "would be nice" is not, and this file exists so that
conversation starts from what was already decided.
