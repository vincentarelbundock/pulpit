# pulpit image-directory specification

Companion to `SPEC-document.md`. Adds §40–§52.

Presenting a folder of images as a paginated, read-only document: the
directory takes the role the PDF file takes today, each image is a page, and
the existing overview grid becomes a contact sheet for the folder.

Scope is deliberately one tier. This document covers **raster images decoded
in-process by the `image` crate**. PostScript, DjVu, DVI, XPS and the
reflowable formats (EPUB, Mobipocket, FB2, CHM, Markdown) are out of scope and
are not deferred features of this design — they need a different page model or
a bundled native library, and §52 records why.

---

## 40. The document is the directory

**§40.1** An image document's source MUST be a directory. The directory is
listed **non-recursively**; subdirectories are ignored. Opening
`~/Pictures/talk/` yields a document whose pages are the supported image files
directly inside it.

**§40.2** Opening an image *file* MUST resolve to its parent directory, with
the initial committed page set to that file. This is what makes "open a
screenshot, get an image viewer" work, but it means the document is larger
than what the presenter picked, so §40.3 applies.

**§40.3** When a document was opened by §40.2, the presenter view MUST state
the resolved directory and its page count before any navigation happens.
Silently sweeping up 400 siblings is the failure mode this rule exists to
prevent.

**§40.4** Ordering MUST be a deterministic natural sort over file names —
`img2` before `img10` — with case folded for comparison and the raw name
breaking ties. `readdir` order MUST NOT reach the page table: it is neither
stable across platforms nor across runs, and page identity (§43) depends on
the order being reproducible.

**§40.5** The page table MUST be capped at `MAX_TRACKED_PAGE_SIZES` (4096)
entries. A directory with more images is refused, naming the count. See §46.3
for why truncation and sampling are both worse.

---

## 41. Supported extensions

**§41.1** The supported set is decided by **file extension alone**, never by
sniffing content. Listing a directory must be cheap and must not depend on
whether a file happens to be readable at that instant.

**§41.2** The set is: `png jpg jpeg gif bmp webp tif tiff qoi tga ico pnm pgm
ppm pbm`. All decode through the `image` crate with default features and no
additional native dependency, which is the property that keeps this tier at
roughly two megabytes of added binary (§52.1).

**§41.3** Animated GIF and WebP present their **first frame**. A presenter
window is not a media player; motion belongs to `pulpit-media`.

**§41.4** SVG is excluded. It is vector content needing a full renderer, and
that is a different decision with a different dependency.

**§41.5** The set MUST live in one constant in `pulpit-render` and be consumed
from there by the page table, the watcher predicate
(`doc/watcher.rs::is_the_watched_file`) and the file dialog filters in
`app.rs`. Three hand-maintained copies of this list would drift, and the
symptom — a file that appears in the grid but does not trigger a reload — is
invisible until it matters.

---

## 42. Who owns the page table

**§42.1** The **application** owns the page table. It lists the directory
itself, holds the ordered file names, and uses them for identity (§43) and for
the stability probe (§44). None of that requires decoding anything.

**§42.2** The renderer worker derives the *same* table independently, from the
same directory path, using the same shared deterministic function. The worker
is not sent a 4000-entry list over the protocol.

**§42.3** Two independent listings can disagree, because the directory can
change between them. `DocumentMetadata` therefore gains
`source_digest: Option<u64>` — a digest over the ordered
`(name, len, mtime)` triples. The application MUST compare the worker's digest
with its own and, on a mismatch, treat the open as stale and re-drive it
through the ordinary candidate/promote path.

**§42.4** This is the whole reason the race is acceptable: a disagreement is
*detectable*, and the recovery is machinery that already exists and is already
tested. A silently mismatched table would put the wrong picture on the
projector with nothing to notice it.

---

## 43. Page identity across a reload

**§43.1** For a PDF, `PresentationState::replace_document`
(`pulpit-core/src/state.rs:415`) keeps the committed and preview *indices* and
clamps them to the new page count. That is correct for a recompiled deck:
slide 7 is still slide 7.

**§43.2** It is wrong for a directory. Adding a file earlier in sort order
shifts every later index, so index 7 becomes a different picture — the
audience frame changing to unrelated content with no navigation, which rule 3
forbids.

