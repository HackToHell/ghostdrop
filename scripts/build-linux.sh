#!/usr/bin/env bash
# Builds, packages, or installs Ghostdrop on Linux (Wayland + X11).
#
#   scripts/build-linux.sh [build]        build into build/ghostty-linux + linux/target/release
#   scripts/build-linux.sh install        build from source and install to $PREFIX (default ~/.local)
#   scripts/build-linux.sh install-auto   use a prebuilt release tarball when on a clean release tag
#                                         (or RELEASE=...), else build from source (FROM_SOURCE=1 forces source)
#   scripts/build-linux.sh download [tag] download and install a prebuilt tarball (default: latest)
#   scripts/build-linux.sh tarball [ver]  package a previous build into build/ghostdrop-<ver>-linux-<arch>.tar.gz
#   scripts/build-linux.sh package        install a previous build only (honours DESTDIR; used by PKGBUILD)
#   scripts/build-linux.sh deps           install system build dependencies for the detected distro
#
# Source builds need: a C compiler, Rust >= 1.88 (cargo), pkg-config, and
# egl + xkbcommon + xkbcommon-x11 + xcb dev libraries (no clang/libclang needed).
# Zig 0.17.1 is fetched into .tools/ unless $ZIG or a matching `zig` on PATH is given.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GHOSTTY="$ROOT/vendor/ghostty"
OUT="$ROOT/build/ghostty-linux"
BIN="$ROOT/linux/target/release"
ZIG_VERSION="0.17.1"
MIN_RUSTC_MAJOR=1
MIN_RUSTC_MINOR=88
REPO="${GHOSTDROP_REPO:-HackToHell/ghostdrop}"
MODE="${1:-build}"
ARG2="${2:-}"

arch_name() {
  case "$(uname -m)" in
    x86_64|amd64) echo x86_64 ;;
    aarch64|arm64) echo aarch64 ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac
}

crate_version() {
  sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/linux/Cargo.toml" | head -n 1
}

distro_id() {
  if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    echo "${ID:-unknown} ${ID_LIKE:-}"
  else
    echo "unknown"
  fi
}

deps_cmd() {
  local d
  d="$(distro_id)"
  case " $d " in
    *" arch "*|*" manjaro "*|*" endeavouros "*|*" cachyos "*)
      echo "pacman -S --needed base-devel git pkgconf curl xz libglvnd libxkbcommon libxkbcommon-x11 libxcb"
      ;;
    *" ubuntu "*|*" debian "*|*" pop "*|*" linuxmint "*)
      echo "apt-get update && apt-get install -y build-essential git pkg-config curl xz-utils libegl-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev"
      ;;
    *" fedora "*|*" rhel "*|*" centos "*|*" rocky "*|*" alma "*|*" nobara "*)
      echo "dnf install -y gcc git pkgconf-pkg-config curl xz libglvnd-devel libxkbcommon-devel libxkbcommon-x11-devel libxcb-devel"
      ;;
    *" opensuse "*|*" suse "*)
      echo "zypper install -y gcc git pkgconf-pkg-config curl xz libglvnd-devel libxkbcommon-devel libxkbcommon-x11-devel libxcb-devel"
      ;;
    *)
      echo ""
      ;;
  esac
}

have_matching_zig() {
  local arch zig_dir z
  arch="$(arch_name)"
  zig_dir="$ROOT/.tools/zig-$arch-linux-$ZIG_VERSION"
  z="${ZIG:-$(command -v zig || true)}"
  if [ -n "$z" ] && [ "$("$z" version 2>/dev/null || true)" = "$ZIG_VERSION" ]; then
    return 0
  fi
  [ -x "$zig_dir/zig" ]
}

