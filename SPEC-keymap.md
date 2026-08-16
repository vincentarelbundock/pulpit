# pulpit keymap: Acrobat and Zathura comparison and proposal

Status: product analysis and a keymap proposal, not an implementation
specification. Companion to `SPEC-zathura-roadmap.md`, which covers features;
this document covers only key bindings, and only for the two readers pulpit's
users are most likely to already have muscle memory for — Adobe Acrobat
Reader and Zathura.

**MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT** and **MAY** are normative,
consistent with `SPEC-document.md`.

## 1. Goal

A presenter or reader coming from Acrobat or Zathura should find the keys
they already reach for do approximately the right thing in pulpit, without
pulpit inheriting either application's conflicts, chord-heaviness, or command
language. Where the two disagree, pulpit picks one behavior and does not try
to satisfy both bindings for the same physical key.

This is a *subset*: Acrobat and Zathura each have far more shortcuts than are
listed here (annotation-tool cycling in Acrobat, quickmarks and jump-history
bisection in Zathura, etc.). Only shortcuts that are plausible defaults for
pulpit — because pulpit has or is roadmapped to have the underlying feature —
are included. The full lists live in the research notes; see §6.

## 2. Reference: Acrobat and Zathura shortcuts, subset to what's relevant

| Action | Acrobat Reader DC | Zathura |
|---|---|---|
| Next page | Right, Page Down, Enter | `j`, Page Down |
| Previous page | Left, Page Up, Shift+Enter | `k`, Page Up |
| First page | Home | `gg` |
| Last page | End | `G` |
| Scroll (line-level) | Up/Down arrows | `h j k l`, arrows |
| Half/full-page scroll | — | `^d`/`^u`, `^f`/`^b` |
| Zoom in | Ctrl+= | `+`, `zI` |
| Zoom out | Ctrl+- | `-`, `zO` |
| Actual size / reset zoom | Ctrl+1 | `=`, `z0` |
| Fit page | Ctrl+0 | `a` (best-fit) |
| Fit width | Ctrl+2 | `s` (best-width) |
| Rotate (view only) | Ctrl+Shift+= / Ctrl+Shift+- | `r` |
| Toggle two-page/dual view | — | `d` |
| Toggle sidebar/navigation pane | F4 | Tab (outline mode) |
| Fullscreen / presentation | Ctrl+L | F5 (presentation), F11 (fullscreen) |
| Exit fullscreen/presentation | Esc | Esc, `q` |
| Find | Ctrl+F | `/` |
| Find backward | — | `?` |
| Find next / previous | F3 / Shift+F3 | `n` / `N` |
| Open | Ctrl+O | `o`, `O` |
| Reload | — (auto) | `R` |
| Quit | Ctrl+W (close doc) | `q` |
| Recolor / dark mode | — | `^r` |
| Set / jump to mark | Ctrl+B (bookmark) | `mX` / `'X` |
| Follow link | click | `f` (hint mode) |
| Undo / Redo | Ctrl+Z / Ctrl+Shift+Z | — |

Full lists (Acrobat's forms, printing, selection shortcuts; Zathura's jump
history, quickmarks, index navigation, restricted fullscreen/presentation key
sets) are in the background research this document is drawn from and are not
reproduced here because pulpit has no present or roadmapped equivalent for
most of them (Zathura's command-line `:` mode, in particular — see
`SPEC-zathura-roadmap.md` §3.11, "do not add").

## 3. Where the two conflict

Only a few keys matter to both audiences and disagree:

- **`f`.** Zathura uses it for link hints; Acrobat has no default binding for
  it. pulpit already binds bare `f` to `ToggleAudienceFullscreen` — a
  presenter action with no Zathura or Acrobat analogue, and one that
  predates this document. Kept as-is; link-hint mode (still roadmapped, see
  `SPEC-zathura-roadmap.md` §3.4) gets a different key when it ships (`F`,
  matching pulpit's existing `FocusNextLink`/`FocusPreviousLink` naming
  once one is chosen — not `f`, since `ToggleAudienceFullscreen` already
  owns it).
- **`r`.** Zathura rotates the page; pulpit already binds bare `r` to
  `ToggleReader`, its own product's central mode switch, which has no
  equivalent in either reference application and outranks both. Reader-only
  rotation (`SPEC-zathura-roadmap.md` §3.3, P1) SHOULD bind to `Shift+R` — a
  small departure from Zathura, made because `r` is already taken by a
  higher-priority pulpit action, not by oversight.
- **`d`.** Zathura's dual-page toggle. Free in pulpit today. Proposed for the
  same purpose once two-page Reader layout ships (§4).
- **`o`/`O`.** Zathura opens with bare `o`; pulpit already uses bare `o` for
  `ShowOverview` and binds `Ctrl+O` to `OpenDocument`, matching Acrobat.
  Kept as-is — `ShowOverview` is presentation-specific and has no Zathura
  equivalent, while `Ctrl+O` already satisfies the Acrobat convention that
  also happens to be the universal one.
- **`q`.** Zathura quits on bare `q`. pulpit already requires `Ctrl+Q`,
  matching Acrobat/every other desktop app, specifically so a mistyped `q`
  mid-presentation cannot end a talk. Kept as-is; bare `q` MUST NOT be
  bound to quit in pulpit regardless of Zathura precedent.

