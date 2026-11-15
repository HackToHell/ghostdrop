#!/usr/bin/env bash
# Builds Ghostdrop for Linux: libghostty (shared, ReleaseFast) from the vendored
# Ghostty checkout, then the Rust app.
#
#   scripts/build-linux.sh            build into build/ghostty-linux + linux/target/release
#   scripts/build-linux.sh install    ...and install to $PREFIX (default ~/.local)
#   scripts/build-linux.sh package    install a previous build only (honours DESTDIR;
#                                     used by packaging/arch/PKGBUILD)
#
# Needs: a C toolchain, Rust (cargo), libclang (bindgen), pkg-config, libxkbcommon,
# libxcb and an EGL driver. Zig 0.17.1 is fetched into .tools/ unless $ZIG or a
# matching `zig` on PATH is given. CARGO_FLAGS defaults to --locked.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GHOSTTY="$ROOT/vendor/ghostty"
OUT="$ROOT/build/ghostty-linux"
BIN="$ROOT/linux/target/release"
ZIG_VERSION="0.17.1"
MODE="${1:-build}"

build() {
  local arch zig_sha zig_dir
  arch="$(uname -m)"
  case "$arch" in
    x86_64) zig_sha=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a ;;
    aarch64) zig_sha=a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795 ;;
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;;
  esac
  zig_dir="$ROOT/.tools/zig-$arch-linux-$ZIG_VERSION"

  if [ ! -f "$GHOSTTY/build.zig" ]; then
    echo "==> fetching ghostty submodule"
    git -C "$ROOT" submodule update --init --depth 1 vendor/ghostty
  fi

  ZIG="${ZIG:-$(command -v zig || true)}"
  if [ -z "$ZIG" ] || [ "$("$ZIG" version)" != "$ZIG_VERSION" ]; then
    if [ ! -x "$zig_dir/zig" ]; then
      echo "==> downloading zig $ZIG_VERSION ($arch)"
      mkdir -p "$ROOT/.tools"
      curl -fL# -o "$ROOT/.tools/zig.tar.xz" "https://ziglang.org/download/$ZIG_VERSION/zig-$arch-linux-$ZIG_VERSION.tar.xz"
      echo "$zig_sha  $ROOT/.tools/zig.tar.xz" | sha256sum -c -
      tar -xf "$ROOT/.tools/zig.tar.xz" -C "$ROOT/.tools"
      rm "$ROOT/.tools/zig.tar.xz"
    fi
    ZIG="$zig_dir/zig"
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
  # shellcheck disable=SC2086 # CARGO_FLAGS is a flag list
  (cd "$ROOT/linux" && GHOSTTY_DIR="$OUT" cargo build --release ${CARGO_FLAGS:---locked})
  echo "==> $BIN/ghostdrop"
}

install_files() {
  local dest="${DESTDIR:-}$PREFIX"
  echo "==> installing to $dest"
  install -Dm755 "$BIN/ghostdrop" "$dest/bin/ghostdrop"
  # The binary's rpath is $ORIGIN:$ORIGIN/../lib/ghostdrop.
  install -Dm755 "$BIN/libghostty.so" "$dest/lib/ghostdrop/libghostty.so"
  # Ghostty resources (shell integration, themes) and its terminfo, which
  # Ghostty finds as <resources>/../terminfo.
  rm -rf "$dest/share/ghostdrop/ghostty" "$dest/share/ghostdrop/terminfo"
  mkdir -p "$dest/share/ghostdrop"
  cp -R "$OUT/share/ghostty" "$OUT/share/terminfo" "$dest/share/ghostdrop/"
  install -Dm644 "$ROOT/Resources/AGENTS.md" "$dest/share/doc/ghostdrop/AGENTS.md"
}

case "$MODE" in
  build) build ;;
  install)
    PREFIX="${PREFIX:-$HOME/.local}"
    build
    install_files
    echo "==> done. Start it from your window manager, e.g."
    echo "      i3:   exec --no-startup-id $PREFIX/bin/ghostdrop --daemon"
    echo "      sway: exec $PREFIX/bin/ghostdrop --daemon"
    echo "            bindsym ctrl+grave exec $PREFIX/bin/ghostdrop"
    ;;
  package)
    PREFIX="${PREFIX:-/usr}"
    install_files
    ;;
  *) echo "usage: $0 [build|install|package]" >&2; exit 2 ;;
esac
