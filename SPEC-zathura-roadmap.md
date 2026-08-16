# Zathura-informed roadmap for Pulpit

Status: product analysis and prioritization, not an implementation specification.

This document evaluates the feature set of Zathura 2026.07.18 as a source of
product ideas for Pulpit. It does not make feature parity a goal. Zathura is a
minimal, keyboard-first document reader; Pulpit is first a reliable two-window
PDF presenter and is becoming a reader, annotator, and form filler. A Zathura
feature belongs in Pulpit's roadmap only when it improves that product without
weakening the audience-frame, process-isolation, platform-boundary, or native-
annotation invariants in `docs-src/internals.typ` and `SPEC-document.md`.

The comparison is against the current working tree, including the unreleased
document-mode work recorded in `CHANGELOG.md`. “Existing” therefore does not
necessarily mean present in the latest published release.

## 1. Priority model

| Priority | Meaning |
|---|---|
| **P0 — finish and harden** | Already promised or substantially implemented. Complete it before expanding document mode. |
| **P1 — next roadmap** | High-value, product-aligned work with a bounded design and no architectural conflict. |
| **P2 — later** | Useful after P0/P1, or valuable to a narrower audience. |
| **P3 — opportunistic** | Small convenience or specialist feature; accept only when its cost is demonstrated. |
| **Do not add** | Conflicts with Pulpit's product, security posture, portability, or deliberate architecture. |

Priority is assigned to outcomes, not Zathura's exact interaction. Pulpit MUST
reuse semantic commands, the existing layout/widget system, explicit `Outcome`
values, and the supervised PDF worker. It MUST NOT import Zathura's Vim command
language, GTK-specific behavior, or backend-pluggability merely to reproduce
surface parity.

## 2. Executive roadmap

### P0 — finish the reader already specified

1. Complete and stabilize Reader mode, native annotation editing, form filling,
   Save As verification, dirty-state handling, and the live gesture-to-render
   handoff already described by `SPEC-document.md`.
2. Finish crash recovery for unsaved document edits and retire every remaining
   second representation of committed marks.
3. Productize the search, outline, page-label, link, encrypted-document, and
   document-capability paths already present in the code: complete keyboard and
   accessibility behavior, error states, hostile-input bounds, and end-to-end
   tests.
4. Preserve and test the presentation fundamentals Zathura also validates:
   automatic reload, explicit reload, fullscreen, fit modes, exact zoom, link
   following, and page navigation—without weakening the last-valid-audience-
   frame rule.

### P1 — the best additions inspired by Zathura

1. **Persistent reading positions and recent documents.** Remember page,
   viewport, zoom/fit mode, Reader layout, and last-open time per document.
2. **User bookmarks and quick jumps.** Add Pulpit-owned bookmarks distinct from
   the PDF outline, plus a fast temporary quick-jump mechanism for rehearsals
   and long documents.
3. **Reader comfort controls.** Add page rotation and a constrained dark/recolor
   mode to Reader only.
4. **Export embedded attachments.** Pulpit already reads attachments for pdfpc
   notes and media; expose a safe, size-bounded Save As flow for arbitrary
   attachments without executing them.
5. **Document information and signature inspection.** Present metadata,
   permissions, encryption, forms, unsupported actions, embedded files, and
   existing signature status in one durable capability/details panel. This is
   inspection, not identity validation.
6. **Print through a platform capability.** Print the saved document revision,
   with dirty-state disclosure and an explicit unsupported outcome.

### P2 — useful later

1. Multi-page/facing-page Reader layouts, including first-page alignment and
   right-to-left ordering.
2. More granular scrolling preferences: page-aware movement, overlap, and
   optional wraparound.
3. SyncTeX forward and backward synchronization through a capability-based,
   opt-in editor integration.
4. Copy/save embedded images when PDFium can identify them without rasterizing
   an arbitrary selection.
5. Open from standard input by spooling to a private bounded temporary file.
6. Presentation-mode polish that remains presenter-side, such as a distraction-
   free single-window reading presentation—not a replacement for the audience
   window workflow.

### P3 — opportunistic conveniences

1. Negative page numbers and “open with initial search” command-line options.
2. Page-number offsets where PDF page labels do not already solve the problem.
3. Optional first-page-on-open and vertical-centering preferences.
4. Advanced title/status formatting and configurable recent-file counts.
5. External commands with a fixed, escaped placeholder vocabulary, if a real
   automation use case justifies the security and portability cost.

