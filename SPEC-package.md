# pulpit Packaging and Distribution Specification

What pulpit ships as, what it requires on the machine, and what it must never
do to obtain a dependency.

Sources of truth in the tree: `flake.nix`, `Makefile`, `scripts/`, `packaging/`.
**MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT** and **MAY** are normative.

## 1. The artifact is one executable

- pulpit ships as a **single binary**. The renderer worker and the media worker
  are *roles of it*, re-executed with a flag. New workers MUST follow this
  pattern rather than adding installed helper executables.
- The executable MUST NOT link a media engine. Packaging MUST verify this from
  the final binary's dynamic dependencies, not from source inspection.

A Linux package installs:

| Path | Content |
|---|---|
| `bin/pulpit` | the executable |
| `share/applications/pulpit.desktop` | desktop entry |
| `share/icons/hicolor/scalable/apps/pulpit.svg` | icon |
| `share/doc/pulpit/` | `README.md`, licence notices |
| `lib/pulpit/libpdfium.so` | required (§2) |

Windows and macOS equivalents are in §6 and §7.

## 2. Run-time dependencies

### 2.1 The `dlopen` set

winit, wgpu and PDFium resolve libraries **at run time**, so they MUST be on
the loader path of the *running* binary — having them at build time is not
enough. This is why an unwrapped `cargo install` binary works on a distribution
with a global `/usr/lib` and fails on NixOS.

- The set: `libxkbcommon`, a Vulkan loader, `wayland`, `libX11`, `libXcursor`,
  `libXrandr`, `libXi`, `libGL`. `dbus` is additionally needed at build time.
- A package MUST make these discoverable, either by depending on system
  packages or by wrapping the binary with an explicit path.
- A missing library MUST be reported as *which one and how to install it*,
  never a loader crash.

### 2.2 PDFium is a hard requirement

- Every supported package installs it. A renderer worker that cannot bind it
  MUST print where it looked and exit — never substitute placeholder pages,
  which would put something on the projector that is not the presenter's deck.
- The fixture backend is reachable only when a test asks for it by name, via
  `PULPIT_FORCE_FIXTURE_BACKEND`.
- Search order: `PULPIT_PDFIUM_PATH`, the directory beside the executable, the
  installed `<prefix>/lib/pulpit` derived from it, `./lib`, then the system
  loader path. The derived step is what lets a `.deb` or `.rpm` install to
  §1's layout and start with no wrapper and no environment variable.

### 2.3 A browser is a *recommended* dependency

Every media overlay — animated image, video, interactive HTML — is rendered by
an installed Chromium-family browser.

- A package SHOULD declare one as **recommended, not required**: without it
  overlays fall back to their posters and the deck still presents.
- Recommended is not optional in practice. A format that cannot satisfy the
  recommendation on a normal install has shipped a diminished product.
- pulpit MUST NOT bundle, download or manage a browser engine.

**How it is driven constrains which formats can host pulpit.** The browser is
launched `--headless=new` with `--remote-debugging-pipe`, a private
`--user-data-dir`, and assets served from an allowlisted loopback origin. Control
travels over the child's **own file descriptors**, so the browser MUST be a
direct child with its fds intact. It needs no display, window or portal — but
no amount of display integration substitutes for spawning it. *A packaging
format that interposes on process creation cannot host pulpit.*

## 3. PDFium supply chain

A **pinned** prebuilt copy is fetched from `bblanchon/pdfium-binaries` and
verified against a recorded SHA-256 — by `scripts/fetch-pdfium.sh` outside Nix,
by an equivalent pinned `fetchurl` inside `flake.nix`. PDFium is never vendored
into the repository.

- Both MUST stay on the same pinned release.
- Every selectable target — Linux x64/arm64, macOS arm64/x64, Windows
  x64/arm64 — MUST carry a recorded hash, so no supported target fails closed.
- The release and hash MUST be updated deliberately. Resolving "latest" at
  build time is forbidden.
- An artifact with no recorded hash MUST fail the fetch, printing the observed
  hash for pinning, never proceed unverified.
- This is an unaffiliated third-party dependency. Producing internal builds is
  the documented escape hatch if it disappears.

## 4. Nix is the supported installation

