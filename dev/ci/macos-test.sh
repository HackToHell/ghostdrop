#!/usr/bin/env bash
# macOS structural boundary checks + live libghostty callback/re-entrancy test suite.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

echo "==> 1/2 static FFI isolation & modal-loop checks"

# Only GhosttyFFI.swift may import GhosttyKit.
IMPORTERS="$(grep -l '^import GhosttyKit' Sources/Ghostdrop/*.swift || true)"
if [ "$IMPORTERS" != "Sources/Ghostdrop/GhosttyFFI.swift" ]; then
  echo "FAIL: GhosttyKit must only be imported by Sources/Ghostdrop/GhosttyFFI.swift; found:" >&2
  echo "$IMPORTERS" >&2
  exit 1
fi

# GhosttyFFI.swift must not import AppKit/Carbon/UserNotifications so C callbacks
# cannot touch windows, sheets, pasteboards, or responders.
if grep -E '^import (AppKit|Cocoa|Carbon|UserNotifications)\b' Sources/Ghostdrop/GhosttyFFI.swift; then
  echo "FAIL: GhosttyFFI.swift must not import UI frameworks" >&2
  exit 1
fi

# No runModal() in runtime/controller/view code (prevents nested modal runloops
# while libghostty surfaces exist).
if grep -En '\brunModal\s*\(' Sources/Ghostdrop/GhosttyFFI.swift Sources/Ghostdrop/GhosttyRuntime.swift Sources/Ghostdrop/DropController.swift Sources/Ghostdrop/SurfaceView.swift | grep -Ev '^[^:]+:[0-9]+:[[:space:]]*//'; then
  echo "FAIL: runModal() found in live runtime/controller/view code" >&2
  exit 1
fi

# AppDelegate may only use runModal() during pre-runtime startup (before GhosttyRuntime exists).
MODAL_COUNT="$(grep -En '\brunModal\s*\(' Sources/Ghostdrop/AppDelegate.swift | grep -Ev '^[^:]+:[0-9]+:[[:space:]]*//' | wc -l | tr -d ' ')"
if [ "$MODAL_COUNT" -gt 3 ]; then
  echo "FAIL: unexpected runModal() calls in AppDelegate.swift ($MODAL_COUNT > 3 pre-runtime startup dialogs)" >&2
  grep -n 'runModal(' Sources/Ghostdrop/AppDelegate.swift >&2
  exit 1
fi
echo "PASS: static isolation checks"

echo "==> 2/2 live macOS libghostty callback & prompt-queue tests"

# Ensure libghostty + modulemap exist.
make -s build/include/GhosttyKit/module.modulemap

BIN="build/ghostdrop-macos-test"
SOURCES=()
for f in Sources/Ghostdrop/*.swift; do
  [ "$(basename "$f")" = "main.swift" ] && continue
  SOURCES+=("$f")
done

swiftc -O -swift-version 5 -target arm64-apple-macos13.0 \
  -I build/include/GhosttyKit \
  -framework AppKit -framework Metal -framework QuartzCore -framework CoreText \
  -framework CoreGraphics -framework CoreVideo -framework IOSurface -framework IOKit \
  -framework Carbon -framework UniformTypeIdentifiers -framework UserNotifications \
  -framework ServiceManagement -lc++ \
  "${SOURCES[@]}" dev/ci/macos-test.swift build/ghostty/lib/libghostty.a \
  -o "$BIN"

"$BIN"
rm -f "$BIN"