### Do not add

1. A document-renderer plugin ABI or alternative Poppler/MuPDF runtime backends.
2. A Zathura-style arbitrary command language or unrestricted shell execution.
3. Executing PDF JavaScript, launch actions, submission, email, upload, or
   arbitrary attachment actions.
4. An experimental “strict viewer” binary that disables core state piecemeal;
   preserve the existing worker boundary and package sandboxing instead.
5. GTK embedding, tabbed-container integration, or desktop-specific behavior
   above `pulpit::platform`.
6. Zathura-style interface theming that bypasses Pulpit's seven color roles and
   design tokens.

## 3. Detailed feature analysis

### 3.1 Opening and document lifecycle

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Open PDF files | Core Pulpit behavior. Reader mode now opens ordinary PDFs as documents rather than only decks. | **P0 existing.** Maintain one open-document model across Reader and Presenter. |
| Poppler or MuPDF PDF backends | Pulpit deliberately standardizes on dynamically loaded PDFium and supervises it out of process. Multiple engines would multiply rendering, annotation, form, link, and save semantics and weaken the cross-viewer test oracle by turning it into production behavior. | **Do not add.** Keep MuPDF/Poppler as independent verification tools in tests. |
| Password-protected/encrypted PDFs | The document protocol already carries a redacted password and `SPEC-document.md` requires encryption and permission findings to be explicit. | **P0 hardening.** Complete password prompting, retry/cancel behavior, permission enforcement, secret redaction, save semantics, and test coverage. Never persist passwords in recovery state. |
| Open at a specified page | Pulpit can navigate directly; exposing the initial page is useful for links from other tools and rehearsal scripts. PDF page labels should be accepted as well as zero/one-based internal indices. | **P2** for stable URI/CLI integration; **P3** for Zathura-compatible negative numbering. |
| Open and immediately search | Search already exists in both presenter and reader paths. An initial query is mostly a CLI/deep-link convenience. | **P3.** Add only after the search UI and result semantics are P0-complete. |
| Read from standard input | Useful in Unix pipelines but conflicts with reopening, file watching, source identity, recovery fingerprints, and Save As provenance unless input is first materialized. | **P2.** Spool to a private, permission-restricted, size-bounded temporary file; label it unsaved; disable reload; require Save As for durable output. |
| Open without a document | Pulpit already starts empty. | **Existing; no roadmap item.** |
| Remember last page and viewport | Pulpit has presentation recovery, and `SPEC-document.md` includes page and zoom in a future document snapshot, but this is not the same as durable per-document reading history. | **P1.** Store fingerprint-keyed page, scroll anchor, zoom/fit mode, Reader layout, and timestamp. Reject stale identity rather than applying position to a changed file blindly. |
| Always open on first page | Useful as a preference for presenters and privacy-sensitive readers, but should not override explicit page/deep-link requests. | **P3.** A semantic opening policy: resume, first page, or explicit target. |
| Automatic reload on file change | Already a central Pulpit feature with debounce, atomic promotion, generations, and last-good-frame retention; its guarantees are stronger than Zathura's basic reload. | **P0 existing.** Keep regression coverage for partial writes, failed rebuilds, and dirty Reader documents. A dirty document MUST not be silently replaced. |
| Manual reload | Already bound and implemented. | **P0 existing.** In Reader mode, require an explicit decision if reload would discard unsaved edits. |
| Document metadata | Pulpit reads metadata and capabilities internally but lacks one comprehensive user-facing inspector. | **P1.** Add a document-details surface containing metadata, page count/labels, file identity, permissions, encryption, forms, signatures, attachments, unsupported features, and active fallbacks. |
| Page-number offsets | Pulpit already reads PDF page labels, which are a more faithful solution for roman front matter and custom numbering. Manual offsets remain useful for malformed or label-less documents. | **P3.** Prefer page labels; offer a session/document override only after confirming real demand. |