check_deps() {
  local missing=()
  local need_rust=""

  if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
    missing+=("C compiler (cc/gcc)")
  fi
  command -v git >/dev/null 2>&1 || missing+=("git")
  command -v pkg-config >/dev/null 2>&1 || missing+=("pkg-config")
  if ! have_matching_zig; then
    command -v curl >/dev/null 2>&1 || missing+=("curl (to fetch Zig $ZIG_VERSION)")
    command -v tar >/dev/null 2>&1 || missing+=("tar")
    command -v xz >/dev/null 2>&1 || missing+=("xz")
  fi

  if command -v pkg-config >/dev/null 2>&1; then
    for lib in egl xkbcommon xkbcommon-x11 xcb; do
      pkg-config --exists "$lib" 2>/dev/null || missing+=("pkg-config:$lib")
    done
  fi

  if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    need_rust="missing (need rustc >= ${MIN_RUSTC_MAJOR}.${MIN_RUSTC_MINOR})"
  else
    local rver maj min
    rver="$(rustc --version | awk '{print $2}')"
    maj="${rver%%.*}"
    min="${rver#*.}"; min="${min%%.*}"
    if [ "$maj" -lt "$MIN_RUSTC_MAJOR" ] || { [ "$maj" -eq "$MIN_RUSTC_MAJOR" ] && [ "$min" -lt "$MIN_RUSTC_MINOR" ]; }; then
      need_rust="rustc $rver is too old (need >= ${MIN_RUSTC_MAJOR}.${MIN_RUSTC_MINOR})"
    fi
  fi

  if [ "${#missing[@]}" -eq 0 ] && [ -z "$need_rust" ]; then
    return 0
  fi

  echo "ghostdrop: missing Linux build prerequisites:" >&2
  for m in "${missing[@]}"; do echo "  - $m" >&2; done
  [ -n "$need_rust" ] && echo "  - Rust: $need_rust" >&2
  echo "" >&2

  local cmd
  cmd="$(deps_cmd)"
  if [ "${#missing[@]}" -gt 0 ]; then
    if [ -n "$cmd" ]; then
      echo "Install system packages with 'make deps' or:" >&2
      echo "  sudo sh -c '$cmd'" >&2
    else
      echo "Install system packages providing: gcc, git, pkg-config, curl, xz, libEGL, libxkbcommon, libxkbcommon-x11, libxcb dev headers." >&2
    fi
  fi
  if [ -n "$need_rust" ]; then
    echo "Install/update Rust via rustup (distro packages are often older than ${MIN_RUSTC_MAJOR}.${MIN_RUSTC_MINOR}):" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  fi
  echo "" >&2
  echo "Or install a prebuilt release binary (no compilers needed) with:" >&2
  echo "  make install RELEASE=latest" >&2
  exit 1
}

install_deps() {
  local cmd
  cmd="$(deps_cmd)"
  if [ -z "$cmd" ]; then
    echo "ghostdrop: unknown distro ($(distro_id)); install gcc, git, pkg-config, curl, xz, and egl/xkbcommon/xkbcommon-x11/xcb dev headers manually." >&2
    exit 1
  fi
  echo "==> installing build dependencies: $cmd"
  if [ "$(id -u)" -eq 0 ]; then
    sh -c "$cmd"
  else
    sudo sh -c "$cmd"
  fi
  if ! command -v rustc >/dev/null 2>&1; then
    echo "==> Rust not found; install via rustup:"
    echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  fi
}

