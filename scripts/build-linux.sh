#!/usr/bin/env bash
# Builds Ghostdrop for Linux: libghostty (shared, ReleaseFast) from the vendored
# Ghostty checkout, then the Rust app.
#
#   scripts/build-linux.sh            build into build/ghostty-linux + linux/target/release
#   scripts/build-linux.sh install    ...and install to $PREFIX (default ~/.local)
#
# Needs: a C toolchain, Rust (cargo), libclang (bindgen), pkg-config, libxkbcommon,
# libxcb, and a Mesa EGL. Zig is fetched into .tools/ if a matching one isn't on PATH.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GHOSTTY="$ROOT/vendor/ghostty"
OUT="$ROOT/build/ghostty-linux"
ZIG_VERSION="0.17.1"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) ZIG_SHA=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a ;;
  aarch64) ZIG_SHA=a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795 ;;
  *) echo "unsupported architecture: $ARCH" >&2; exit 1 ;;
esac
ZIG_DIR="$ROOT/.tools/zig-$ARCH-linux-$ZIG_VERSION"

if [ ! -f "$GHOSTTY/build.zig" ]; then
  echo "==> fetching ghostty submodule"
  git -C "$ROOT" submodule update --init --depth 1 vendor/ghostty
fi

ZIG="$(command -v zig || true)"
if [ -z "$ZIG" ] || [ "$("$ZIG" version)" != "$ZIG_VERSION" ]; then
  if [ ! -x "$ZIG_DIR/zig" ]; then
    echo "==> downloading zig $ZIG_VERSION ($ARCH)"
    mkdir -p "$ROOT/.tools"
    curl -fL# -o "$ROOT/.tools/zig.tar.xz" "https://ziglang.org/download/$ZIG_VERSION/zig-$ARCH-linux-$ZIG_VERSION.tar.xz"
    echo "$ZIG_SHA  $ROOT/.tools/zig.tar.xz" | sha256sum -c -
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

echo "==> building libghostty"
(cd "$GHOSTTY" && "$ZIG" build \
  -Doptimize=ReleaseFast \
  -Dapp-runtime=none \
  -Demit-embed-lib=true \
  -Dsentry=false \
  -Di18n=false \
  -p "$OUT")

echo "==> building ghostdrop"
(cd "$ROOT/linux" && GHOSTTY_DIR="$OUT" cargo build --release)
BIN="$ROOT/linux/target/release"
echo "==> $BIN/ghostdrop"

if [ "${1:-}" = install ]; then
  PREFIX="${PREFIX:-$HOME/.local}"
  echo "==> installing to $PREFIX"
  install -Dm755 "$BIN/ghostdrop" "$PREFIX/bin/ghostdrop"
  # The binary's rpath is $ORIGIN:$ORIGIN/../lib/ghostdrop.
  install -Dm755 "$BIN/libghostty.so" "$PREFIX/lib/ghostdrop/libghostty.so"
  # Ghostty resources (shell integration, themes) and its terminfo, which
  # Ghostty finds as <resources>/../terminfo.
  rm -rf "$PREFIX/share/ghostdrop/ghostty" "$PREFIX/share/ghostdrop/terminfo"
  mkdir -p "$PREFIX/share/ghostdrop"
  cp -R "$OUT/share/ghostty" "$OUT/share/terminfo" "$PREFIX/share/ghostdrop/"
  install -Dm644 "$ROOT/Resources/AGENTS.md" "$PREFIX/share/doc/ghostdrop/AGENTS.md"
  echo "==> done. Add to your WM config, e.g. sway:"
  echo "      exec $PREFIX/bin/ghostdrop --daemon"
  echo "      bindsym ctrl+grave exec $PREFIX/bin/ghostdrop"
fi
