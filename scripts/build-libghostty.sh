#!/usr/bin/env bash
# Builds libghostty (static, ReleaseFast) from the vendored Ghostty checkout.
# Needs only the Xcode Command Line Tools; Zig is fetched into .tools/ if missing.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GHOSTTY="$ROOT/vendor/ghostty"
OUT="$ROOT/build/ghostty"
ZIG_VERSION="0.17.1"
ZIG_DIR="$ROOT/.tools/zig-aarch64-macos-$ZIG_VERSION"

if [ ! -f "$GHOSTTY/build.zig" ]; then
  echo "==> fetching ghostty submodule"
  git -C "$ROOT" submodule update --init --depth 1 vendor/ghostty
fi

# Zig: prefer a matching system zig, else download the pinned version.
ZIG="$(command -v zig || true)"
if [ -z "$ZIG" ] || [ "$("$ZIG" version)" != "$ZIG_VERSION" ]; then
  if [ ! -x "$ZIG_DIR/zig" ]; then
    echo "==> downloading zig $ZIG_VERSION"
    mkdir -p "$ROOT/.tools"
    curl -fL# -o "$ROOT/.tools/zig.tar.xz" "https://ziglang.org/download/$ZIG_VERSION/zig-aarch64-macos-$ZIG_VERSION.tar.xz"
    echo "75f1bc7a1e9852f17ec9a0c1895f767c4ec56d46bc8a834920eece1d0359dd8d  $ROOT/.tools/zig.tar.xz" | shasum -a 256 -c -
    tar -xf "$ROOT/.tools/zig.tar.xz" -C "$ROOT/.tools"
    rm "$ROOT/.tools/zig.tar.xz"
  fi
  ZIG="$ZIG_DIR/zig"
fi

# Apply our patches (idempotent).
for p in "$ROOT"/patches/ghostty/*.patch; do
  if git -C "$GHOSTTY" apply --reverse --check "$p" 2>/dev/null; then
    continue
  fi
  echo "==> applying $(basename "$p")"
  git -C "$GHOSTTY" apply "$p"
done

# Precompile shaders when the Metal toolchain exists (full Xcode); otherwise
# embed the source and compile once at runtime (cached by Metal).
# (Probe by compiling: Xcode 26 ships a `metal` stub even without the toolchain.)
METAL_RUNTIME="${GHOSTDROP_METAL_RUNTIME:-}"
if [ -z "$METAL_RUNTIME" ]; then
  METAL_RUNTIME=true
  probe="$(mktemp -d)"
  printf 'kernel void k() {}\n' > "$probe/p.metal"
  if xcrun -sdk macosx metal -c "$probe/p.metal" -o "$probe/p.air" >/dev/null 2>&1; then
    METAL_RUNTIME=false
  fi
  rm -rf "$probe"
fi

echo "==> building libghostty (metal-runtime-compile=$METAL_RUNTIME)"
if [ -n "${GITHUB_ACTIONS:-}" ]; then
  echo "::notice title=Metal shaders::$([ "$METAL_RUNTIME" = false ] && echo precompiled metallib || echo compiled at runtime)"
fi
cd "$GHOSTTY"
# Clean PATH: stray pkg-config shims confuse the build.
env -i HOME="$HOME" PATH="/usr/bin:/bin:/usr/sbin:/sbin" "$ZIG" build \
  -Doptimize=ReleaseFast \
  -Dapp-runtime=none \
  -Demit-xcframework=false \
  -Demit-macos-app=false \
  -Demit-embed-lib=true \
  -Dmetal-runtime-compile=$METAL_RUNTIME \
  -Dsentry=false \
  -Di18n=false \
  -p "$OUT"

echo "==> $OUT/lib/libghostty.a"
