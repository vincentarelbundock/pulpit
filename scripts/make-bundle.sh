#!/usr/bin/env bash
# Build a relocatable directory containing the binary, libpdfium and a
# launcher that finds them wherever the directory is unpacked.
#
# This solves *PDFium* portability. It does not vendor the graphics stack:
# libxkbcommon, the Vulkan loader and libXcursor come from the host, because
# bundling a graphics stack across distributions causes more breakage than it
# prevents. On NixOS, use the flake instead.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(awk -F'"' '/^version/ {print $2; exit}' "$root/Cargo.toml")"
target="${1:-$root/target/release/pulpit}"
out="$root/dist/pulpit-$version-$(uname -m)"

if [ ! -x "$target" ]; then
  echo "no release binary at $target — run 'cargo build --release' first" >&2
  exit 1
fi
if [ ! -f "$root/lib/libpdfium.so" ]; then
  echo "no lib/libpdfium.so — run ./scripts/fetch-pdfium.sh first" >&2
  exit 1
fi

rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/share"
install -m755 "$target" "$out/bin/pulpit"
# Strip the *copy*, never `$target`: the tree you debug against keeps its full
# symbols, and only the thing people download gets smaller. The release
# profile builds with debuginfo, which is most of the binary — the tarball
# went from ~74M to a fraction of that.
#
# `--strip-debug`, not `--strip-all`: it drops the DWARF line tables while
# keeping the symbol table, so a crash in a worker still names functions in
# its backtrace. A packaging problem has to stay diagnosable from the report a
# user sends, and addresses with no names are not that.
if command -v strip >/dev/null 2>&1; then
  strip --strip-debug "$out/bin/pulpit"
else
  echo "note: no strip available; the bundle keeps its debug information" >&2
fi
install -m644 "$root/lib/libpdfium.so" "$out/lib/"
install -m644 "$root/lib/PDFIUM-LICENSE" "$out/share/PDFIUM-LICENSE" 2>/dev/null || true
install -m644 "$root/README.md" "$out/share/README.md"
# A relocatable directory is what somebody hands to somebody else, so the
# notices for the work that is not ours go inside it.
mkdir -p "$out/share/licenses"
install -m644 "$root/LICENSES/README.md" "$out/share/licenses/README.md"
install -m644 "$root/LICENSES/LICENSE-MIT" "$out/share/licenses/LICENSE-MIT"
install -m644 "$root/LICENSES/LICENSE-APACHE" "$out/share/licenses/LICENSE-APACHE"
install -m644 "$root/LICENSES/ICED_AW-LICENSE" \
  "$out/share/licenses/ICED_AW-LICENSE"
install -m644 "$root/LICENSES/LUCIDE-LICENSE" \
  "$out/share/licenses/LUCIDE-LICENSE"
install -m644 "$root/packaging/pulpit.desktop" "$out/share/"
# The desktop entry says `Icon=pulpit`, so the icon has to travel with it —
# otherwise anything installing from this tarball (the AUR package among them)
# lands a menu entry with no icon.
install -m644 "$root/packaging/pulpit.svg" "$out/share/"

cat > "$out/pulpit" <<'LAUNCHER'
#!/usr/bin/env bash
# Launcher: makes the bundled libpdfium discoverable regardless of where this
# directory was unpacked.
here="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
export PULPIT_PDFIUM_PATH="${PULPIT_PDFIUM_PATH:-$here/lib}"
exec "$here/bin/pulpit" "$@"
LAUNCHER
chmod +x "$out/pulpit"

tar -czf "$out.tar.gz" -C "$(dirname "$out")" "$(basename "$out")"
echo "bundle: $out"
echo "archive: $out.tar.gz"
