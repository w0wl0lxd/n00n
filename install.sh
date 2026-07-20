#!/usr/bin/env bash
set -euo pipefail

REPO="w0wl0lxd/noon"
INSTALL_DIR="${NOON_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
Linux) target_os="unknown-linux-musl" ;;
Darwin) target_os="apple-darwin" ;;
*)
  echo "unsupported OS: $os" >&2
  exit 1
  ;;
esac
case "$arch" in
x86_64 | amd64) target_arch="x86_64" ;;
aarch64 | arm64) target_arch="aarch64" ;;
*)
  echo "unsupported arch: $arch" >&2
  exit 1
  ;;
esac
target="${target_arch}-${target_os}"

tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" |
  grep -m1 '"tag_name"' |
  sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
asset="noon-${tag}-${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/${tag}/${asset}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading ${url}"
curl -fSL "$url" -o "$tmp/$asset"
tar -xzf "$tmp/$asset" -C "$tmp"

mkdir -p "$INSTALL_DIR"
install -m 0755 "$tmp/noon" "$INSTALL_DIR/noon"
echo "Installed noon to ${INSTALL_DIR}/noon"