### 3.2 Navigation and scrolling

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Keyboard-first navigation | Already a standing invariant and now Vim-friendly without removing conventional keys or remote mappings. | **P0 existing.** Finish Reader focus order and ensure every pointer operation has a keyboard alternative. |
| Arrow, Vim, Page Up/Down, Space, Home/End movement | Present in Presenter; Reader has continuous navigation machinery. Exact commands must remain semantic and mode-aware. | **P0 hardening.** Test long documents, focused form fields, annotations, and search so keystrokes go to the correct owner. |
| Half-page and full-page scrolling | Valuable in Reader, not Presenter. | **P2.** Define movement relative to the viewport in logical units and retain a visible overlap. |
| Configurable scroll step | Fine-grained preference with limited product value and accessibility implications. | **P3.** Prefer a small set of semantic step sizes over arbitrary pixels. |
| Page-aware scrolling | Helps readers avoid losing page boundaries in continuous mode. | **P2.** Make it a Reader preference; never change Presenter page-turn semantics. |
| Full-screen overlap setting | Useful, but a numeric proportion is configuration surface most users should not need. | **P3.** Choose a good default first; expose only if evidence shows need. |
| Wrap between first and last page | Risky during a talk and surprising in a document. | **P3.** If added, Reader-only and off by default. Presenter MUST continue to clamp. |
| Jump to top/bottom of current page | Useful for long pages and posters in Reader. | **P2.** Define behavior under rotation, crop, multi-page rows, and fit-page mode. |
| Snap to current page | Useful after free panning/scrolling. | **P2.** A semantic “center current page” command also usable by search and outline navigation. |
| Jump history with back/forward | High value when following outline entries, links, bookmarks, and search results. Pulpit currently has navigation state but not a clearly user-facing cross-source jump stack. | **P1.** One bounded history of document locations with back/forward commands; ordinary sequential scrolling should not flood it. |
| Bisect between two jump points | Clever but specialist Zathura behavior with weak discoverability. | **P3.** Do not reserve default keys; consider only as a command if requested. |
| Right-to-left page ordering | Necessary for facing-page Arabic/Hebrew documents and manga; text selection already encounters RTL content but page order is separate. | **P2.** Add to the multi-page-layout milestone with explicit logical reading order and tests. |
| Page-aware advancement in multi-column mode | Required if multi-page rows ship; otherwise page turns can skip or repeat rows. | **P2 dependency.** Specify together with pages-per-row, not as an independent toggle. |

### 3.3 Page layout, zoom, and transforms

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Continuous scrolling | Already specified and implemented for Reader with visible-page planning. | **P0.** Harden virtualization, mixed page sizes, rotation, high DPI, cache pressure, and focus retention. |
| Single-page view | Presenter is inherently page-at-a-time; Reader is continuous. A Reader single-page layout can help forms, comics, and distraction-free review. | **P2.** Implement as a viewport/layout policy, not a second rendering path. |
| Dual-page/facing-page view | Valuable for books and handouts, less relevant to slides. | **P2.** Add only after continuous single-column Reader is stable. |
| Arbitrary pages per row | Generalizes dual-page view but greatly expands viewport, keyboard, selection, and memory cases. | **P2**, initially bounded to one or two pages; arbitrary counts require measured demand. |
| First-page column/alignment | Essential for correct facing-page spreads where a cover stands alone. | **P2 dependency** of dual-page view. Use a semantic cover/spread policy rather than Zathura's compact colon syntax. |
| Horizontal and vertical page gaps | Reader already needs a mount and gap. | **Existing implementation detail.** Keep design-token based; do not expose raw pixel settings unless needed for accessibility. |
| Vertical page centering | Minor reading preference, useful in single-page mode. | **P3.** Layout policy in logical units. |
| Fit page and fit width | Explicitly part of `DocumentNav`; fit-to-cell already exists in Presenter. | **P0 existing.** Preserve fit mode across resize without storing physical DPI. |
| Incremental zoom | Free zoom is already specified and implemented in Reader. | **P0 existing.** Keep zoom centered on a stable document point and test mixed DPI. |
| Exact zoom percentage | Useful for technical documents and comparisons. | **P1 finish/polish.** Expose through `DocumentNav`; retain logical scale semantics. |
| Configurable min/max/step | Bounds are necessary; user-configurable numerical limits are not initially necessary. | **P3.** Ship safe constants and expose preferences only on evidence. |
| Horizontally centered zoom | Correct anchor behavior matters more than a boolean preference. | **P0 quality.** Zoom around cursor or viewport center according to the initiating interaction, with deterministic fallback. |
| Link-specified zoom | PDF destinations may include fit and rectangle semantics. Pulpit's exact-size cache already recognizes crop-dependent frames. | **P1 hardening.** Honor safe internal destination geometry while preserving a back-stack entry; never let it affect the audience unexpectedly during presenter preview. |
| Rotate pages by 90 degrees | High-value Reader feature for scanned or incorrectly authored documents. Rotation must be a view transform unless the user explicitly saves a document edit. | **P1.** Start with session/per-document view rotation; test links, search highlights, forms, annotations, and canonical coordinates. |