build() {
  check_deps
  local arch zig_sha zig_dir
  arch="$(arch_name)"
  case "$arch" in
    x86_64) zig_sha=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a ;;
    aarch64) zig_sha=a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795 ;;
  esac
  zig_dir="$ROOT/.tools/zig-$arch-linux-$ZIG_VERSION"

  if [ ! -f "$GHOSTTY/build.zig" ]; then
    echo "==> fetching ghostty submodule"
    git -C "$ROOT" submodule update --init --depth 1 vendor/ghostty
  fi

  ZIG="${ZIG:-$(command -v zig || true)}"
  if [ -z "$ZIG" ] || [ "$("$ZIG" version 2>/dev/null || true)" != "$ZIG_VERSION" ]; then
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

  echo "==> building libghostty (cpu=${ZIG_CPU:-baseline})"
  (cd "$GHOSTTY" && "$ZIG" build \
    -Doptimize=ReleaseFast \
    -Dcpu="${ZIG_CPU:-baseline}" \
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

warn_path() {
  local dest="$1"
  if [ -z "${DESTDIR:-}" ] && [[ ":$PATH:" != *":$dest/bin:"* ]]; then
    echo "note: $dest/bin is not in PATH (add: export PATH=\"$dest/bin:\$PATH\")"
  fi
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
  warn_path "$dest"
}

make_tarball() {
  local ver arch name stage
  ver="${ARG2:-$(crate_version)}"
  arch="$(arch_name)"
  if [ ! -x "$BIN/ghostdrop" ] || [ ! -f "$BIN/libghostty.so" ] || [ ! -d "$OUT/share/ghostty" ]; then
    echo "error: build artifacts missing — run 'scripts/build-linux.sh build' first" >&2
    exit 1
  fi
  name="ghostdrop-${ver}-linux-${arch}"
  stage="$ROOT/build/tarball/$name"
  rm -rf "$stage" "$ROOT/build/$name.tar.gz" "$ROOT/build/$name.tar.gz.sha256"
  DESTDIR="$stage" PREFIX="" install_files
  install -Dm644 "$ROOT/LICENSE" "$stage/share/licenses/ghostdrop/LICENSE"
  [ -f "$GHOSTTY/LICENSE" ] && install -Dm644 "$GHOSTTY/LICENSE" "$stage/share/licenses/ghostdrop/LICENSE.ghostty"
  tar -C "$ROOT/build/tarball" -czf "$ROOT/build/$name.tar.gz" "$name"
  rm -rf "$ROOT/build/tarball"
  (cd "$ROOT/build" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
  echo "==> $ROOT/build/$name.tar.gz ($(du -h "$ROOT/build/$name.tar.gz" | cut -f1))"
}

resolve_release_tag() {
  local req="$1"
  if [ "$req" != "latest" ]; then
    echo "$req"
    return 0
  fi
  local loc tag
  loc="$(curl -fsSI "https://github.com/$REPO/releases/latest" | awk -F': ' 'tolower($1)=="location" {sub(/\r$/, "", $2); print $2}' | tail -n 1)"
  tag="${loc##*/}"
  case "$tag" in
    v*) echo "$tag" ;;
    *) return 1 ;;
  esac
}

