#!/usr/bin/env sh
# Package a built boba-agent binary as dist/boba-agent-<target>.tar.gz (+ .sha256).
# Usage: scripts/package.sh <target-triple> [path/to/boba-agent]
# Asset names carry no version so `releases/latest/download/<asset>` always works.
set -eu
target="$1"
bin="${2:-target/$target/release/boba-agent}"
[ -x "$bin" ] || { echo "no executable at $bin" >&2; exit 1; }
name="boba-agent-$target"
stage="dist/$name"
rm -rf "$stage" && mkdir -p "$stage"
cp "$bin" "$stage/boba-agent"
cp LICENSE README.md "$stage/"
[ -f docs/RUNNER.md ] && cp docs/RUNNER.md "$stage/"
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "$stage"
if command -v sha256sum >/dev/null 2>&1; then
  (cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
else
  (cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
fi
cat "dist/$name.tar.gz.sha256"
