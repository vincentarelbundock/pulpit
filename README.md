# pulpit

<img src="logo.svg" alt="pulpit" width="240">

A focused PDF presenter in Rust. The promise is narrow and deliberate:

> **pulpit is the PDF presenter that does not screw up your projector.**

It runs a presenter window on your display and an audience window on the
projector, and it treats connection, disconnection, mirroring, swapping,
suspend/resume and mixed DPI as the *main* engineering problem — not as
polish.

## Status

The presenter is complete and in use: two-window presentation, display
reconciliation, layouts, speaker notes, PDF links, presenter annotations,
session recovery, and media overlays for animated images and interactive HTML.

`SPEC-package.md` contains only the work that **remains**;
`docs-src/internals.typ` records the standing invariants. Four things are
deliberately left undone: the upstream Iced
contribution (without which portable targeted fullscreen on Wayland is impossible),
physical-hardware qualification, packaging beyond Nix, and screen-reader
support — which is blocked on Iced exposing an accessibility tree at all.

## Quick start

### Nix / NixOS — the supported installation

```sh
nix run . -- path/to/deck.pdf        # or: nix profile install .
nix develop                          # dev shell, then: cargo run -- deck.pdf
make launch                          # development launcher
make launch DECK=examples/mosaic.pdf # development launcher, Mosaic example
```

The packaged binary is wrapped with the loader path it needs and a pinned
PDFium, so it starts with no environment setup at all. This matters on NixOS
specifically: winit, wgpu and PDFium are all `dlopen`ed at run time, and a
plain `cargo install` binary finds nothing without a global `/usr/lib`.

### Other distributions

```sh
./scripts/fetch-pdfium.sh            # pinned, hash-verified PDFium into ./lib
cargo run --release -- path/to/deck.pdf
```

This needs the usual desktop libraries present (`libxcursor`, `libxkbcommon`,
a Vulkan loader). If they are missing, pulpit says which ones and how to
install them rather than crashing.

`.deb` and `.rpm` packages are the planned Linux install and do not exist yet.
They are the right answer because they can declare what pulpit actually
needs — the desktop libraries as dependencies, and a Chromium-family browser
as a *recommendation*, so media overlays work on a default install instead of
falling back to posters. Flatpak, Snap and AppImage are deliberately not
targets: a sandbox blocks the browser pulpit has to launch as a child
process, and a self-contained image cannot carry the graphics drivers, which
are the part that actually breaks. Until the native packages exist,
`make install` and `scripts/make-bundle.sh` are best-effort helpers and not a
tested surface.

`libpdfium` is required, not optional. Every package installs it; if it is
missing, pulpit tells you where it looked and exits, rather than showing
you placeholder pages where your slides should be.

### Keys

| Key | Action |
|---|---|
| `→` `↓` `Space` `PageDown` | next slide (audience follows) |
| `←` `↑` `PageUp` `Backspace` | previous slide |
| `Home` / `End` | first / last |
| `Tab` / `Shift+Tab` | move the **preview only** — the audience does not follow |
| `Enter` | show the previewed slide |
| `Esc` | cancel the preview |
| `b` / `w` | blank black / blank white |
| `p` / `r` | start-pause / reset the timer |
| `s` | swap presenter and audience displays |
| `f` | toggle audience fullscreen |
| `o` / `F5` | open / reload |
| `d` | diagnostics bundle |
| `q` | quit |

Presenter remotes usually emit `PageUp`/`PageDown`, media keys or browser
back/forward — all bound by default. A remote whose keys the toolkit cannot
name is still usable: press the key and the presenter window offers to bind
it, storing the raw scancode in `settings.toml`.

In the layout designer: `Ctrl/Cmd+Z` and `Ctrl/Cmd+Shift+Z` undo and redo,
`Ctrl/Cmd+S` saves, and with a divider focused the arrow keys move it 1% at a
time (5% with `Shift`).

Only the presenter window opens initially. **Start ▾** beside the hamburger is
a split dropdown button: **Start** uses the saved audience display, while the
arrow lists the connected displays so one click both selects a projector and
starts the audience. It also offers a five-second delayed start (switch to the
projector workspace during the count) and a windowed start for manual
placement. The matching **Stop** button removes the audience window entirely.

On Niri, pulpit uses compositor IPC, so choosing the projector sends the
audience window to that output's active workspace. Other Wayland compositors
still explain when they require manual placement.

## Layouts

The presenter screen is a **layout**: a tree of splits and cells with a widget
in each cell, scaled proportionally to the window. Four built-in layouts ship
with the application, and the designer builds custom ones — split cells, drop
widgets, drag dividers, and preview the result with realistic sample content.

```sh
pulpit --layouts                          # the layout library
pulpit --edit-layout slide-next-notes     # straight into the designer
```

Built-ins are read only; **Duplicate to Customize** makes an editable copy.
Custom layouts are JSON files that can be exported and imported. See
`docs-src/usage.typ`.

