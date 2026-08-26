# pulpit reader format specification

Companion to `SPEC-document.md` and `SPEC-images.md`. Adds §53–§66.

`SPEC-images.md` covers one tier: raster images decoded in-process, presented
as a directory. This document covers everything else Okular reads, and its
main purpose is to say **which of those pulpit should do, which it should
refuse, and why** — so that "why not EPUB?" has a written answer rather than
being re-litigated.

**What has shipped is no longer specified here.** Class A (`.cbz`, `.cbt`,
with `.cbr` and `.cb7` refused by name) and Class B DjVu (bound to an
installed djvulibre, discovered at run time, never bundled) are implemented
and their clauses have been deleted; the code and its tests are the record.
What remains below is what is **not** built: the contract the *next* Class B
backend must meet, the deferred and not-planned formats, and Class C — plus
the refusal rules of §61, which every future entry in §64 must satisfy.

Section numbers are kept stable because other specs cite them; gaps are
deletions, not omissions.

---

## 53. Three classes, not one list

**§53.1** The formats divide by *page model*, not by file type, and the
division decides everything else:

- **Class A — archives of images.** `.cbz .cbt .cb7 .cbr`. A container around
  content pulpit already renders. Extends `SPEC-images.md` almost unchanged.
  **Implemented** for `.cbz`/`.cbt`.
- **Class B — paginated, native-library.** DjVu, XPS, PostScript, DVI. Pages
  exist, have fixed sizes, and render independently. Fits `PdfBackend` as it
  stands. Blocked on packaging, not architecture. **DjVu implemented**; the
  rest are deferred or not planned (§55).
- **Class C — reflowable.** EPUB, Mobipocket, FB2, CHM, Markdown, ODT. **No
  page count or page size exists until a viewport is chosen.** This is a
  different document model wearing a file extension. **Not started.**

**§53.2** A format MUST be placed in a class before any code is written for
it. The classes have different backend contracts, different failure modes and
different answers on whether they ship at all.

---

## 54. Class A — archives of images

**Implemented and deleted.** `.cbz` and `.cbt` are read as
`SPEC-images.md`'s directory made into one file; entries are bounded before
extraction, never written to disk, and flattened in natural sort order.
`.cbr` and `.cb7` are refused by name before any backend is bound.

**§54.6 (open)** `.cb7` is in scope **if** a maintained pure-Rust 7z decoder
is available at the time, and is otherwise deferred rather than taking a
native dependency. It is refused by name today; adopting a decoder is a
dependency decision, not a design one.

**§54.7 (standing)** `.cbr` is **not planned**: RAR needs `unrar`, whose
licence this project will not carry. See §65.6.

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

**§55.5 DjVu.** Implemented; specification deleted. It is the worked example
for §56, and §56.5, §56.6 and §56.7 are what that example cost.

**§55.6 XPS.** Page-oriented and structurally close to PDF. **Deferred, low
value** — almost nothing produces XPS that does not also produce PDF — and it
is listed here only so the answer is written down. If it is ever built, it is
a §56 backend and nothing above the backend changes.

**§55.7 PostScript.** libspectre is thin glue over **Ghostscript**: 20–40MB
with fonts, and **AGPL-3.0** against this project's `MIT OR Apache-2.0`.
Under §55.3 it would be discovered rather than bundled, which keeps the
licences separate, but a presenter without Ghostscript gets nothing. Given
that essentially every `.ps` in circulation converts to PDF cleanly, the
better answer is to **tell the presenter to convert it**, and pulpit SHOULD
say so by name when a `.ps` is opened. The advice is given (§61.4); the
backend is not planned.

**§55.8 DVI.** **Not planned.** Rendering DVI requires resolving fonts
through a TeX installation; without one there is nothing to draw. A machine
with a TeX installation can produce a PDF, which pulpit already opens.

---

## 56. The Class B backend contract

Written for the first Class B format and met by it. It stands as the contract
**the next one must meet**, and §56.5 and §56.6 in particular are findings
rather than preferences: they were paid for once and should not be rediscovered.

**§56.1** Binding MUST be lazy and per-document, through §45.2's router. A
missing library for one format MUST NOT prevent the worker from opening a PDF
or an image, and vice versa.

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