### 3.4 Search, links, and outlines

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Forward/backward full-text search | Already implemented across page text, speaker notes, and outline text—a broader scope than Zathura. | **P0.** Finish empty/unsearchable states, cancellation, long-document progress, accessibility, and tests. |
| Incremental search | Current state can restart searches as the query changes. | **P0 polish.** Debounce input, cancel stale generations, and ensure partial results never masquerade as complete results. |
| Next/previous result | Present in the semantic search model. | **P0 existing.** Maintain a stable current hit as chunks arrive. |
| Highlight all/current result | Reader already distinguishes ordinary and current hit geometry. | **P0 existing.** Presenter-side only; search chrome and highlights MUST never leak to the audience unless navigating commits a page normally. |
| Search result centering | Useful for Reader. | **P1 polish.** Scroll the hit into view with context; do not force horizontal centering when a rectangle/column destination is more meaningful. |
| Search color customization | Pulpit's seven-role design vocabulary forbids arbitrary component colors. | **Do not copy literally.** Derive current/other hit styles from `accent`, `surface`, and contrast rules. |
| Keyboard link labels | Pulpit supports link focus and activation but removed default focus keys because presentation link traversal is rare. Label-based activation could be faster in dense technical PDFs. | **P2.** Reader-first accessibility/product study; implement as semantic numbered hints, not a Zathura-specific mode, and never draw them on the audience. |
| Mouse link following | Already supported in Presenter and Reader when no annotation tool owns the press. | **P0 existing.** Preserve precedence among editable marks, forms, interactive overlays, and links. |
| Single/double-click link policy | A preference adds ambiguity with annotation selection and text interactions. | **Do not add as a global toggle.** Use conventional single-click activation with clear precedence; reserve double click for editing textual marks as specified. |
| Show or copy link target | Valuable safety affordance before opening external URLs. | **P1.** Show sanitized destination on hover/focus and provide Copy Link; external navigation remains an explicit platform outcome. |
| Confirm external links | Useful for untrusted PDFs, but confirmation on every internal link is noise. | **P1.** Confirm external schemes according to policy; refuse unsupported or dangerous actions explicitly. |
| Honor link destination alignment/zoom | Part of correct PDF navigation. | **P1.** Implement safe `/XYZ`, `/Fit*`, and `/FitR` semantics through canonical destinations and jump history. |
| Browse hierarchical PDF outline | Already implemented in the Reader outline rail; Presenter also uses outline sections. | **P0.** Complete keyboard expansion/collapse, labels, URI-entry treatment, accessibility, and large-outline bounds. |

### 3.5 Bookmarks, quickmarks, and history

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Persistent user bookmarks | PDF outlines are authored navigation, not personal bookmarks. Persistent bookmarks would benefit reading, reviewing, and rehearsal. | **P1.** Store portable document locations outside the source PDF by default; include label, page identity/label, canonical point, and fingerprint. Offer annotation-based durable marks separately. |
| List, rename, jump to, delete bookmarks | Necessary lifecycle around bookmarks. | **P1**, in the existing `DocumentOutline` widget as a Bookmarks view; full keyboard operation and undo for destructive list edits. |
| Quickmarks assigned to letters/numbers | Excellent for jumping among nonadjacent slides during rehearsal and Q&A, but persistent letter namespaces can become obscure. | **P1.** Start as session-scoped named slots with visible feedback; persistence can follow if users rely on it. Do not consume remote navigation keys. |
| Persistent input history | Search-query history has some value; command-line history does not because Pulpit has no command language. | **P3.** Recent searches only, opt-in/clearable and bounded. |
| Recent documents | High-value file-opening convenience. | **P1.** Use platform-standard storage, canonical non-UTF-8-safe paths, missing-file handling, privacy controls, and a clear-history action. |
| Plain/SQLite/null history backends | An implementation choice, not product value. Multiple storage backends add migrations and support burden. | **Do not add.** Use Pulpit's existing atomic/versioned settings store; offer history off/on and clear controls. |
| Save history on every page change | Excess writes are unnecessary. | **Do not copy literally.** Debounce position persistence and flush on lifecycle events; presentation crash recovery remains separate in durability policy. |

