#!/usr/bin/env bash
# Build the `.deb`, the `.rpm` and the Arch `.pkg.tar.zst` from one
# description (packaging/linux/nfpm.yaml, SPEC-package.md §5.1).
#
# This is the primary Linux deliverable. The tarball beside it solves PDFium
# portability only; a native package is the one artifact that can *state* the
# dlopen set as dependencies and a Chromium-family browser as a recommendation,
# which is what makes an installed pulpit start with nothing else to do.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)"
target="${1:-$root/target/release/pulpit}"
staging="$root/dist/linux-staging"

case "$(uname -m)" in
  x86_64)  arch=amd64 ;;
  aarch64) arch=arm64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

if ! command -v nfpm >/dev/null 2>&1; then
  cat >&2 <<'MISSING'
nfpm is not installed.

Both packages are generated from one description rather than maintained
twice, and nfpm is what does that. Install a pinned release from
https://github.com/goreleaser/nfpm/releases, or `nix develop` and use the
dev shell, which carries it.
MISSING
  exit 1
fi
if [ ! -x "$target" ]; then
  echo "no release binary at $target — run 'cargo build --release' first" >&2
  exit 1
fi
if [ ! -f "$root/lib/libpdfium.so" ]; then
  echo "no lib/libpdfium.so — run ./scripts/fetch-pdfium.sh first" >&2
  exit 1
fi

# §1: the executable MUST NOT link a media engine, and that is verified from
# the final binary's dynamic dependencies rather than by reading the source.
# Media is driven by an installed browser in a separate process; a linked
# engine here would mean a gigabyte of dependency and a second way to render.
if command -v ldd >/dev/null 2>&1; then
  if ldd "$target" | grep -Eiq 'gstreamer|libwebkit|libcef|libavcodec'; then
    echo "refusing to package: the binary links a media engine" >&2
    ldd "$target" | grep -Ei 'gstreamer|libwebkit|libcef|libavcodec' >&2
    exit 1
  fi
else
  echo "note: no ldd available; skipped the media-engine check" >&2
fi

rm -rf "$staging"
mkdir -p "$staging" "$root/dist"
install -m755 "$target" "$staging/pulpit"
# Strip the *copy*, for the same reason make-bundle.sh does: `--strip-debug`
# drops the DWARF line tables and keeps the symbol table, so a worker crash in
# an installed package still names functions in its backtrace.
if command -v strip >/dev/null 2>&1; then
  strip --strip-debug "$staging/pulpit"
else
  echo "note: no strip available; the packages keep their debug information" >&2
fi

export PULPIT_VERSION="$version" PULPIT_ARCH="$arch"
for packager in deb rpm archlinux; do
  nfpm package --packager "$packager" \
    --config packaging/linux/nfpm.yaml --target dist/
done
rm -rf "$staging"

for artifact in dist/pulpit*.deb dist/pulpit*.rpm dist/pulpit*.pkg.tar.zst; do
  [ -e "$artifact" ] || continue
  echo "package: $artifact"
done
