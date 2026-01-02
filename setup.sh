#!/usr/bin/env bash
set -euo pipefail

# quictls-openssl setup script (linux only)
# clones, builds, and installs quictls openssl.

PREFIX="${PREFIX:-/opt/quictls}"
LIBDIR="${LIBDIR:-lib}"
REPO_URL="${REPO_URL:-https://github.com/quictls/openssl.git}"
SRC_DIR="${SRC_DIR:-quictls-openssl}"
BUILD_JOBS="${BUILD_JOBS:-$(nproc)}"

log() { echo -e "\n>>> $*"; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: missing required command: $1" >&2
    exit 1
  }
}

detect_config_target_linux() {
  local arch
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) echo "linux-x86_64" ;;
    aarch64|arm64) echo "linux-aarch64" ;;
    ppc64le) echo "linux-ppc64le" ;;
    riscv64) echo "linux64-riscv64" ;;
    *) echo "linux-x86_64" ;;
  esac
}

require_sudo_if_needed() {
  if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
    need_cmd sudo
    sudo -v
  fi
}

main() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: this script supports linux only." >&2
    exit 1
  fi

  need_cmd git
  need_cmd make
  need_cmd perl
  need_cmd cc
  need_cmd nproc

  local target
  target="$(detect_config_target_linux)"

  log "using settings:"
  echo "  prefix: $PREFIX"
  echo "  libdir: $LIBDIR"
  echo "  source dir: $SRC_DIR"
  echo "  configure target: $target"
  echo "  make jobs: $BUILD_JOBS"

  if [[ -d "$SRC_DIR/.git" ]]; then
    log "updating existing repo: $SRC_DIR"
    git -C "$SRC_DIR" fetch --all --tags
    git -C "$SRC_DIR" pull --ff-only
  else
    log "cloning repo..."
    git clone "$REPO_URL" "$SRC_DIR"
  fi

  log "configuring build..."
  (
    cd "$SRC_DIR"
    ./Configure "$target" \
      --prefix="$PREFIX" \
      --libdir="$LIBDIR" \
      enable-tls1_3 \
      enable-ktls \
      enable-ssl3-method \
      no-deprecated
  )

  log "building..."
  ( cd "$SRC_DIR" && make -j"$BUILD_JOBS" )

  log "installing..."
  if [[ -w "$PREFIX" ]] || [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    ( cd "$SRC_DIR" && make install )
  else
    require_sudo_if_needed
    sudo make -C "$SRC_DIR" install
  fi

  log "done!"
  echo
  echo "to use the installed quictls-openssl, run:"
  echo "  export LD_LIBRARY_PATH=\"$PREFIX/$LIBDIR:\$LD_LIBRARY_PATH\""
  echo "  export PATH=\"$PREFIX/bin:\$PATH\""
  echo "  export PKG_CONFIG_PATH=\"$PREFIX/$LIBDIR/pkgconfig:\$PKG_CONFIG_PATH\""
  echo
  echo "then verify with:"
  echo "  env | grep -i quic"
  echo
}

main "$@"