**§56.5** A Class B backend MUST hold **one library context per process**
where the library's own concurrency does not hold up under two.

Measured, on djvulibre 3.5.30: two `ddjvu_context_t` alive in one process are
fine while only one thread is inside the library, and driving two of them from
two threads makes `ddjvu_document_create_by_filename_utf8` return null for
perfectly good files in roughly one run in seven. A backend that reported "this
file will not open" for a book that opens on the next attempt is worse than one
that refuses to exist twice, so the second bind is refused by name. This is the
invariant PDFium already carries, and it is what makes the worker *process*
boundary mandatory rather than stylistic (§56.2).

**§56.6** A Class B backend MUST NOT re-apply a page transform the library has
already applied.

Measured, on the same version: `ddjvu_document_get_pageinfo` — the call §56.3
requires, because it answers without decoding — reports a rotated page's
*turned* dimensions and returns the angle beside them. The header documents
rotation as honoured by `ddjvu_page_render`, `ddjvu_page_get_width` and
`ddjvu_page_get_height` and says nothing about `get_pageinfo`, which invites
exactly the wrong conclusion; applying the angle again reports every rotated
scan at the wrong aspect and letterboxes it. Each Class B library needs this
checked against a rotated fixture rather than against its documentation.

**§56.7** A Class B backend MUST NOT assume a library reports *text* in the
same space it reports *pages*.

Measured, on the same version, and the mirror image of §56.6:
`ddjvu_document_get_pagetext` answers in the page's **stored, unturned** image
space while `ddjvu_document_get_pageinfo` answers with that page **turned**. A
page stored 120×80 and rotated a quarter turn reports 80×120 from one call and
still says `(page 0 0 120 80)` in the other, and the header documents neither.
So the rotation the renderer applies for free must be applied to the text
coordinates by hand — counter-clockwise, and from a bottom-left origin — or
every highlight on a rotated scan lands somewhere the word is not. Like §56.6,
this needs checking against a rotated fixture *per call*, because getting one
call right says nothing about the next.

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

**§59.1** Class A has no text layer and reports `find_text` unsupported.
Implemented; deleted.

**§59.2** Class B DjVu and XPS may carry text. A backend that can reach it
MUST implement `find_text` over **the same matcher the PDF path uses**, so a
hit found in the presenter is the hit found in the reader and the format
contributes geometry rather than a second idea of what matching means. A
backend that cannot reach it reports `find_text` unsupported, which §48.2
permits: "this cannot be searched" and "there are no matches" are different
facts. **Implemented for DjVu**, and its specification is deleted; §56.7 is
what it cost. XPS is unbuilt (§55.6).

**§59.3** Class C has text by construction, and the hard part is not finding
it but mapping a hit to a rectangle on a paginated render — which only exists
at the pinned width (§57.3).

**§59.4** All three classes pin `NotesMapping::SlidesOnly` for the reasons in
§46.4. None of them carries a `.pdfpc` sidecar.

---

## 60. Document mode across the classes

**§60.1–§60.2** Implemented and deleted: every non-PDF class reports
`Unsupported` for all `DocumentBackend` operations, and the reader states the
limitation rather than leaving it to be discovered by pressing a control that
refuses.

**§60.3** Annotation of Class A and Class B documents is **not planned**. It
would require a per-format sidecar — the one thing `SPEC-document.md` A1
explicitly refuses for PDF, on the grounds that a second copy of the
annotations can drift from the document. Introducing exactly that for other
formats would undo the invariant rather than extend it. See §65.4.

---

## 61. What is refused, and how

**§61.1** An unsupported format MUST be refused **by name**, saying what it
is and what would be needed. Implemented for every format §64 names, in one
table (`crates/pulpit-render/src/formats.rs`), plus the missing-DjVu message.

**§61.2** A refusal MUST NOT be reported as a corrupt file. "pulpit cannot
read this kind of file" and "this file is damaged" are different facts, and
telling a presenter the second when the first is true sends them looking for a
problem that does not exist.