download_and_install() {
  local req="${1:-latest}"
  local arch tag ver asset base_url tmp stage dest
  command -v curl >/dev/null 2>&1 || { echo "ghostdrop: curl is required to download releases" >&2; return 1; }
  command -v tar >/dev/null 2>&1 || { echo "ghostdrop: tar is required to extract releases" >&2; return 1; }
  command -v sha256sum >/dev/null 2>&1 || { echo "ghostdrop: sha256sum is required to verify releases" >&2; return 1; }

  arch="$(arch_name)"
  tag="$(resolve_release_tag "$req")" || { echo "ghostdrop: could not resolve release '$req'" >&2; return 1; }
  ver="${tag#v}"
  asset="ghostdrop-${ver}-linux-${arch}.tar.gz"
  base_url="${GHOSTDROP_RELEASE_URL:-https://github.com/$REPO/releases/download/$tag}"

  tmp="$(mktemp -d)"
  # Clean up temp dir on any exit path from this function.
  local rc=0
  {
    echo "==> downloading $asset ($tag)"
    curl -fsSL -o "$tmp/$asset" "$base_url/$asset" || { rc=1; }
    if [ "$rc" -eq 0 ]; then
      curl -fsSL -o "$tmp/$asset.sha256" "$base_url/$asset.sha256" || { rc=1; }
    fi
    if [ "$rc" -eq 0 ]; then
      (cd "$tmp" && sha256sum -c "$asset.sha256") || { rc=1; }
    fi
    if [ "$rc" -eq 0 ]; then
      stage="$tmp/stage"
      mkdir -p "$stage"
      tar -xzf "$tmp/$asset" -C "$stage" --strip-components=1 || { rc=1; }
    fi
    if [ "$rc" -eq 0 ] && ! "$stage/bin/ghostdrop" +version >/dev/null 2>&1; then
      echo "ghostdrop: prebuilt binary failed runtime check on this system (missing runtime libs or glibc < 2.35)" >&2
      rc=1
    fi
    if [ "$rc" -eq 0 ]; then
      dest="${DESTDIR:-}$PREFIX"
      echo "==> installing prebuilt $tag to $dest"
      install -Dm755 "$stage/bin/ghostdrop" "$dest/bin/ghostdrop"
      install -Dm755 "$stage/lib/ghostdrop/libghostty.so" "$dest/lib/ghostdrop/libghostty.so"
      rm -rf "$dest/share/ghostdrop/ghostty" "$dest/share/ghostdrop/terminfo"
      mkdir -p "$dest/share/ghostdrop"
      cp -R "$stage/share/ghostdrop/ghostty" "$stage/share/ghostdrop/terminfo" "$dest/share/ghostdrop/"
      install -Dm644 "$stage/share/doc/ghostdrop/AGENTS.md" "$dest/share/doc/ghostdrop/AGENTS.md"
      if [ -d "$stage/share/licenses/ghostdrop" ]; then
        mkdir -p "$dest/share/licenses/ghostdrop"
        cp -R "$stage/share/licenses/ghostdrop/." "$dest/share/licenses/ghostdrop/"
      fi
      warn_path "$dest"
      echo "==> done ($tag installed to $dest/bin/ghostdrop)"
    fi
  }
  rm -rf "$tmp"
  return "$rc"
}

install_auto() {
  PREFIX="${PREFIX:-$HOME/.local}"
  if [ "${FROM_SOURCE:-0}" = "1" ]; then
    build
    install_files
    return 0
  fi

  local want_tag="${RELEASE:-}"
  if [ -z "$want_tag" ] && command -v git >/dev/null 2>&1 && git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    if [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no -- . ':(exclude)vendor/ghostty' 2>/dev/null)" ]; then
      want_tag="$(git -C "$ROOT" describe --tags --exact-match --match 'v*' HEAD 2>/dev/null || true)"
    fi
  fi

  if [ -n "$want_tag" ]; then
    if download_and_install "$want_tag"; then
      return 0
    fi
    echo "==> prebuilt download unavailable for '$want_tag'; falling back to building from source" >&2
  fi

  build
  install_files
  echo "==> done. Start it from your window manager, e.g."
  echo "      i3:   exec --no-startup-id $PREFIX/bin/ghostdrop --daemon"
  echo "      sway: exec $PREFIX/bin/ghostdrop --daemon"
  echo "            bindsym ctrl+grave exec $PREFIX/bin/ghostdrop"
}

case "$MODE" in
  build) build ;;
  deps) install_deps ;;
  install)
    PREFIX="${PREFIX:-$HOME/.local}"
    build
    install_files
    echo "==> done. Start it from your window manager, e.g."
    echo "      i3:   exec --no-startup-id $PREFIX/bin/ghostdrop --daemon"
    echo "      sway: exec $PREFIX/bin/ghostdrop --daemon"
    echo "            bindsym ctrl+grave exec $PREFIX/bin/ghostdrop"
    ;;
  install-auto)
    install_auto
    ;;
  download)
    PREFIX="${PREFIX:-$HOME/.local}"
    download_and_install "${ARG2:-latest}"
    ;;
  tarball)
    make_tarball
    ;;
  package)
    PREFIX="${PREFIX:-/usr}"
    install_files
    ;;
  *)
    echo "usage: $0 [build|install|install-auto|download [tag]|tarball [ver]|package|deps]" >&2
    exit 2
    ;;
esac