### 3.6 Text, images, attachments, forms, and annotations

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Select and copy PDF text | Text extraction and selection exist for highlights, and native text editing already requires clipboard semantics. Plain copy is an expected Reader capability. | **P0/P1 boundary.** Complete selection without forcing highlight creation; support keyboard selection where PDFium permits and copy normalized text to the platform clipboard. |
| Primary-selection vs clipboard choice | X11-specific vocabulary violates portable persisted settings. | **Do not add as a cross-platform setting.** Ask `PlatformServices` what clipboard capabilities exist and use platform conventions. |
| Selection notification | A toast after every copy is noisy; accessible confirmation can be useful. | **P3.** Use a subtle presenter/reader-side status only when the platform does not already confirm the action. |
| Copy/save embedded images | Useful for research and teaching preparation, but image discovery and exact encoded-image extraction may be backend-limited. | **P2.** Offer only for images PDFium can identify and export faithfully; otherwise label rasterized-region export honestly as a different feature. |
| Export embedded attachments | Pulpit already enumerates/reads attachments for notes and media under strict size limits. A generic safe export is a natural extension. | **P1.** List names, media type if known, and size; sanitize destination names; never auto-open or execute; stream/bound memory and return explicit outcomes. |
| Region highlighter | Pulpit's durable highlighter is text-semantic `/Highlight`, while a freehand translucent mark is `/Ink` or transient. Zathura's rectangular visual highlighter must not create a second mark representation. | **Existing by different design.** Keep text highlighter and ink tools; a transient reading ruler/region tint could be P3 but must not save as fake text highlighting. |
| Native ink annotations | Implemented and central to `SPEC-document.md`. | **P0.** Finish live preview, cross-viewer fidelity, worker replay, and recovery. |
| Free text, sticky notes, stamps/checks | Implemented or specified in the document-mode milestones. | **P0.** Finish interaction, appearance, accessibility, editing, and preservation tests before adding more annotation subtypes. |
| Annotation selection, move, resize, grouped erase, undo/redo | Substantially specified and implemented in Reader. | **P0.** Complete keyboard alternatives, transaction boundaries, stable IDs, and unsupported-annotation behavior. |
| AcroForm filling | Current unreleased work uses PDFium's form-fill environment rather than imitating field appearance. | **P0.** Finish the spike gates, JavaScript-denial tests, corpus, Save As verification, compatibility status, and crash behavior. |
| Save edited PDF | Pulpit's safe `Save As` validates and atomically publishes a new file while preserving the source. | **P0.** Keep source immutability. Zathura's force-overwrite behavior does not justify in-place save. |
| Force overwrite | Useful only for replacing an existing destination, not the source. | **P1 polish.** A confirmed overwrite may replace a non-source destination through the same verified atomic Save As path. Never introduce an unverified shortcut. |

### 3.7 Color and reading comfort

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Recolor/invert pages | Strong Reader comfort feature, especially for long reading sessions. On the audience it changes authored slide colors and can corrupt pedagogical meaning. | **P1.** Reader-window-only post-processing, disabled for the audience by invariant. It must not alter saved content or annotation colors. |
| Custom light/dark replacement colors | Arbitrary colors conflict with the seven-role design system, but the page transform is content rather than UI. Still, unconstrained controls create contrast and fidelity problems. | **P2.** Begin with tested dark and high-contrast presets derived from sanctioned roles. |
| Preserve hue/adjust lightness | Useful sophistication after a basic dark mode proves valuable. | **P2.** Specify one perceptually sensible transform and test images/transparency; avoid a matrix of toggles. |
| Preserve image colors while recoloring text/page | Important for charts, photographs, and teaching material but difficult to do correctly without object-aware rendering. | **P2 investigation.** Do not claim it from a whole-frame pixel transform. Ship only if PDFium/object masks make the distinction reliable. |

### 3.8 Fullscreen and presentation behavior

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Fullscreen mode | Fundamental and already capability-driven in Pulpit. | **P0 existing.** Continue topology, reconnect, mixed-DPI, and compositor testing. |
| Presentation mode | Pulpit's presenter/audience workflow is the product and is substantially richer than Zathura's single-window presentation mode. | **Existing; no parity work.** Do not replace it with a generic fullscreen reader. |
| Mode-specific keybindings | Pulpit already uses semantic actions and context. | **P0 existing.** Stored bindings should target semantic commands with mode applicability, not duplicate raw key maps per view unless necessary. |
| Hide input/status/notification bars | Pulpit's audience already contains no chrome; Reader and Presenter layouts are customizable. | **P2 polish.** A focus/distraction-free Reader layout is preferable to independent bar toggles. Critical presenter errors must remain durably visible presenter-side. |