**§43.3** For an image document the identity of a page is its **file name**,
not its index. Before applying `Command::ReplaceDocument`, the application
MUST translate the committed and preview positions from names to their indices
in the new table, and issue the corresponding navigation immediately after.

**§43.4** `pulpit-core` MUST NOT learn what a file name is. The translation
lives in the application, next to the page table it already owns (§42.1), and
`replace_document` keeps its current index semantics unchanged. The domain
crate stays pure, and the mapping is an ordinary unit test over two vectors of
names.

**§43.5** When the committed file is **gone** from the new table, the position
MUST fall to the nearest surviving neighbour by sort position, not to page 0.
Deleting the picture on screen should advance to the next one, which is what a
presenter expects and what an index-clamp cannot express.

---

## 44. Knowing when the directory has settled

**§44.1** `doc/manager.rs` debounces a watch hint and requires the source to
present the same stamp twice before it opens a candidate. `FileStamp { len,
modified }` describes one file.

**§44.2** That probe is blind to the case that matters here. A directory's
mtime moves when a member is added or removed, but **not** when a member's
contents are overwritten — an export re-writing `slide03.png` in place would
never be noticed.

**§44.3** The stamp therefore becomes an enum:

```
SourceStamp::File { len, modified }
SourceStamp::Directory { entries: usize, digest: u64 }
```

where `digest` is §42.3's digest over the ordered `(name, len, mtime)`
triples. The manager only ever compares stamps for equality, so the enum drops
into the existing logic unchanged, and `FileProbe` is already abstracted for
exactly this kind of test.

**§44.4** A directory whose digest is still moving MUST be treated as an
unstable source, on the same debounce and backoff as a half-written PDF. A
folder receiving fifty files from a camera import is the normal case, not an
error.

---

## 45. Two backends in one worker

**§45.1** `select_backend()` (`bin/worker.rs:35`) currently chooses once at
startup, before any path is known. A worker holds several documents at a time
— always two during a reload — and after this change they need not be the same
kind.

**§45.2** Selection therefore moves to `open`, behind a router that dispatches
per `BackendDocumentId`. A directory source routes to the image backend, a
file source to PDFium.

**§45.3** **This softens a documented invariant.** Today a worker that cannot
bind PDFium prints `missing_pdfium_message` and exits, because a presenter
must never be shown placeholder slides. After this change the worker MUST
still exit that way when a **PDF** is opened, but MUST NOT exit at startup:
refusing to display a JPEG because a PDF library is absent is not defensible,
and the reasoning behind the original rule — a deck silently rendering as
blanks — does not apply to a format the worker can fully decode.

**§45.4** PDFium binding is therefore lazy, attempted on the first PDF open.
The diagnostic text is unchanged; only its timing moves.

---

## 46. Metadata, sizes and notes

**§46.1** Page sizes MUST come from **header-only** dimension reads
(`image::ImageReader::into_dimensions`), never a full decode. Opening a folder
of high-resolution photographs must not stall on pixel data nobody has asked
for yet.

**§46.2** Aspect ratios varying wildly from page to page is already a
supported case: `DocumentInfo::page_sizes` exists because decks glue a 4:3
appendix onto a 16:9 talk.

**§46.3** The sampling fallback beyond `MAX_TRACKED_PAGE_SIZES` — pages past
the bound answer with the *first* page's size — rests on pages being
"overwhelmingly uniform" (`pulpit-core/src/document.rs:112`). That holds for
scanned decks and is false for a photo directory. Rather than let it return a
confident wrong aspect ratio, §40.5 caps the page table at the bound and
refuses beyond it. `page_sizes_sampled` MUST therefore always be false for an
image document.

**§46.4** Notes MUST be pinned to `NotesMapping::SlidesOnly`. An image
document MUST NOT consult `Settings::default_mapping`, and MUST NOT record a
mapping through `remember_mapping`. A presenter whose default is a `SplitPage`
mapping would otherwise have every photograph cut down the middle with its
right half treated as speaker notes.

**§46.5** There is no `.pdfpc` sidecar, no embedded attachment and no metadata
text. `metadata_text` MUST be empty rather than synthesised from file names.

---

## 47. Decoding

**§47.1** The image backend holds a small **byte-bounded LRU of decoded
images**, in the worker. Re-scaling one decoded image across the audience
frame, the presenter frame and a thumbnail must not decode it three times.
This cache is distinct from the frame cache and from `thumbnails.rs`, and its
budget is its own.

