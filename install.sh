#!/bin/sh
# Install boba-agent (the Boba Agent server and remote runner) from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/Flotapponnier/boba-agent/main/install.sh | sh
#
# Options (environment):
#   BOBA_VERSION=v0.1.0          a specific release (default: latest)
#   BOBA_INSTALL_DIR=~/.local/bin where to put the binary
#   BOBA_DOWNLOAD_URL=…          base URL holding the assets (mirrors, tests)
set -eu

REPO="Flotapponnier/boba-agent"
VERSION="${BOBA_VERSION:-latest}"
INSTALL_DIR="${BOBA_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "needs $1"; }

need uname
need tar
need mkdir
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "needs curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "needs sha256sum or shasum to verify the download"
fi

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) os_part="apple-darwin" ;;
  Linux) os_part="unknown-linux-musl" ;;
  *) die "unsupported OS: $os (macOS and Linux are supported; on Windows use WSL)" ;;
esac
case "$arch" in
  x86_64 | amd64) arch_part="x86_64" ;;
  arm64 | aarch64) arch_part="aarch64" ;;
  *) die "unsupported CPU: $arch" ;;
esac
# Rosetta: an Intel shell on Apple Silicon still gets the native build.
if [ "$os" = Darwin ] && [ "$arch_part" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch_part="aarch64"
fi
target="$arch_part-$os_part"
asset="boba-agent-$target.tar.gz"

if [ -n "${BOBA_DOWNLOAD_URL:-}" ]; then
  base="${BOBA_DOWNLOAD_URL%/}"
elif [ "$VERSION" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t boba)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading $asset ($VERSION)…"
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"

expected="$(grep " $asset\$" "$tmp/SHA256SUMS" | cut -d' ' -f1 | head -n 1)"
[ -n "$expected" ] || die "$asset is not listed in SHA256SUMS"
actual="$(sha256 "$tmp/$asset")"
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"
say "Checksum OK."

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
# Replace atomically so a running copy is not corrupted.
cp "$tmp/boba-agent-$target/boba-agent" "$INSTALL_DIR/.boba-agent.new"
chmod 755 "$INSTALL_DIR/.boba-agent.new"
mv -f "$INSTALL_DIR/.boba-agent.new" "$INSTALL_DIR/boba-agent"
say "Installed $INSTALL_DIR/boba-agent"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say ""
     say "Add it to your PATH (then open a new terminal):"
     say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.profile" ;;
esac

say ""
say "Next steps to connect your Claude Code to a Boba Agent server:"
if command -v claude >/dev/null 2>&1; then
  say "  1. Claude Code is installed. Make sure it is logged in: claude auth status"
else
  say "  1. Install Claude Code and log in: npm install -g @anthropic-ai/claude-code && claude auth login"
fi
say "  2. In Boba Agent, open the account menu > Connect your Claude Code, and copy the command:"
say "       boba-agent runner --server https://… --token …"
say "  3. To keep it running in the background, see"
say "       https://github.com/$REPO/blob/main/docs/RUNNER.md"
