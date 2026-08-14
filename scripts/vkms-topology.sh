#!/usr/bin/env bash
# Script virtual connector transitions with VKMS and capture them.
#
# VKMS (the kernel's virtual KMS driver) provides connectors that can be
# enabled and disabled without hardware, which is how connection,
# disconnection and re-enumeration get exercised against a real display server
# instead of only against snapshots.
#
# What this does NOT replace: compositor-specific behaviour, EDID quirks,
# docks, suspend/resume and physical projectors. Those stay manual, by design
# (`docs-src/internals.typ`).
#
# Requires: root (modprobe, DRM writeback), a kernel with CONFIG_DRM_VKMS,
# and an X server or compositor able to use the VKMS device.
#
#   sudo ./scripts/vkms-topology.sh                 # run the full sequence
#   sudo ./scripts/vkms-topology.sh capture out.txt # just capture transitions
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-run}"
output="${2:-$root/crates/pulpit-display/tests/topology/vkms-capture.txt}"
dump="$root/target/debug/pulpit-topology"

log() { printf '\n== %s\n' "$*"; }

require_root() {
  if [ "$(id -u)" -ne 0 ]; then
    echo "this needs root: modprobe and DRM sysfs writes" >&2
    exit 1
  fi
}

load_vkms() {
  log "loading VKMS"
  if ! modprobe vkms enable_cursor=1 2>/dev/null; then
    echo "cannot load vkms — is CONFIG_DRM_VKMS available in this kernel?" >&2
    exit 1
  fi
  # Give udev a moment to create the device nodes.
  sleep 1
  for card in /sys/class/drm/card*; do
    [ -e "$card/device/uevent" ] || continue
    if grep -qs 'DRIVER=vkms' "$card/device/uevent"; then
      echo "vkms is $card"
      return 0
    fi
  done
  echo "vkms loaded but no card found" >&2
  exit 1
}

# Enable or disable a connector through DRM sysfs. Not every kernel exposes
# `status` as writable; when it does not, this reports and continues so the
# rest of the sequence still runs.
set_connector() {
  local connector="$1" state="$2"
  if [ -w "$connector/status" ]; then
    echo "$state" > "$connector/status"
    echo "  $connector -> $state"
  else
    echo "  $connector: status is not writable on this kernel; skipping"
  fi
  sleep 1
}

vkms_connectors() {
  for connector in /sys/class/drm/card*-*; do
    [ -e "$connector/status" ] || continue
    if grep -qs 'DRIVER=vkms' "$connector/device/uevent" 2>/dev/null; then
      echo "$connector"
    fi
  done
}

build_tool() {
  if [ ! -x "$dump" ]; then
    log "building pulpit-topology"
    (cd "$root" && cargo build -p pulpit-display --bin pulpit-topology)
  fi
}

capture() {
  build_tool
  log "capturing topology transitions to $output"
  "$dump" --watch --timeout "${CAPTURE_SECONDS:-45}" \
    --description "captured under VKMS by scripts/vkms-topology.sh" > "$output" &
  local capture_pid=$!

  log "scripting connector transitions"
  local connectors
  mapfile -t connectors < <(vkms_connectors)
  if [ "${#connectors[@]}" -eq 0 ]; then
    echo "no VKMS connectors found" >&2
  fi
  for connector in "${connectors[@]}"; do
    set_connector "$connector" detach
    set_connector "$connector" on
    # Repeated identical notifications must be harmless; issue a burst.
    set_connector "$connector" on
    set_connector "$connector" on
    set_connector "$connector" detach
    set_connector "$connector" on
  done

  wait "$capture_pid" || true
  log "captured:"
  cat "$output"
}

replay() {
  log "replaying every committed scenario through the reconciler"
  (cd "$root" && cargo test -p pulpit-display --test topology_script)
}

case "$mode" in
  capture)
    require_root
    load_vkms
    capture
    ;;
  replay)
    replay
    ;;
  run)
    require_root
    load_vkms
    capture
    replay
    ;;
  *)
    echo "usage: $0 [run|capture|replay] [output-file]" >&2
    exit 2
    ;;
esac

log "done"
echo "Commit the captured file to keep this topology as a permanent test."
