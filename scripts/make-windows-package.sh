#!/usr/bin/env bash
# Build the Windows artifacts: a portable directory and its zip, and — when
# Inno Setup is available — the per-user installer.
#
# Runs under Git Bash on Windows, which is what the CI runner provides.
#
# Two artifacts because there are two channels (SPEC-package.md §6.1):
#
#   the zip        Scoop installs from it. No installer and no signature is
#                  needed at all; the manifest's `shortcuts` field makes the
#                  Start Menu entry.
#   the installer  winget runs it, and it is what writes the Start Menu
#                  shortcut and the Add/Remove Programs entry for people who
#                  do not use a package manager. Per-user, so no UAC prompt.
#
# Neither is signed here. Signing is a *quality* improvement and must not
# become a prerequisite: no release may be gated on possessing a certificate.
# When a certificate exists, the signing step runs in CI between this script
# and the release upload, and nothing else about distribution changes.
#
# Windows needs no equivalent of the Linux bundle's launcher: pdfium.dll sits
# beside pulpit.exe, where the executable-directory step of the PDFium search
# order finds it, and the graphics stack ships with the operating system.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(awk -F'"' '/^version/ {print $2; exit}' "$root/Cargo.toml")"
target="${1:-$root/target/release/pulpit.exe}"
out="$root/dist/pulpit-windows"

if [ ! -f "$target" ]; then
  echo "no release binary at $target — run 'cargo build --release' first" >&2
  exit 1
fi
if [ ! -f "$root/lib/pdfium.dll" ]; then
  echo "no lib/pdfium.dll — run ./scripts/fetch-pdfium.sh first" >&2
  exit 1
fi

rm -rf "$out"
mkdir -p "$out/licenses"

cp "$target" "$out/pulpit.exe"
cp "$root/lib/pdfium.dll" "$out/pdfium.dll"
cp "$root/README.md" "$out/README.md"
for notice in README.md LICENSE-MIT LICENSE-APACHE ICED_AW-LICENSE LUCIDE-LICENSE; do
  cp "$root/LICENSES/$notice" "$out/licenses/$notice"
done
cp "$root/lib/PDFIUM-LICENSE" "$out/licenses/PDFIUM-LICENSE" 2>/dev/null || true

# The zip. PowerShell is always present; `zip` is not.
zip="$root/dist/pulpit-$version-windows-x64.zip"
rm -f "$zip"
powershell -NoProfile -Command \
  "Compress-Archive -Path '$(cygpath -w "$out")\\*' -DestinationPath '$(cygpath -w "$zip")'"

echo "portable: $out"
echo "zip:      $zip"

# The installer, if Inno Setup is on this machine. Its absence is not an
# error: the zip alone is a complete Scoop channel, and a contributor
# building locally should not need Inno Setup to produce something runnable.
iscc=""
for candidate in \
  "$(command -v iscc || true)" \
  "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" \
  "/c/Program Files/Inno Setup 6/ISCC.exe"; do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then iscc="$candidate"; break; fi
done

if [ -z "$iscc" ]; then
  echo "note: Inno Setup not found; skipping the installer" >&2
  exit 0
fi

# `MSYS2_ARG_CONV_EXCL` disables Git Bash's path mangling for this call.
# Without it the MSYS runtime sees an argument beginning with `/`, assumes it
# is a Unix path and rewrites it into a Windows one, so `/DAppVersion=…`
# arrives as a second filename and ISCC refuses with "You may not specify more
# than one script filename."
MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 "$iscc" \
  "/DAppVersion=$version" \
  "/DSourceDir=$(cygpath -w "$out")" \
  "$(cygpath -w "$root/packaging/windows/pulpit.iss")"

echo "installer: $root/dist/pulpit-$version-windows-x64-setup.exe"
