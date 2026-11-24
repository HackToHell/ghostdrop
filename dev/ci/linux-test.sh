#!/usr/bin/env bash
# Linux static FFI boundary checks + Rust unit tests (re-entrancy trap,
# clipboard state tracking, IPC timeout/cap, key/settings parsers).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

echo "==> 1/2 static FFI isolation checks"

# Only ghostty.rs may reference crate::ffi (other than ffi.rs itself).
FFI_USERS="$(grep -lE '\bcrate::ffi\b|\bghostty_[a-z0-9_]+\s*\(' linux/src/*.rs | grep -v '^linux/src/ffi\.rs$' || true)"
if [ "$FFI_USERS" != "linux/src/ghostty.rs" ]; then
  echo "FAIL: libghostty FFI must only be used from linux/src/ghostty.rs; found:" >&2
  echo "$FFI_USERS" >&2
  exit 1
fi

# ghostty.rs must not import UI, window-system, core, or IPC modules so C
# callbacks structurally cannot touch state or trigger re-entrancy.
if grep -En '\bcrate::(core|x11|wayland|ipc|draw|tab)\b' linux/src/ghostty.rs; then
  echo "FAIL: linux/src/ghostty.rs must not import core/backend/IPC/UI modules" >&2
  exit 1
fi
echo "PASS: static FFI isolation checks"

echo "==> 2/2 Rust unit tests"
cd "$ROOT/linux"
# shellcheck disable=SC2086 # CARGO_FLAGS is a flag list
cargo test ${CARGO_FLAGS:---locked}
