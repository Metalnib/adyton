#!/bin/sh
# Adyton installer — downloads a prebuilt release binary, verifies its checksum,
# and installs it. Usage:
#   curl -fsSL https://raw.githubusercontent.com/Metalnib/adyton/main/install.sh | sh
# Env: ADYTON_INSTALL_DIR (default ~/.local/bin), ADYTON_VERSION (pin a tag).
set -eu

REPO="Metalnib/adyton"
: "${ADYTON_INSTALL_DIR:=$HOME/.local/bin}"

err() { printf 'adyton install: %s\n' "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# --- target triple (matches the release asset naming, spec §12) ------------
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin) case "$arch" in
      arm64 | aarch64) triple="aarch64-apple-darwin" ;;
      x86_64) triple="x86_64-apple-darwin" ;;
      *) err "unsupported macOS arch: $arch" ;;
    esac ;;
  Linux) case "$arch" in
      x86_64 | amd64) triple="x86_64-unknown-linux-musl" ;;
      arm64 | aarch64) triple="aarch64-unknown-linux-musl" ;;
      *) err "unsupported Linux arch: $arch" ;;
    esac ;;
  *) err "unsupported OS: $os (build from source: cargo build --release)" ;;
esac

# --- downloader (curl or wget; their default User-Agent satisfies GitHub) ---
if have curl; then
  fetch() { curl -fsSL "$1"; }
  fetch_to() { curl -fsSL -o "$2" "$1"; }
elif have wget; then
  fetch() { wget -qO- "$1"; }
  fetch_to() { wget -qO "$2" "$1"; }
else
  err "need curl or wget"
fi

# --- resolve version -------------------------------------------------------
if [ -n "${ADYTON_VERSION:-}" ]; then
  tag="$ADYTON_VERSION"
else
  tag=$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' | head -1 \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
  [ -n "$tag" ] || err "could not resolve the latest release tag"
fi

asset="adyton-$tag-$triple.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

# --- download + verify + extract -------------------------------------------
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

printf 'downloading %s (%s)…\n' "$asset" "$tag" >&2
fetch_to "$base/$asset" "$tmp/$asset" || err "download failed: $asset"
fetch_to "$base/SHA256SUMS.txt" "$tmp/SHA256SUMS.txt" || err "download failed: SHA256SUMS.txt"

expected=$(awk -v a="$asset" '$2 == a { print $1 }' "$tmp/SHA256SUMS.txt")
[ -n "$expected" ] || err "no checksum listed for $asset"
if have shasum; then
  got=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
elif have sha256sum; then
  got=$(sha256sum "$tmp/$asset" | awk '{print $1}')
else
  err "need shasum or sha256sum to verify the download"
fi
[ "$got" = "$expected" ] || err "checksum mismatch (expected $expected, got $got)"

tar -xzf "$tmp/$asset" -C "$tmp" adyton || err "could not extract adyton"

# --- install ---------------------------------------------------------------
mkdir -p "$ADYTON_INSTALL_DIR" || err "cannot create $ADYTON_INSTALL_DIR"
if have install; then
  install -m 755 "$tmp/adyton" "$ADYTON_INSTALL_DIR/adyton" || err "install to $ADYTON_INSTALL_DIR failed"
else
  cp "$tmp/adyton" "$ADYTON_INSTALL_DIR/adyton" && chmod 755 "$ADYTON_INSTALL_DIR/adyton" \
    || err "install to $ADYTON_INSTALL_DIR failed"
fi
printf 'installed adyton %s → %s\n' "$tag" "$ADYTON_INSTALL_DIR/adyton"

# --- next steps ------------------------------------------------------------
case ":$PATH:" in
  *":$ADYTON_INSTALL_DIR:"*) ;;
  *) printf '\nNOTE: %s is not on PATH. Add:\n  export PATH="%s:$PATH"\n' \
       "$ADYTON_INSTALL_DIR" "$ADYTON_INSTALL_DIR" ;;
esac

shell=$(basename "${SHELL:-zsh}")
case "$shell" in zsh | bash | fish) ;; *) shell=zsh ;; esac
printf '\nEnable the shell integration — add to your rc file:\n'
if [ "$shell" = fish ]; then
  printf '  adyton init fish | source\n'
else
  printf '  eval "$(adyton init %s)"\n' "$shell"
fi
printf '\nThen: adyton config set-key <profile>  (see the README to configure a provider)\n'