Where there is no conflict, pulpit already tracks one or both conventions
(see `crates/pulpit/src/settings/keys.rs`): `j`/`l`/`k`/`h` navigation,
`Space`/arrows/`PageUp`/`PageDown` navigation, `Ctrl+F` and `/` for search,
`F3`/`Shift+F3` for find next/previous, `Escape` to cancel, `Ctrl+B` for the
outline rail (Acrobat's bookmark key, repurposed since pulpit's outline is
the nearer analogue to Acrobat's navigation pane), `Ctrl+O` to open,
`Ctrl+Z`/`Ctrl+Shift+Z` and `u`/`Ctrl+Y` for undo/redo.

## 4. Proposal: new bindings for gaps

pulpit's `Action` enum (`crates/pulpit/src/settings/keys.rs`) has no zoom,
fit-mode, or rotation actions yet — Presenter doesn't need them (slides fit
the frame by definition) and Reader's zoom is presently pointer/gesture
driven. As Reader zoom, fit modes, and rotation are specified
(`SPEC-zathura-roadmap.md` §3.3, P1), the following defaults are proposed,
chosen to match whichever of Acrobat/Zathura is unambiguous and free in
pulpit's existing table:

| New action (Reader-scoped) | Proposed key | Source | Notes |
|---|---|---|---|
| `ZoomIn` | `+` / `=` | Zathura (`+`) and Acrobat (`Ctrl+=`, base key without Ctrl) | Ctrl is already load-bearing elsewhere in pulpit's scheme (a modifier is never incidental, per `keys.rs`'s own `Mods` doc comment) — Reader zoom is not a global accelerator, so the bare key is correct and matches Zathura exactly. |
| `ZoomOut` | `-` | Zathura and Acrobat agree (mod stripped) | |
| `ZoomReset` | `0` | Acrobat's Ctrl+1/Ctrl+0 family, mod stripped; Zathura's `=`/`z0` conflicts with `ZoomIn`'s proposed key | `0` is free; matches Acrobat's "reset to a fixed state" mnemonic more than Zathura's overloaded `=`. |
| `FitPage` | `Shift+0` (or menu-only) | Acrobat `Ctrl+0` | Bare `0` already taken by `ZoomReset`; a Reader-only fit toggle is a reasonable secondary key, not a lectern-critical one. |
| `FitWidth` | — (menu/settings only initially) | Acrobat `Ctrl+2`, Zathura `s` | No bare key proposed: `s` is pulpit's `SwapDisplays`, a presenter action that must not move. Ship as a button/menu item before spending a key. |
| `RotateReader` | `Shift+R` | Zathura `r`, shifted per §3 | |
| `ToggleDualPage` | `d` | Zathura `d` | Free today; Reader-only. |
| `ToggleRecolor` | — (settings toggle, not a key) | Zathura `Ctrl+R` | `Ctrl+R` is pulpit's `ReloadDocument`, a much higher-frequency and higher-stakes action (`F5` is its only other binding); do not contest it for a comfort preference. |
| Find backward | `?` | Zathura | Free; pulpit currently has no backward-search binding distinct from `Shift+F3`. Optional — `Shift+F3` already covers it. |

None of these are presenter-scoped: Presenter's page *is* the frame, at a
size the projector and layout decide, so zoom/fit/rotation binding proposals
apply to Reader mode only, consistent with `SPEC-zathura-roadmap.md` §3.7's
rule that reading-comfort transforms are Reader-window-only and never reach
the audience.

## 5. What is deliberately not adopted

- Zathura's `:` command line and its whole command-language surface —
  already ruled out in `SPEC-zathura-roadmap.md` §3.11.
- Zathura's letter/number quickmarks (`mX`/`'X`) as *keyboard* bindings —
  the feature is roadmapped (`SPEC-zathura-roadmap.md` §3.5, P1) but as a
  named-slot UI, not a bare-letter mark/jump pair, because pulpit's bare
  letters are a scarce, already-allocated resource (annotation tools sit on
  `1`–`4`, not letters, for exactly this reason).
- Zathura's `gg`/`gN` multi-key sequences — pulpit's resolver has no
  key-sequence buffer (`keys.rs` says so explicitly, at `Last`'s binding);
  `Home` stands in for `gg` today, and this proposal does not ask for one.
- Acrobat's Ctrl+number zoom-level shortcuts beyond reset (Ctrl+1) — an
  unbounded chord family that does not match pulpit's small, named-action
  style.
- Any binding that would take `q`, `s`, `f`, `r`, `o`, `b`, `w`, or `t` away
  from their existing pulpit actions. These are pulpit's own reflexive
  lectern keys (blank, swap, fullscreen, reader, overview, outline, timer)
  and outrank both reference applications' conventions for the same letters.

## 6. Sources

- Adobe: <https://helpx.adobe.com/acrobat/desktop/get-started/preferences-and-settings/keyboard-shortcuts.html>
- Zathura: <https://pwmt.org/projects/zathura/documentation/>, <https://wiki.archlinux.org/title/Zathura>
- pulpit's current keymap: `crates/pulpit/src/settings/keys.rs`
- Feature-level Zathura comparison: `SPEC-zathura-roadmap.md`