## Architecture

```
crates/
  pulpit-core       PresentationState, notes mapping, timer, generations   (pure)
  pulpit-display    identity ladder, snapshots, roles, reconcile()         (pure + adapters)
  pulpit-render     PDF backends, IPC protocol, worker processes, cache
  pulpit-app        the Iced application, and everything only it uses:
                          layout/     the presenter layout tree and widgets   (pure)
                          doc/        debounced watcher, failure-safe reload  (pure + notify)
                          platform/   the desktop boundary behind contracts
                          settings/   atomic settings, keymap, diagnostics
```

Four packages, not nine: `core`, `display` and `render` are separate because
they cross a process or tool boundary, isolate a large external dependency, or
have a test surface worth running alone. The rest were app-only libraries
whose Cargo boundary bought nothing, and are now modules with the same rule —
no Iced, no clocks, no services below the application layer.

Three rules hold the design together:

1. **One reconciliation function.** Startup, hot-plug, resume, selection,
   fullscreen and swap all call `reconcile(snapshot, roles, capabilities,
   windows)`. It is pure, idempotent, and tested against every topology the
   specification lists — including reconnect at a different index, resolution
   and scale, which is the defect that motivated this project.
2. **No native handle outlives a call.** Monitors are re-enumerated for every
   reconciliation; a handle is resolved immediately before a native operation
   and forgotten afterwards. A display vanishing mid-operation is a normal
   race that converges, not a failure.
3. **The audience frame is sacred.** The last valid frame stays on screen
   until a complete replacement exists. Rendering happens in supervised child
   processes; a worker crash fails one request and restarts, and a rebuilt PDF
   is promoted only after its first audience frame renders.

## Platform support

| Platform | Enumeration & identity | Targeted fullscreen | Notes |
|---|---|---|---|
| X11 | XRandR + EDID | yes, via EWMH — verified, not assumed | reference platform |
| X11, WM that owns layout | XRandR + EDID | **no** — refused, with manual guidance | detected by observing that a placement did not hold, so unknown tiling WMs are covered |
| Wayland | `wl_output` + `xdg_output` | **no** — compositor placement, explained in the UI | needs the toplevel object, which Iced does not expose |
| Windows | `QueryDisplayConfig` device path | yes, borderless on the target monitor | **written, not yet run on hardware** |
| macOS | CoreGraphics vendor/model/serial | yes, via AppKit frame + fullscreen | **written, not yet run on hardware** |

Iced 0.14 exposes no monitor enumeration and no targeted fullscreen, exactly
the parity gap the specification identifies; `crates/pulpit-display`
implements it behind a trait, so an upstream contribution or a pinned patch
can replace an adapter without touching the application. See
`docs-src/internals.typ`.

## Testing

```sh
cargo test                                   # everything; no display required
cargo test -p pulpit-display --test topology_script
sudo ./scripts/vkms-topology.sh              # virtual connectors, privileged
```

- Reconciliation, presentation state, document reload, cache accounting,
  render generations and protocol decoding are pure tests.
- **Scripted topologies**: every file in
  `crates/pulpit-display/tests/topology/` is replayed through the real
  state machine under X11, Wayland and tiling capability profiles, asserting
  the invariants after each transition. `pulpit-topology` dumps a live
  topology in that same format, so a session with an awkward dock or projector
  becomes a permanent regression test by committing the capture.
- Supervisor tests spawn **real worker processes**, including a deliberately
  crashing and a deliberately hanging one.
- PDFium tests render a generated PDF and skip with a message when no
  `libpdfium` is installed.

## Documentation

The website is <https://vincentarelbundock.github.io/pulpit>. Its Typst
sources live in `docs-src/`; `docs/` holds the compiled HTML and PDF that
GitHub Pages serves, and is regenerated with `make website` — never edited by
hand.

- `docs-src/install.typ` — Nix and other distributions, PDFium provenance and
  pinning, and how the test suite is run.
- `docs-src/usage.typ` — keys, the presenter layout model and its designer,
  and the deterministic notes-mapping contract including the Typst/Mosaic
  metadata form.
- `docs-src/internals.typ` — how the pieces fit, the standing invariants, the
  platform boundary and design system, and the display-control findings: what
  each platform actually permits and what parity costs.

`SPEC-package.md` stays in the repository rather than on the website: it
records the rules a package must satisfy and the packaging work that remains.
Everything else outstanding is a GitHub issue.

## Licence

MIT or Apache-2.0, at your option. PDFium is BSD-3-Clause and is *not*
vendored here; see `docs-src/install.typ`.

Every licence text lives in `LICENSES/`, and `LICENSES/README.md` says which
one covers which part of the package — pulpit's own source, the vendored
`iced_aw` widgets, the Lucide icons, PDFium, and the Cargo dependency tree.
