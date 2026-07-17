#!/bin/sh
set -eu

REPOSITORY=${GROK_BUILD_PROXY_REPOSITORY:-${GITHUB_REPOSITORY:-bengHak/grok-build-proxy}}
INSTALL_DIR=${GROK_BUILD_PROXY_INSTALL_DIR:-${INSTALL_DIR:-"$HOME/.local/bin"}}
REQUESTED_VERSION=${GROK_BUILD_PROXY_VERSION:-}
FROM_SOURCE=${GROK_BUILD_PROXY_INSTALL_FROM_SOURCE:-0}
BINARY_NAME=grok-build-proxy

usage() {
  cat <<'USAGE'
Install grok-build-proxy for macOS.

Usage:
  install.sh [--version VERSION] [--install-dir DIRECTORY] [--from-source]

Options:
  --version VERSION       Release tag or source ref (for example v0.1.0)
  --install-dir DIRECTORY Installation directory (default: ~/.local/bin)
  --from-source           Skip release assets and build from source with Cargo
  -h, --help              Show this help

Environment variables:
  GROK_BUILD_PROXY_VERSION
  GROK_BUILD_PROXY_INSTALL_DIR
  GROK_BUILD_PROXY_INSTALL_FROM_SOURCE=1
  GROK_BUILD_PROXY_REPOSITORY=owner/repository
USAGE
}

say() {
  printf '%s\n' "grok-build-proxy installer: $*"
}

fail() {
  printf '%s\n' "grok-build-proxy installer: error: $*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || fail "--version requires a value"
      REQUESTED_VERSION=$2
      shift 2
      ;;
    --install-dir)
      [ "$#" -ge 2 ] || fail "--install-dir requires a value"
      INSTALL_DIR=$2
      shift 2
      ;;
    --from-source)
      FROM_SOURCE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"
command -v shasum >/dev/null 2>&1 || fail "shasum is required"

OS=$(uname -s)
[ "$OS" = "Darwin" ] || fail "macOS is the only supported operating system (found $OS)"

case $(uname -m) in
  arm64|aarch64)
    ARCH=arm64
    ;;
  x86_64|amd64)
    ARCH=amd64
    ;;
  *)
    fail "unsupported Mac architecture: $(uname -m)"
    ;;
esac

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/grok-build-proxy.XXXXXX")
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT HUP INT TERM

resolve_latest_version() {
  effective_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
    "https://github.com/$REPOSITORY/releases/latest" 2>/dev/null || true)
  candidate=${effective_url##*/}
  case "$candidate" in
    v[0-9]*|[0-9]*)
      printf '%s\n' "$candidate"
      ;;
    *)
      return 1
      ;;
  esac
}

normalize_release_version() {
  case "$1" in
    [0-9]*) printf 'v%s\n' "$1" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

install_executable() {
  source_path=$1
  mkdir -p "$INSTALL_DIR" || fail "cannot create $INSTALL_DIR"
  destination="$INSTALL_DIR/$BINARY_NAME"
  install -m 0755 "$source_path" "$destination" || fail "cannot install to $destination"
  say "installed $destination"

  case ":${PATH:-}:" in
    *":$INSTALL_DIR:"*)
      ;;
    *)
      say "add $INSTALL_DIR to PATH, for example:"
      printf '%s\n' "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc"
      ;;
  esac
}

install_binary() {
  version=$(normalize_release_version "$1")
  asset="grok-build-proxy_Darwin_${ARCH}.tar.gz"
  base_url="https://github.com/$REPOSITORY/releases/download/$version"
  archive="$TMP_DIR/$asset"
  checksums="$TMP_DIR/checksums.txt"

  say "downloading $version for macOS/$ARCH"
  curl -fL --retry 3 --retry-delay 1 "$base_url/$asset" -o "$archive" || return 1
  curl -fL --retry 3 --retry-delay 1 "$base_url/checksums.txt" -o "$checksums" || return 1

  expected=$(awk -v file="$asset" '$2 == file { print $1; exit }' "$checksums")
  [ -n "$expected" ] || fail "checksum for $asset is missing"
  actual=$(shasum -a 256 "$archive" | awk '{ print $1 }')
  [ "$actual" = "$expected" ] || fail "checksum verification failed for $asset"

  mkdir -p "$TMP_DIR/extract"
  tar -xzf "$archive" -C "$TMP_DIR/extract"
  [ -f "$TMP_DIR/extract/$BINARY_NAME" ] || fail "release archive does not contain $BINARY_NAME"
  install_executable "$TMP_DIR/extract/$BINARY_NAME"
}

check_rust_toolchain() {
  command -v cargo >/dev/null 2>&1 || fail \
    "no compatible release asset was found and the Rust toolchain is not installed"
  command -v rustc >/dev/null 2>&1 || fail "rustc is required for a source install"
}

install_from_source() {
  ref=$1
  check_rust_toolchain
  say "building $ref from source with $(rustc --version)"

  archive="$TMP_DIR/source.tar.gz"
  source_url="https://github.com/$REPOSITORY/archive/refs/heads/$ref.tar.gz"
  case "$ref" in
    v[0-9]*|[0-9]*)
      tag=$(normalize_release_version "$ref")
      source_url="https://github.com/$REPOSITORY/archive/refs/tags/$tag.tar.gz"
      ref=$tag
      ;;
  esac

  curl -fL --retry 3 --retry-delay 1 "$source_url" -o "$archive" \
    || fail "failed to download source ref $ref"
  mkdir -p "$TMP_DIR/source"
  tar -xzf "$archive" -C "$TMP_DIR/source"
  set -- "$TMP_DIR/source"/*
  source_dir=$1
  [ -d "$source_dir" ] || fail "source archive is empty"

  case "$ARCH" in
    arm64) target=aarch64-apple-darwin ;;
    amd64) target=x86_64-apple-darwin ;;
  esac
  command -v rustup >/dev/null 2>&1 && rustup target add "$target" >/dev/null
  (
    cd "$source_dir"
    cargo build --locked --release --target "$target"
    cp "target/$target/release/$BINARY_NAME" "$TMP_DIR/$BINARY_NAME"
  )
  install_executable "$TMP_DIR/$BINARY_NAME"
}

if [ "$FROM_SOURCE" != "1" ]; then
  version=$REQUESTED_VERSION
  if [ -z "$version" ]; then
    version=$(resolve_latest_version || true)
  fi
  if [ -n "$version" ]; then
    if install_binary "$version"; then
      exit 0
    fi
    if [ -n "$REQUESTED_VERSION" ]; then
      say "release asset unavailable; falling back to source ref $REQUESTED_VERSION"
      install_from_source "$REQUESTED_VERSION"
      exit 0
    fi
    say "latest release asset unavailable; falling back to main"
  else
    say "no GitHub release found; falling back to main"
  fi
fi

source_ref=${REQUESTED_VERSION:-main}
install_from_source "$source_ref"
