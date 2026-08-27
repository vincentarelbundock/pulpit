# CLAUDE.md

This file provides guidance to coding agents working in this repository.
`AGENTS.md` is a symlink to it, so there is one file and no drift.

## What this is

Pulpit is a PDF presenter in Rust, built on Iced 0.14. It opens a presenter
window on the operator's display and an audience window on the projector, and
its reason for existing is that connection, disconnection, mirroring,
swapping, suspend/resume and mixed DPI are treated as the main engineering
problem rather than as polish.

The workspace is five crates under `crates/`:

- `pulpit-core` — presentation state, notes mapping, timer, generations, and
  the decision half of speech (sentences, reading cursor, language). The
  domain modules are pure; `ipc` is not, and is the one exception — see the
  purity note under Conventions.
- `pulpit-display` — the display identity ladder, snapshots, roles and the
  single `reconcile()` function, plus X11/Wayland/Niri adapters.
- `pulpit-render` — PDF backends (PDFium, fixture), the render and document
  worker protocols, supervised worker processes, and the byte-bounded frame
  cache. The pipe plumbing underneath the protocols is `pulpit_core::ipc`.
- `pulpit-media` — the runtimes pulpit launches rather than links: media and
  interactive overlays, driven in a separate worker process by an installed
  Chromium-family browser over CDP or by an installed libmpv, whichever the
  probe selects; and `speech`, which drives an installed synthesiser and
  audio player as child processes. The two share no types, only that policy.
- `pulpit` — the Iced application and everything only it uses: the
  presenter layout tree and widgets, the document watcher, the platform
  boundary, settings and diagnostics.

`pulpit`, the renderer worker and the media worker are **roles of one binary**,
re-executed with a flag. `pulpit-topology` is the only other executable.

## Commands

The `Makefile` is the canonical entry point; `make help` lists every target.

- `make` / `make build` — release build; `make check` for a fast `cargo check`
- `make test` — `cargo test --workspace`; no display required
- Single test: `cargo test -p pulpit-display --test topology_script`
- `make lint` — `cargo fmt --check` plus clippy with warnings denied
- `make pdfium` — fetch the pinned, hash-verified PDFium into `./lib`
- `make website` / `make serve` — compile `docs-src/` into `docs/` with Calepin
- `make bump VERSION=x.y.z` then `make release` — tag and push, which fires the
  cargo-dist and crates.io workflows. `make release` refuses a dirty tree.
- `make launch [DECK=deck.pdf]` — development launcher inside the dev shell;
  no deck argument starts the app empty

PDFium tests skip with a message when no `libpdfium` is installed, so a green
run on a machine without it has skipped the meaningful rendering tests.
`PULPIT_FORCE_FIXTURE_BACKEND=1` selects the fixture backend explicitly.

## Architecture

`docs-src/internals.typ` is the authoritative account: the component map, the
three rules, the standing invariants (normative MUST/SHOULD language), the
platform boundary, the design system, and the display-control findings the
design rests on. Read it before changing anything below the application layer.

The three rules, in short:

1. **One reconciliation function.** `reconcile(snapshot, roles, capabilities,
   windows)` is pure and idempotent. Swap is a role exchange followed by
   ordinary reconciliation, never ad-hoc window moves.
2. **No native handle survives an event-loop turn.** Identities are records;
   handles are resolved immediately before a native call and forgotten.
3. **The audience frame is never worse than it was.** The last complete frame
   stays until a complete replacement exists; rendering happens in supervised
   child processes.

## Conventions

- The domain crates are pure: no UI types, no window handles, no PDF-library
  types and no clock reads (time is passed in). That is what keeps the hard
  cases — reconnect at a new index, an unequal mirror, a partial write, a
  stale delayed notification — ordinary unit tests that run in CI.
- `pulpit_core::ipc` is the one exception, and a deliberate one: it spawns
  processes, maps files and blocks on a clock. It is there because it is the
  only place `pulpit-render`, `pulpit-media` and `pulpit` can all reach —
  the two worker crates are siblings — and because four copies of it had
  already drifted into a shared-memory leak and an unenforced fork-bomb
  marker. **No module outside `ipc` may depend on `ipc`**, so the domain
  stays pure even though the crate no longer is.
- Ask what the session can do, never what OS it is. Everything above
  `pulpit::platform` reads `Capabilities`; `cfg!(target_os = ...)` in a
  view or a state transition is a bug.
- Views name meanings, not colours or numbers: the seven colour roles and the
  spacing/type scales in `crates/pulpit/src/theme/tokens.rs`.
- Every operation that leaves the process returns an explicit `Outcome`
  (`Done` / `Refused` / `Unsupported` / `Failed`). Never a bare `bool`.
- Scripted topology files in `crates/pulpit-display/tests/topology/` are the
  regression surface for display behaviour; capture a new one with
  `pulpit-topology` rather than writing a bespoke test.
- Measure before restructuring. Three negative results are recorded in
  `docs-src/internals.typ` and should not be re-litigated without numbers.
- `docs/` is generated output. Edit `docs-src/` and run `make website`.
- Licence texts live in `LICENSES/`, with `LICENSES/README.md` saying what
  covers which part of the package.

## Git workflow

- Before creating any git commit, always ask the user for approval and wait for their response.
- At the end of every turn that modifies files, offer to create a git commit for the current task.
- Before committing, run `git status --short` and review the diff.
- Stage only files changed for the current task. Do not stage unrelated user changes.
- Use a concise imperative commit message.
- If no files were modified, do not create a commit.
- If required checks fail, do not commit unless the failure is unrelated or explicitly accepted; explain the reason before ending the turn.

## Release notes and versioning

- Ask the user whether the current task should be added to the changelog.
- Version bumps go through `make bump VERSION=x.y.z`, which updates the workspace manifest and `Cargo.lock` together.