```sh
nix run . -- deck.pdf     # or: nix profile add .   (`install` before Nix 2.30)
nix develop               # dev shell
```

The flake is the reference for what a correct package does: it builds from
`Cargo.lock`, installs the desktop entry and icon, and wraps `bin/pulpit` with
both `LD_LIBRARY_PATH` for the `dlopen` set and a default `PULPIT_PDFIUM_PATH`.
The result starts with no environment setup at all.

The dev shell MUST export the same paths the wrapper bakes in, so a
`cargo`-built binary behaves like the packaged one.

## 5. Linux

### 5.1 `.deb` and `.rpm` are the target

They are the only formats that can *state* this application's dependency
structure:

```
Depends:    libxkbcommon0, libvulkan1, libxcursor1, …   the dlopen set (§2.1)
Recommends: chromium | google-chrome-stable             the browser (§2.3)
```

That single fact resolves every open problem at once: the graphics stack lands
on the default loader path so no wrapper is needed; the browser arrives as a
normal executable pulpit can spawn as a direct child; and the desktop entry,
icon and uninstall are what the format already does.

- Both SHOULD be generated from one description in CI rather than maintained
  twice. They are: `packaging/linux/nfpm.yaml`, built by
  `scripts/make-linux-packages.sh` (`make linux-packages`), with per-packager
  dependency names as the only overrides.
- Both MUST install the *pinned* `libpdfium` into `lib/pulpit/`. A
  distribution's own PDFium is not the build pulpit is tested against.

### 5.2 Arch: the same description, plus a PKGBUILD

The same description also emits an Arch `.pkg.tar.zst`, which `pacman -U`
installs with the dlopen set as dependencies. It is **not** the whole answer,
for one specific reason: the generator emits no `optdepend` field, so that
package cannot state §2.3's browser recommendation — the exact defect that
disqualifies the formats in §5.3.

- `packaging/linux/PKGBUILD.in` therefore carries the recommendation as a real
  `optdepends`, and the AUR is where Arch users look in any case. It
  repackages the published tarball rather than building from source, so the
  binary and the pinned PDFium are the ones every other platform ships.
- The release renders it against the published tarball's hash and attaches it
  as an asset. Pushing to the AUR needs an account and an SSH key that CI does
  not have, which leaves it where winget and Scoop are: written, not submitted.
- If the generator gains `optdepend`, the downloaded package becomes complete
  on its own and the PKGBUILD becomes a convenience rather than the fix.

### 5.3 Flatpak, Snap and AppImage are rejected

A decision, not a deferral:

- **Sandboxes interpose on process creation.** Reaching a host browser through
  a portal means forwarding the control pipe across that boundary; bundling one
  is forbidden by §2.3 and costs a gigabyte. The sandbox protects nothing the
  private profile and loopback allowlist do not already protect.
- **A self-contained image cannot carry the part that breaks.** Mesa, the
  Vulkan loader and driver ICDs are bound to the host kernel and GPU; shipping
  our own produces mismatches worse than the missing-library error it set out
  to prevent.
- Neither can express a *recommended* dependency, so neither can promise a
  working HTML overlay on a default install.

### 5.4 Building from source

```sh
./scripts/fetch-pdfium.sh
cargo run --release -- deck.pdf
```

`make install` is a best-effort helper, **not** a tested surface:

- It deliberately does not depend on `build`: rebuilding under `sudo` leaves a
  root-owned `target/`. Build as yourself, install as root.
- It honours `PREFIX` and `DESTDIR`.
- On NixOS it MUST refuse by default (`FORCE_NIXOS_INSTALL=1` overrides),
  because the binary it would install cannot start.
- `make uninstall` MUST remove exactly what it wrote.

`make bundle` produces a relocatable directory and tarball carrying the binary,
`libpdfium` and a launcher. It solves *PDFium* portability only — the graphics
stack still comes from the host. The bundled copy is stripped with
`--strip-debug`, which removes ~86% of the binary while keeping the symbol
table, so a worker crash still names functions in its backtrace.

## 6. Windows

### 6.1 Package managers are the channel

Distributed through **winget and Scoop**, not a signed installer from a web
page, because signing is the only expensive part and package managers make it
optional rather than blocking:

- **Scoop** needs no installer and no signature; its `shortcuts` field gives
  the Start Menu entry §8 requires.
- **winget** needs a checksum, not a signature, and does not take the
  browser-download path that produces the loudest SmartScreen warning.

An installer is still built, for people who use neither. It MUST be
**per-user** by default, so an ordinary install raises no UAC prompt.

**Signing MUST NOT become a prerequisite.** No release may be gated on
possessing a certificate. The release workflow carries a signing step
conditional on a `SIGNPATH_API_TOKEN` secret: with it, the installer is signed
and checksums are computed from the *signed* bytes; without it the same tag
publishes the same unsigned artifacts. A fork, which cannot see secrets, must
stay green — that is the test of whether signing has quietly become required.

### 6.2 What Windows makes easy

- The §2.1 `dlopen` problem does not exist: the graphics stack ships with the
  OS, and `pdfium.dll` beside `pulpit.exe` is found with no wrapper and no
  environment variable.
- The browser is *better* here than anywhere else: Edge is preinstalled on
  every supported version and is Chromium-family, so §2.3's recommendation is
  satisfied on every machine with nothing to install — a stronger position than
  §5.1 can offer. Discovery looks beneath `%ProgramFiles%`,
  `%ProgramFiles(x86)%` and `%LOCALAPPDATA%`, tries `.exe`, and knows the name
  `msedge`. Consulting the `App Paths` registry key would additionally find a
  browser installed somewhere unusual and remains worth doing.

## 7. macOS

### 7.1 Homebrew Cask over an ad-hoc signed `.app`

Shipped as an `.app` bundle in a **Homebrew Cask**, carrying
`libpdfium.dylib` inside it. The `.app` is what puts pulpit in Launchpad and
Applications, which §8 requires and a bare binary does not provide.

- The bundle MUST be **ad-hoc signed** (`codesign -s -`). This is not
  notarization and buys no Gatekeeper relief; it is required because Apple
  Silicon refuses to execute a binary carrying no signature at all. It is free
  and needs no Apple account.
- Notarization requires a paid Developer ID and MUST NOT gate a release.
- `libpdfium.dylib` goes in `Contents/MacOS/` beside the executable, where the
  §2.2 search order already looks — no launcher, no environment variable. No
  `install_name_tool` fixup is needed either, because the library is opened by
  explicit path with `dlopen`; dyld is never asked to find it.

### 7.2 Quarantine, not signing, is what is being managed

The Gatekeeper prompt comes from the `com.apple.quarantine` attribute, which
the **downloading program** applies. Browsers set it; command-line tools do
not. This is the same structural fact that makes package managers the answer on
Windows: what matters is the path the artifact travels, not what is inside it.

- Without notarization, a first launch costs one trip through System Settings →
  Privacy & Security → *Open Anyway*. macOS 15 removed the Control-click
  shortcut that used to make this quicker.
- Homebrew re-applies quarantine to cask installs deliberately, so
  `--no-quarantine` is the documented way to skip it, not a default we assume.
- A plain formula would install with no prompt — and no Launchpad entry, no
  icon, no double-click launch. pulpit takes the `.app` and the prompt, because
  application identity is part of the definition of supported and a one-time
  dialog is not.

## 8. Packaging in the definition of supported

A desktop platform is not **supported** until it is packaged with an icon,
application identity, required native libraries, and platform-standard
install/uninstall behaviour. Compiling is *experimental*, not supported. A
packaging problem MUST be diagnosable from the diagnostics report a user sends.

## 9. Deliberately not done

- Vendoring a browser engine, a graphics stack, or PDFium sources.
- Flatpak, Snap and AppImage (§5.3).
- Resolving any dependency version at build time rather than from a pin.
- Installing helper executables beside the binary; workers are roles of it.

## 10. Status

| | Built | Verified in CI | Verified by a human |
|---|---|---|---|
| Nix | yes | yes | yes |
| Linux tarball | yes | yes | no |
| Linux `.deb`/`.rpm` | yes | yes — the `.deb` is installed and run | **no** |
| Arch `.pkg.tar.zst` | yes | built only, never installed | **no** |
| AUR `PKGBUILD` | rendered | **no** — never submitted | no |
| macOS `.app` + `.dmg` | yes | yes | **no** |
| Homebrew cask | yes | **no** — skipped for prereleases | no |
| Windows zip + installer | yes | yes | **no** |
| winget / Scoop manifests | written | **no** — never submitted | no |