**§61.3** Format detection for refusal messages MAY sniff content, unlike the
listing rule in §41.1. Naming the format correctly is worth reading sixteen
bytes. **Not done**, and it is a MAY: the extension is right often enough
that the refusal is honest, and a wrong guess costs a wrong name in a message
rather than a wrong render. Worth revisiting only if renamed files turn out to
be common.

**§61.4** Every format §64 names MUST be refused with **its own message and
its own reason** — deferred, not planned, or convert-instead — and the refusal
MUST be reached **before any library is bound**, so that naming a format never
depends on PDFium or djvulibre being installed (§65.2).

Implemented. One table in `crates/pulpit-render/src/formats.rs`, consulted at
the top of the router's `open`, both image open paths, the worker's routing in
`main.rs`, and the application's `open_document`. Before it, the router refused
`.cbr` and `.cb7` by name and sent **everything else** to the PDF backend, so a
`.ps`, `.epub`, `.xps`, `.dvi`, `.mobi`, `.chm` or `.odt` failed as a damaged
PDF — the §61.2 violation — and §55.7's advice to convert PostScript was never
given anywhere.

---

## 62. Ordering

What is left, in order of risk rather than appetite:

**§62.1** §61.4's refusal table. **Done.**

**§62.2** §59.2's DjVu text layer. **Done.**

**§62.3** §54.6's `.cb7`, only if a maintained pure-Rust 7z decoder exists.

**§62.4** Class C last, if ever, and only for document mode. §57.2 is a real
architectural conflict and it should not be attempted while anything else is
outstanding.

XPS (§55.6) and PostScript (§55.7) are not in this list: they are deferred
decisions, and the only thing owed to them is a refusal message (§61.4).

---

## 63. Testing

**§63.1–§63.2** Class A's bounds and listing tests, and Class B's
skip-with-a-message convention when the library is absent, are in place.

**§63.3** §61.4's refusal table MUST be table-driven and tested per extension:
that the message names the format, that it is not the corrupt-file message,
that it says what to do instead, and that the PDF backend is never bound. Done,
in `formats.rs` and in the router's own test, both iterating the table rather
than naming extensions — a format added to §64 without a message fails a test.

**§63.4** Class C: pagination at a pinned width MUST be a golden test — same
input, same width, same page count — or §57.5's identity guarantee is
unverifiable.

---

## 64. Summary of decisions

| Format | Class | Decision |
|---|---|---|
| `.cbz` `.cbt` | A | **Implemented** |
| `.cb7` | A | Deferred — refused by name; in scope if a pure-Rust decoder exists (§54.6) |
| `.cbr` | A | **Not planned** — unrar licence (§54.7) |
| DjVu | B | **Implemented** — discovered djvulibre, view-only, searchable (§59.2) |
| XPS | B | Deferred; low value (§55.6) |
| PostScript | B | Deferred; advise conversion to PDF instead (§55.7) |
| DVI | B | **Not planned** — needs a TeX installation (§55.8) |
| EPUB, Mobi, FB2, CHM, Markdown | C | **Not planned for the presenter**; document mode only, after §57 (§57.6) |
| ODT | C | **Not planned** — lossy conversion, and its producers export PDF |

Every format in this table that pulpit does not read is refused by name, with
its own message and its own way forward (§61.4).

---

## 65. Standing constraints these decisions rest on

**§65.1** Never bundle a format library (§55.3). PDFium is the single
exception and it is the reason the application exists.

**§65.2** Never let a format's absence break another format (§56.1).

**§65.3** Never let pagination depend on a window size (§57.3).

**§65.4** Never introduce an annotation sidecar (§60.3).

**§65.5** Never report "unsupported" as "corrupt" (§61.2).

**§65.6** Never take a dependency whose licence the package cannot carry
(§54.7), and never take a native dependency where a maintained pure-Rust one
would do (§54.6).

**§65.7** Never extract an archive entry to disk, and never decompress one
unbounded (Class A, implemented).

---

## 66. Re-litigating this

These are decisions, not conclusions from measurement, and the numbers behind
them are in `SPEC-images.md` §52 and in the sizing done alongside it. A
concrete user need for a specific format is a good reason to revisit §64; an
observation that a format "would be nice" is not, and this file exists so that
conversation starts from what was already decided.