### 3.9 LaTeX and SyncTeX

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Automatic reload after recompilation | Already supported with stronger atomic-promotion guarantees. | **P0 existing.** |
| Forward SyncTeX | Valuable for authors who use Pulpit to preview a live deck or paper. It requires instance discovery and editor-to-viewer routing, but not arbitrary PDF action execution. | **P2.** Define a platform-neutral deep-link/IPC capability and map SyncTeX onto it. Never expose native window handles. |
| Backward SyncTeX | Useful in Reader for source editing; less relevant during a talk. | **P2**, after forward synchronization. Require an explicit configured editor command or registered integration, sanitize placeholders, and keep it disabled during audience interaction. |
| D-Bus service | D-Bus is Linux-specific plumbing, not the feature. | **Do not make it the domain API.** Define semantic instance/SyncTeX commands and implement D-Bus, named pipes, or platform IPC in adapters as capabilities. |
| Emit navigation signals | Useful for editor integration and automation. | **P2**, through the same semantic IPC contract with authentication/same-user restrictions where the platform supports them. |

### 3.10 Printing and signature information

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Print document | Expected of a mature reader/form tool, but it crosses the process and platform boundary and raises dirty-revision questions. | **P1.** Add `Print` to `PlatformServices` with `Done/Refused/Unsupported/Failed`. Print a verified saved revision; if dirty, offer Save As first or clearly state what revision will print. |
| Display embedded signature information | `SPEC-document.md` already requires detection and warning, while `SPEC-signing.md` carefully separates presence, cryptographic integrity, and trusted identity. | **P0/P1.** P0: warn before mutation and preserve signatures. P1: details panel with coverage and cryptographic status supported by the signing spec. Never call a signer trusted without identity assurance. |
| Signature success/warning/error colors | Pulpit forbids color-only status and arbitrary component colors. | **Do not copy literally.** Use icon, label, explanation, and centrally derived color together. |

### 3.11 Command line, automation, and configuration

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Alternate config/data/plugin directories | Alternate config/data roots can help testing and portable deployments; a plugin directory is irrelevant. | **P3** for explicit testing/portable-profile roots. **Do not add** a plugin directory. |
| Fork/background option | GUI launchers and platform application lifecycle already solve this; Unix daemonization complicates logs and worker supervision. | **Do not add** unless a packaging environment proves it necessary. |
| Logging verbosity | Diagnostics are already a Pulpit concern. | **P1 polish.** Provide documented CLI verbosity and preserve secrets redaction. |
| Execute external command with file/page placeholders | Powerful but creates quoting, injection, portability, and hostile-path risks. SyncTeX does not require a general shell. | **P3 at most.** Prefer registered semantic integrations. If implemented, use argv templates from a fixed vocabulary, never `sh -c`, and require explicit user configuration. |
| D-Bus automation | See SyncTeX analysis: transport is platform-specific. | **P2** for a small semantic control API; no general remote eval and no audience-window content injection. |
| Dump current settings | Useful for support and reproducibility if secrets and machine-specific paths are redacted. | **P2.** Fold into diagnostics rather than add a command-language primitive. |
| Source another config at runtime | Conflicts with atomic/versioned structured settings and makes runtime state hard to diagnose. | **Do not add.** Use explicit import/export with validation and migrations. |
| Command input bar and tab completion | Zathura needs this for its command language; Pulpit already exposes discoverable buttons, menus, layouts, and semantic shortcuts. | **Do not add as a Vim command line.** A future command palette may be **P2** if it searches semantic actions rather than parsing commands. |
| Remap keys, modifiers, sequences, mouse buttons | Pulpit already has an input router, semantic actions, remote capture, and stored raw scancodes. | **P0/P1.** Finish a discoverable binding editor and conflict validation. Multi-key sequences and arbitrary mouse remapping are P3 unless demanded. |
| Separate bindings by mode | Context matters, but duplicated maps can create invisible conflicts. | **P1.** Express applicability on semantic actions and show conflicts across Presenter, Reader, forms, annotations, and designer contexts. |
| Bind shortcuts to shell commands | Same concerns as external command execution. | **Do not add by default.** Registered integrations first. |
| Include config files | Structured imports are safer and portable. | **Do not add.** |
| Arbitrary UI colors/fonts/padding | Conflicts with the design-system invariants and accessibility guarantees. | **Do not add.** Support sanctioned palettes, system appearance/high contrast, type scale, and layout editing through existing tokens. |