**§47.2** Input pixel dimensions MUST be bounded before decode. The existing
16384px limit in `RenderRequest::validate` bounds the *output*; a 64000×64000
PNG is a decompression bomb that never reaches it.

**§47.3** Cancellation granularity is **one image**. `CancelSignal` is checked
before decode and before scaling, but a decode is a single blocking call and
cannot be interrupted the way PDFium's progressive API can. This is a known
and accepted regression in responsiveness against the PDF path, bounded by
§47.2 and mitigated by §47.1.

---

## 48. What an image document cannot do

**§48.1** Every `DocumentBackend` operation MUST report `Unsupported`:
annotations, form fields, text selection, save, signing. These are PDF
semantics and there is nothing honest to map them onto.

**§48.2** `find_text` MUST report unsupported, not an empty result. The
existing default already does, and the distinction is load-bearing: "this
cannot be searched" and "there are no matches" are different facts.

**§48.3** The presenter and reader UI MUST reflect that rather than offering
controls that refuse when pressed.

**§48.4** `links`, `outline` and `page_labels` answer empty, which is
indistinguishable from a PDF that carries none. No special handling needed.

---

## 49. A file that will not decode

**§49.1** A file listed by §41.1 that fails to decode MUST remain in the page
table and MUST fail its render with `PdfError::Render`.

**§49.2** It MUST NOT be dropped from the table. Dropping it would shift every
later index and so violate §43, and would hide the fact that a file is broken
by making it look like it was never there.

**§49.3** It MUST NOT render a placeholder image. The existing rule stands:
the last complete audience frame holds, and the presenter view names the file
that failed.

---

## 50. What this does not change

**§50.1** The overview grid and `thumbnails.rs` require **no changes at all**.
Both are driven by page index over whatever the backend reports, so a contact
sheet of the folder falls out of §40 for free. Any future change to this
design MUST preserve that: if the image path needs its own overview code, the
page model has gone wrong.

**§50.2** The watcher already watches the containing directory
non-recursively, for `typst watch` rename-over semantics. Only its filename
predicate widens (§41.5).

**§50.3** `reconcile()`, the display ladder, roles and swap are untouched. An
image document is a document.

---

## 51. Testing

None of this requires PDFium, which is the point of putting the page table in
pure code.

**§51.1** Natural sort, including case folding, tie-breaking and the
`img2`/`img10` case.

**§51.2** Digest stability: same directory, same digest; a member overwritten
in place, different digest (§44.2 is the regression this guards).

**§51.3** Name-anchored re-indexing across insert, delete, rename and reorder,
including §43.5's fall to the nearest surviving neighbour.

**§51.4** The extension set has exactly one definition (§41.5) — a test that
the watcher predicate and the dialog filters are derived from it, not
restated.

**§51.5** A directory over the §40.5 cap is refused, and an empty directory
reports no pages through the existing zero-page refusal in `manager.rs:282`.

**§51.6** A corrupt file among good ones: still counted, still positioned,
fails its own render, leaves neighbours renderable (§49).

---

## 52. Deferred scope

**§52.1 Why this tier and no further.** Images add roughly two megabytes to a
binary that is already about 75MB stripped, and no native dependency. Every
other Okular format either needs a pinned native library on five platforms —
the recurring packaging tax, not the binding, being the real cost — or does
not fit a paginated model at all.

**§52.2 PostScript.** libspectre is glue over Ghostscript: 20–40MB with fonts,
roughly a third onto the package, and AGPL-3.0 against this project's
`MIT OR Apache-2.0`. If it is ever done it should use an installed Ghostscript
and report `Unsupported` otherwise, in the way `pulpit-media` uses an installed
Chromium rather than bundling one.

**§52.3 DjVu, XPS.** Page-oriented, so they would fit this design almost
exactly. Blocked on packaging appetite, not on architecture.

**§52.4 DVI.** Needs a TeX font tree to render anything. Not bundlable.

**§52.5 Reflowable formats.** EPUB, Mobipocket, FB2, CHM and Markdown have no
page count or page size until a viewport is chosen, and the presenter and
audience windows are different sizes — so "page 7" would name different
content in each. That is a rule 3 violation, not a missing feature. If it is
ever attempted, the route is paginating through the already-external Chromium
at one pinned width, making it one backend rather than five layout engines.