- The tag-only half of releasing ran as `v0.0.1-rc1` and `-rc2`. Eight assets
  publish; the prerelease flag, the crates.io exclusion and the cask skip all
  behave. The **cask push itself** and the tag component of its URL remain
  unproven, because both are deliberately skipped for a prerelease.
- CI on macOS and Windows builds, signs and launches `--version`. That proves
  the bundle layout and, on macOS, that the signature is acceptable to the
  kernel. It proves **nothing about placement, reconciliation or the swap**,
  which need two real displays.
- macOS ships one universal bundle — arm64 and x86_64 slices in both the
  binary and the bundled libpdfium — and CI asserts both slices exist. The
  Intel half is nevertheless **never executed**: no runner in either workflow
  is an Intel Mac, so what is proven is that the slice is there, not that it
  runs. Windows x64 only.

## 11. TODO — getting this to users

Ordered. Each step is a thing only the maintainer can do.

### Before any release

1. **Enable GitHub Pages** — Settings → Pages → Source: *GitHub Actions*. The
   Pages workflow fails on every `docs/` push until this is set.
2. **Confirm the bundle identifier** `com.arelbundock.pulpit` in
   `scripts/make-app-bundle.sh`. It is baked into the `.app` and must never
   change afterwards: an upgrade with a different identifier installs *beside*
   the old app instead of replacing it.
3. **Choose the version.** `main` currently sits at `0.0.1` from the
   rehearsal. `make bump VERSION=x.y.z`, commit, then `make release`.

### Cut the first release

4. `make release` tags `vx.y.z` and fires both workflows. This is the first
   time the **cask push** runs — check that
   `vincentarelbundock/homebrew-tap` gains `Casks/pulpit.rb` with the right
   version and SHA-256, then verify `brew install --cask
   vincentarelbundock/tap/pulpit` on a Mac.
5. **Verify on real hardware.** The macOS and Windows display adapters have
   never run on a machine with two displays. Until someone presents from one,
   both platforms are *experimental* by §8's own definition, whatever CI says.

### Windows distribution

6. **Scoop**: create a bucket repository, or submit
   `packaging/windows/scoop-pulpit.json` to `ScoopInstaller/Extras`. Fill in
   the release URL and SHA-256.
7. **winget**: open a PR against `microsoft/winget-pkgs` with the three files
   in `packaging/windows/winget/`, filling `InstallerSha256` from the published
   `.sha256` and bumping `PackageVersion` in all three together.
8. *(Optional)* **Signing**: apply to SignPath Foundation, free for open
   source. The workflow expects project slug `pulpit`, policy slug
   `release-signing`, a `SIGNPATH_API_TOKEN` secret and a
   `SIGNPATH_ORGANIZATION_ID` variable. Nothing is blocked on this.

### Linux distribution

9. **Submit `pulpit-bin` to the AUR.** The rendered `PKGBUILD` ships as a
   release asset; pushing it needs an AUR account and an SSH key. Until then
   Arch users install the `.pkg.tar.zst`, which cannot state the browser
   recommendation (§5.2).
10. **Install the `.rpm` on a real Fedora machine.** Both packages are built
    and published (§5.1), and the release workflow installs the `.deb`, runs
    the binary and checks the installed layout — but the `.rpm` is only
    *generated* on Debian, never installed anywhere. Its dependency names are
    the one part of the description that no CI step exercises.

### Later

11. **Multi-architecture**: Windows arm64 needs its own build; its pin already
    exists in `scripts/fetch-pdfium.sh`. macOS is done — `make app-universal`
    fetches both Apple PDFium artifacts and `lipo`s both Mach-Os — but the
    Intel slice wants one run on a real Intel Mac before it is believed.
12. **`App Paths`** registry lookup for browsers installed in unusual places
    (§6.2).
13. Make an installed PDFium unconditional on the non-Nix paths: `make install`
    and `make bundle` still leave the packager to run `make pdfium` first.
