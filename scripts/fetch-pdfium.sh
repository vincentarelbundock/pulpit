#!/usr/bin/env bash
# Fetch a pinned PDFium binary into ./lib.
#
# bblanchon/pdfium-binaries is an unaffiliated third-party supply-chain
# dependency. Versions and hashes are pinned here, verified on download, and
# must be updated deliberately — never by "latest".
#
# Producing internal builds is the documented escape hatch if the service
# disappears or drops a target; see `docs-src/install.typ`.
#
# With no arguments this fetches the artifact for the machine it runs on,
# which is what every build target wants. `--platform` and `--dest` exist for
# one caller: the universal macOS app, which needs both Apple architectures
# staged separately before `lipo` fuses them, and cannot get the second one
# from `uname`.
set -euo pipefail

PDFIUM_RELEASE="chromium/7999"

usage() {
  echo "usage: fetch-pdfium.sh [--platform SLUG] [--dest DIR]" >&2
  echo "  SLUG is one of: linux-x64 linux-arm64 mac-arm64 mac-x64" >&2
  echo "                  win-x64 win-arm64  (default: this machine)" >&2
}

# The platform slug for the machine this runs on. Git Bash and MSYS report the
# host as MINGW64_NT-… or MSYS_NT-…, so the Windows arms match on a prefix
# rather than an exact string.
host_platform() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)  echo "linux-x64" ;;
    Linux-aarch64) echo "linux-arm64" ;;
    Darwin-arm64)  echo "mac-arm64" ;;
    Darwin-x86_64) echo "mac-x64" ;;
    MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64) echo "win-x64" ;;
    MINGW*-aarch64|MSYS*-aarch64|CYGWIN*-aarch64|MINGW*-arm64|MSYS*-arm64)
                   echo "win-arm64" ;;
    *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
  esac
}

# The pinned artifact and its hash, per platform. One table, so a pin moves in
# one place and a target with no entry is an error rather than a guess.
pin_for() {
  case "$1" in
    linux-x64)   ARTIFACT="pdfium-linux-x64.tgz"
                 SHA256="c3af580f9df0fef9545b44115bc5ea440f286956b5f231df69fb373b8efc4f69" ;;
    linux-arm64) ARTIFACT="pdfium-linux-arm64.tgz"
                 SHA256="a19862a36e2b2da3c3fb43f0deef45fbbc331f58cd47943782ae4bd9db4c66d9" ;;
    mac-arm64)   ARTIFACT="pdfium-mac-arm64.tgz"
                 SHA256="e214ee33f22b2204daa765a545aee1e425d88448e6154dac95c6a06206b7437f" ;;
    mac-x64)     ARTIFACT="pdfium-mac-x64.tgz"
                 SHA256="4b924d948d2ec4863435d375a94541b4003c59f8adc28cc5e4236b0ab81a355d" ;;
    win-x64)     ARTIFACT="pdfium-win-x64.tgz"
                 SHA256="55329d5cb5de8a379a2fc563106492d7f385a1f795d18970922c71f708f9fbb4" ;;
    win-arm64)   ARTIFACT="pdfium-win-arm64.tgz"
                 SHA256="ccc39582f8d9ac0bd57b8e279e8cfc5067cc0f0b4db9bc179ddf7cd3636c0bd6" ;;
    *) echo "no pinned PDFium artifact for platform: $1" >&2; usage; exit 1 ;;
  esac
}

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
platform=""
dest="$root/lib"

while [ $# -gt 0 ]; do
  case "$1" in
    --platform) platform="${2:?--platform needs a slug}"; shift 2 ;;
    --dest)     dest="${2:?--dest needs a directory}"; shift 2 ;;
    -h|--help)  usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

[ -n "$platform" ] || platform="$(host_platform)"
pin_for "$platform"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

url="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_RELEASE//\//%2F}/$ARTIFACT"
echo "fetching $url"
curl --fail --location --silent --show-error --output "$work/$ARTIFACT" "$url"

# macOS ships `shasum`, not GNU coreutils' `sha256sum`. The hash is the whole
# point of this script, so the absence of both is an error rather than a skip.
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/$ARTIFACT" | cut -d' ' -f1)"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$work/$ARTIFACT" | cut -d' ' -f1)"
else
  echo "ERROR: no sha256sum or shasum available to verify $ARTIFACT" >&2
  exit 1
fi
if [ -z "$SHA256" ]; then
  echo "ERROR: no pinned hash recorded for $ARTIFACT." >&2
  echo "Verify the artifact yourself, then pin: $actual" >&2
  exit 1
fi
if [ "$actual" != "$SHA256" ]; then
  echo "ERROR: hash mismatch for $ARTIFACT" >&2
  echo "  expected $SHA256" >&2
  echo "  actual   $actual" >&2
  exit 1
fi

mkdir -p "$dest"
tar -xzf "$work/$ARTIFACT" -C "$work"
find "$work" -name 'libpdfium.*' -o -name 'pdfium.dll' | while read -r library; do
  install -m 0644 "$library" "$dest/"
done
cp "$work/LICENSE" "$dest/PDFIUM-LICENSE" 2>/dev/null || true
cp "$work/VERSION" "$dest/PDFIUM-VERSION" 2>/dev/null || true

echo "installed into $dest:"
ls -l "$dest"
echo
echo "Run pulpit with PULPIT_PDFIUM_PATH=$dest if it is not found automatically."
