#!/usr/bin/env bash
# Regenerate the platform icon formats from `packaging/pulpit.svg`.
#
#   packaging/pulpit.iconset/   the macOS sizes, which `iconutil` turns into
#                               the `.icns` at bundle time
#   packaging/pulpit.ico        the Windows icon, embedded in the executable
#                               and used by the installer
#
# Both are checked in so that building a package needs no SVG rasterizer on
# the build machine — a Windows or macOS runner has neither resvg nor
# ImageMagick. The SVG remains the source of truth; run this after changing
# it, on a machine with the dev shell.
#
# This is deliberately not a dependency of any build target: a release must
# not be able to fail because a rasterizer is missing.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
svg="$root/packaging/pulpit.svg"
iconset="$root/packaging/pulpit.iconset"
ico="$root/packaging/pulpit.ico"

for tool in resvg icotool; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "$tool is not installed; enter the dev shell (nix develop)" >&2
    exit 1
  fi
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The artwork is square, so only the width is given: passing both dimensions
# makes resvg refuse the arguments.
render() { resvg --width "$1" "$svg" "$2"; }

# --- macOS -----------------------------------------------------------------
# The names are Apple's and `iconutil` rejects anything else.
rm -rf "$iconset"
mkdir -p "$iconset"
while read -r size name; do
  [ -z "$size" ] && continue
  render "$size" "$iconset/$name.png"
done <<'SIZES'
16 icon_16x16
32 icon_16x16@2x
32 icon_32x32
64 icon_32x32@2x
128 icon_128x128
256 icon_128x128@2x
256 icon_256x256
512 icon_256x256@2x
512 icon_512x512
1024 icon_512x512@2x
SIZES

# --- Windows ---------------------------------------------------------------
# The 256px frame is stored as PNG (`--raw`) rather than a bitmap, which is
# what Vista and later expect and what keeps the file at ~100K instead of
# ~370K. The smaller frames stay as bitmaps for older shells.
for size in 16 24 32 48 64 128 256; do
  render "$size" "$work/$size.png"
done
icotool --create --output "$ico" --raw="$work/256.png" \
  "$work/16.png" "$work/24.png" "$work/32.png" \
  "$work/48.png" "$work/64.png" "$work/128.png"

echo "iconset: $iconset"
echo "ico:     $ico ($(wc -c < "$ico") bytes)"
