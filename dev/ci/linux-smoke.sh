#!/usr/bin/env bash
# Smoke test for an installed Ghostdrop package (CI; needs Xvfb + Mesa).
set -euo pipefail
G=/usr/bin/ghostdrop
T="$(mktemp -d)"
# Failures show up as GitHub annotations (readable without access to the logs).
report() {
  echo "::error::smoke test failed at line $1: $2"
  [ -f "$T/gd.log" ] && echo "::error::daemon log: $(tail -c 600 "$T/gd.log" | tr '\n' ' ')"
  [ -f "$T/xvfb.log" ] && echo "::error::Xvfb log: $(tail -c 300 "$T/xvfb.log" | tr '\n' ' ')"
}
trap 'report $LINENO "$BASH_COMMAND"' ERR
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$T"' EXIT

GHOSTDROP_CONFIG="$T/config" "$G" check
"$G" +version
# Container images often skip usr/share/doc (pacman NoExtract): check the package.
pacman -Qlq ghostdrop | grep -qx /usr/share/doc/ghostdrop/AGENTS.md
test -f /usr/share/ghostdrop/terminfo/x/xterm-ghostty
test -d /usr/share/ghostdrop/ghostty/shell-integration
# libghostty resolves next to the binary (rpath $ORIGIN/../lib/ghostdrop).
ldd "$G" | grep -E 'libghostty\.so => .*/lib/ghostdrop/libghostty\.so'

# Render a frame on a virtual X server (software OpenGL).
Xvfb :77 -screen 0 1280x800x24 >"$T/xvfb.log" 2>&1 &
for _ in $(seq 50); do [ -e /tmp/.X11-unix/X77 ] && break; sleep 0.1; done
export DISPLAY=:77 GHOSTDROP_CONFIG="$T/config" GHOSTDROP_SOCKET="$T/gd.sock" GHOSTDROP_DEBUG=1
"$G" --daemon >"$T/gd.log" 2>&1 &
for _ in $(seq 100); do "$G" ctl ping >/dev/null 2>&1 && break; sleep 0.1; done
"$G" ctl show
# Software OpenGL compiles its shaders on first use: allow a slow runner a minute.
for _ in $(seq 300); do
  if "$G" ctl dump | grep -qE '"frame":"[0-9]+x[0-9]+'; then
    echo "rendered: $("$G" ctl dump | grep -oE '"frame":"[^"]*"')"
    "$G" ctl quit force >/dev/null
    exit 0
  fi
  sleep 0.2
done
report "$LINENO" "no frame rendered within 60 s; dump: $("$G" ctl dump 2>&1 | head -c 300)"
exit 1
