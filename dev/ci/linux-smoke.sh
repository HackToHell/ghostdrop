#!/usr/bin/env bash
# Smoke test for an installed Ghostdrop package (CI; needs Xvfb + Mesa).
set -euo pipefail
G=/usr/bin/ghostdrop
T="$(mktemp -d)"
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$T"' EXIT

GHOSTDROP_CONFIG="$T/config" "$G" check
"$G" +version
test -f /usr/share/doc/ghostdrop/AGENTS.md
test -f /usr/share/ghostdrop/terminfo/x/xterm-ghostty
test -d /usr/share/ghostdrop/ghostty/shell-integration
# libghostty resolves next to the binary (rpath $ORIGIN/../lib/ghostdrop).
ldd "$G" | grep -E 'libghostty\.so => .*/lib/ghostdrop/libghostty\.so'

# Render a frame on a virtual X server (software OpenGL).
Xvfb :77 -screen 0 1280x800x24 >"$T/xvfb.log" 2>&1 &
for _ in $(seq 50); do [ -e /tmp/.X11-unix/X77 ] && break; sleep 0.1; done
export DISPLAY=:77 GHOSTDROP_CONFIG="$T/config" GHOSTDROP_SOCKET="$T/gd.sock" GHOSTDROP_DEBUG=1
"$G" --daemon >"$T/gd.log" 2>&1 &
for _ in $(seq 50); do "$G" ctl ping >/dev/null 2>&1 && break; sleep 0.1; done
"$G" ctl show
for _ in $(seq 50); do
  if "$G" ctl dump | grep -qE '"frame":"[0-9]+x[0-9]+'; then
    echo "rendered: $("$G" ctl dump | grep -oE '"frame":"[^"]*"')"
    "$G" ctl quit force >/dev/null
    exit 0
  fi
  sleep 0.2
done
echo "no frame rendered"; cat "$T/gd.log"; exit 1