### 3.12 Interface and desktop integration

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Minimal, space-efficient interface | Aligned with Pulpit's layout system, but presenter controls must remain discoverable under pressure. | **Continuous design constraint.** Judge Reader and Presenter independently; never remove safety state merely for minimalism. |
| Mouse-free use | Already a standing invariant. | **P0.** Include forms, text selection, annotation manipulation, outline, bookmarks, Save As, and details in acceptance tests. |
| Mouse support | “Keyboard first, pointer fully” already requires it. | **P0.** |
| File completion with hidden/directories/recent controls | Native file dialogs should follow platform conventions. | **P1** recent documents; **do not add** a parallel custom filesystem browser unless the toolkit/platform cannot provide required behavior. |
| Configurable status bar and title formatting | Pulpit's layouts are the composition mechanism; document dirty state and compatibility must remain visible somewhere. | **P3** for concise basename/page formatting. Do not allow layouts to hide the only durable record of critical state. |
| D-Bus window activation | Platform-specific instance activation belongs below the capability boundary. | **P2** only as part of SyncTeX/deep-link delivery. |
| Tabbed-container/window embedding | Removed upstream in GTK 4 and contrary to native-shell portability. | **Do not add.** If multi-document Pulpit is ever desired, specify it as an application feature rather than embedding in another shell. |

### 3.13 Document-format plugins

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Pluggable document backends | Pulpit's value depends on one PDF model shared by presenter rendering, links, forms, annotations, saving, and recovery. A document ABI would either expose unstable internals or reduce capabilities to a lowest common denominator. | **Do not add.** |
| DjVu, PostScript, comic-book archives | These formats do not carry Pulpit's PDF annotation/form/notes semantics. Adding them dilutes the product and multiplies security and packaging work. | **Do not add to the core roadmap.** Users can convert them to PDF. Reconsider only with measured demand and a format-neutral product specification. |
| Third-party plugin development API | It would create compatibility and trust obligations before Pulpit's own document mode is complete. | **Do not add.** Extend internal capability traits only when a first-party need exists. |

### 3.14 Security-oriented mode

| Zathura feature | Pulpit assessment | Priority and roadmap treatment |
|---|---|---|
| Separate strict sandbox binary | Pulpit already isolates hostile PDF and browser work in supervised child roles of one binary. A read-only alternate executable would create another behavior matrix and does not itself sandbox the GUI or platform services portably. | **Do not copy literally.** Harden the existing workers, package them under OS sandboxes where available, and keep capability denial explicit. |
| Disable writing/printing/history/IPC/SyncTeX in sandbox | These are useful policy controls, but should derive from an immutable capability/security profile rather than scattered mode checks. | **P2 security profile** only if threat modeling shows value. It MUST report every unavailable action as `Refused` or `Unsupported`, never silently no-op. |
| seccomp/Landlock confinement | Valuable Linux defense in depth for workers, but not portable product behavior. | **P2 platform hardening.** Implement in the Linux worker adapter/package after measuring compatibility; seek Windows/macOS equivalents through capabilities rather than `cfg!` in domain logic. |

## 4. Proposed delivery slices

The priorities above should be delivered in vertical slices rather than as a
large parity project.

### Slice A — Reader completion gate (P0)

- Complete `SPEC-document.md` milestones M1–M5.
- Make search, outline, links, forms, selection/copy, encrypted-open, Save As,
  and dirty-state flows keyboard-complete and screen-reader-checkable.
- Add end-to-end failure tests for partial reload, document-worker crash,
  stale search/result generations, bad passwords, permission restrictions,
  signed-document mutation warnings, and Save As validation.
- Update public usage documentation only after these flows are shipped.

Exit criterion: an ordinary searchable, linked, outlined, annotated, encrypted,
or AcroForm PDF can be read and safely saved without a mouse and without an
unexplained degradation.

### Slice B — Return to places (P1)

