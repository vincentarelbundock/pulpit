#!/usr/bin/env bash
# Build `Pulpit.app` — the macOS application bundle — and ad-hoc sign it.
#
# Three things make this simpler than the Linux bundle of `make-bundle.sh`:
#
#  1. `libpdfium.dylib` goes beside the executable in `Contents/MacOS`, where
#     the "directory next to the executable" step of the PDFium search order
#     (`pulpit-render/src/pdf/pdfium.rs`) already looks. No launcher script and
#     no `PULPIT_PDFIUM_PATH`. The workers are re-execs of the same binary, so
#     `current_exe()` resolves to the same directory for them too.
#  2. No `install_name_tool` fixup is needed, because the library is opened by
#     an explicit path with `dlopen`, not linked against. dyld is never asked
#     to find it.
#  3. The graphics stack is the operating system's, so there is nothing to
#     decide about vendoring one.
#
# The bundle is **ad-hoc signed** (`codesign -s -`). That is not notarization
# and buys no Gatekeeper relief; it is required because Apple Silicon refuses
# to execute a binary carrying no signature at all. See SPEC-package.md §7.
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
  echo "make-app-bundle.sh builds a macOS bundle and must run on macOS" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(awk -F'"' '/^version/ {print $2; exit}' "$root/Cargo.toml")"
out="$root/dist/Pulpit.app"

# `--universal` fuses the two per-architecture builds into one bundle that
# runs natively on both Apple Silicon and Intel, which is what `make
# app-universal` and the release workflow use. Without it the bundle carries
# whatever single binary it was handed, which is what a local `make app` on
# one machine wants.
#
# Every Mach-O in the bundle has to be fat, not just the executable: dyld
# never sees libpdfium — it is opened by explicit path with `dlopen` — so an
# arm64-only library inside a universal app fails at run time on Intel, and
# silently, because the app degrades to the fixture backend rather than
# crashing.
universal=false
if [ "${1:-}" = "--universal" ]; then
  universal=true
  shift
fi
target="${1:-$root/target/release/pulpit}"

# The inputs `--universal` needs, named once. `make app-universal` produces
# both halves; a missing one is an error here rather than a thin bundle
# published as a fat one.
arm_binary="$root/target/aarch64-apple-darwin/release/pulpit"
intel_binary="$root/target/x86_64-apple-darwin/release/pulpit"
arm_pdfium="$root/lib/libpdfium.dylib"
intel_pdfium="$root/lib/mac-x64/libpdfium.dylib"

if $universal; then
  for input in "$arm_binary" "$intel_binary" "$arm_pdfium" "$intel_pdfium"; do
    if [ ! -f "$input" ]; then
      echo "missing $input — run 'make app-universal', which builds both" \
        "architectures and fetches both PDFium artifacts" >&2
      exit 1
    fi
  done
else
  if [ ! -x "$target" ]; then
    echo "no release binary at $target — run 'cargo build --release' first" >&2
    exit 1
  fi
  if [ ! -f "$arm_pdfium" ]; then
    echo "no lib/libpdfium.dylib — run ./scripts/fetch-pdfium.sh first" >&2
    exit 1
  fi
fi

rm -rf "$out"
mkdir -p "$out/Contents/MacOS" "$out/Contents/Resources"

if $universal; then
  lipo -create "$arm_binary" "$intel_binary" \
    -output "$out/Contents/MacOS/pulpit"
  chmod 755 "$out/Contents/MacOS/pulpit"
  lipo -create "$arm_pdfium" "$intel_pdfium" \
    -output "$out/Contents/MacOS/libpdfium.dylib"
  chmod 644 "$out/Contents/MacOS/libpdfium.dylib"
else
  install -m755 "$target" "$out/Contents/MacOS/pulpit"
  install -m644 "$arm_pdfium" "$out/Contents/MacOS/libpdfium.dylib"
fi

# The icon. `iconutil` is part of the developer tools and needs no account;
# the `.iconset` is checked in so this step needs no SVG rasterizer on the
# build machine. Regenerate it from the SVG with `scripts/make-iconset.sh`.
iconutil --convert icns --output "$out/Contents/Resources/pulpit.icns" \
  "$root/packaging/pulpit.iconset"

# Notices for the work that is not ours travel inside the thing being handed
# to somebody else, exactly as in `make-bundle.sh`.
mkdir -p "$out/Contents/Resources/licenses"
for notice in README.md LICENSE-MIT LICENSE-APACHE ICED_AW-LICENSE LUCIDE-LICENSE TYPST_ASSETS-NOTICE; do
  install -m644 "$root/LICENSES/$notice" "$out/Contents/Resources/licenses/$notice"
done
install -m644 "$root/lib/PDFIUM-LICENSE" \
  "$out/Contents/Resources/licenses/PDFIUM-LICENSE" 2>/dev/null || true

cat > "$out/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>Pulpit</string>
  <key>CFBundleDisplayName</key>       <string>Pulpit</string>
  <key>CFBundleIdentifier</key>        <string>com.arelbundock.pulpit</string>
  <key>CFBundleExecutable</key>        <string>pulpit</string>
  <key>CFBundleIconFile</key>          <string>pulpit</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key>           <string>$version</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSApplicationCategoryType</key> <string>public.app-category.productivity</string>
  <!-- Iced draws through wgpu at the display's own scale; without this the
       window is upscaled from 1x and every glyph is soft on a Retina panel. -->
  <key>NSHighResolutionCapable</key>   <true/>
  <!-- The presenter is a foreground application with a Dock entry: it owns
       windows on two displays and must be able to take focus. -->
  <key>LSBackgroundOnly</key>          <false/>
  <key>LSMinimumSystemVersion</key>    <string>11.0</string>
  <!-- Opening a deck from the Finder is the ordinary way in on this desktop,
       so the bundle declares what it can open rather than only accepting a
       path on the command line. -->
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key>     <string>PDF document</string>
      <key>CFBundleTypeRole</key>     <string>Viewer</string>
      <key>LSHandlerRank</key>        <string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array><string>com.adobe.pdf</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Sign inside-out: nested code first, then the bundle that contains it.
# `--deep` is deprecated and signs less predictably than naming the parts.
codesign --force --sign - --timestamp=none "$out/Contents/MacOS/libpdfium.dylib"
codesign --force --sign - --timestamp=none "$out/Contents/MacOS/pulpit"
codesign --force --sign - --timestamp=none "$out"

# A signature that does not verify is worse than none: it fails at launch on
# the user's machine rather than here.
codesign --verify --strict --verbose=2 "$out"

# A universal bundle that lost a slice is the failure this cannot ship: it
# looks correct everywhere except on the architecture that is missing, where
# it either refuses to launch or quietly renders nothing.
if $universal; then
  for macho in "$out/Contents/MacOS/pulpit" "$out/Contents/MacOS/libpdfium.dylib"; do
    archs="$(lipo -archs "$macho")"
    case " $archs " in
      *" arm64 "*) ;;
      *) echo "ERROR: $macho has no arm64 slice (got: $archs)" >&2; exit 1 ;;
    esac
    case " $archs " in
      *" x86_64 "*) ;;
      *) echo "ERROR: $macho has no x86_64 slice (got: $archs)" >&2; exit 1 ;;
    esac
    echo "universal: $(basename "$macho") — $archs"
  done
fi

echo "bundle: $out"