- Per-document reading position and recent documents.
- Bounded jump back/forward history.
- User bookmarks and session quickmarks in `DocumentOutline`.
- Search-hit and internal-link destinations participate in the same history.

Exit criterion: a user can leave a long document, return later, and move among
personally meaningful locations without memorizing page numbers.

### Slice C — Inspect and take out (P1)

- Document details/capabilities panel.
- Safe attachment listing and export.
- External-link target display and Copy Link.
- Signature presence/coverage information consistent with `SPEC-signing.md`.
- Platform print capability with dirty-revision handling.

Exit criterion: everything the PDF can cause or contain is disclosed before it
leaves the process, and export/print operations say exactly which revision was
used.

### Slice D — Reading comfort (P1/P2)

- Reader-only view rotation.
- Reader-only dark/recolor preset with no audience effect.
- Single-page and two-page Reader viewport policies.
- Cover alignment, right-to-left order, page-aware keyboard movement, and jump
  history integration.

Exit criterion: books, papers, forms, scans, and comics are comfortable to read
without introducing a second render pipeline or persisting physical display
assumptions.

### Slice E — Author integration (P2)

- Platform-neutral instance/deep-link protocol.
- SyncTeX forward synchronization.
- Opt-in backward synchronization using an argv template, not a shell string.
- Platform adapters for delivery and activation.

Exit criterion: source-to-PDF and PDF-to-source navigation work without D-Bus
types, editor process handles, or native window handles entering domain state.

## 5. Cross-cutting acceptance rules

Every accepted roadmap item MUST satisfy all of the following:

1. The audience retains its last complete valid frame; Reader chrome, search
   marks, recoloring, dialogs, and failures never appear there.
2. PDF parsing, extraction, forms, annotations, attachments, and saving remain
   in supervised workers. No feature moves PDFium into the application process.
3. A platform operation returns `Done`, `Refused`, `Unsupported`, or `Failed`;
   no new bare boolean or silent no-op is introduced.
4. Commands are semantic. Keyboard, menu, button, remote, automation, and
   accessibility actions dispatch the same model.
5. Persisted document locations use a fingerprint plus portable PDF concepts;
   they never store native handles, monitor indices, physical DPI, or lossy
   UTF-8 paths.
6. User data is bounded: histories, bookmark text, attachment names/sizes,
   search queries/results, outline depth, and IPC payloads have explicit limits.
7. External actions are deny-by-default. PDF JavaScript, URI launch, attachment
   execution, file access, email, upload, and submission never become implicit
   consequences of opening or interacting with a document.
8. Dirty documents are never silently reloaded, closed, printed from the wrong
   revision, or replaced. Source immutability and verified atomic Save As remain
   the default.
9. Status is not conveyed by color alone, and all new UI uses the existing
   seven roles, spacing/type scales, focus rules, and reduced-motion behavior.
10. Performance follows the visible/active set rather than document length or
    session history. Any cache, prefetch, or worker-queue change is measured in
    a release build before restructuring.

## 6. Decisions this comparison settles

- Pulpit should become a strong PDF reader where that strengthens presenting,
  review, annotation, and form workflows; it should not become a Zathura clone.
- The next product value is continuity—recovery, remembered locations,
  bookmarks, history, inspection—not another rendering backend.
- Zathura's keyboard discipline is worth retaining; its command language and
  unrestricted shell integration are not.
- Reader-only recoloring and rotation are worthwhile precisely because they
  are kept out of the audience path.
- SyncTeX is valuable author integration, but D-Bus is only one adapter and
  must not define the architecture.
- PDF outlines and Pulpit bookmarks are separate concepts: one is authored in
  the document, the other belongs to the user's relationship with it.
- PDFium remains the production engine. Cross-viewer verification provides
  interoperability without multiplying production semantics.

## 7. Zathura sources reviewed

- Project overview and headline features:
  <https://pwmt.org/projects/zathura/>
- Current user manual, commands, bindings, SyncTeX, and sandbox behavior:
  <https://man.archlinux.org/man/zathura.1.en>
- Current configuration reference:
  <https://man.archlinux.org/man/zathurarc.5>
- Official document-plugin list:
  <https://pwmt.org/projects/zathura/plugins/>
- Poppler PDF backend:
  <https://pwmt.org/projects/zathura-pdf-poppler/>
- MuPDF PDF backend:
  <https://pwmt.org/projects/zathura-pdf-mupdf/>
