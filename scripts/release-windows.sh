#!/usr/bin/env bash
# Build the Windows x64 release: the `pawl` CLI/hub and the desktop installers (which bundle pawl.exe
# next to the app, see apps/desktop/src-tauri/tauri.windows.conf.json). Output goes to dist/release/.
# Usage: bash scripts/release-windows.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
OUT="dist/release"
TRIPLE="x86_64-pc-windows-msvc"

cargo build --release -p pawl-hub
(cd apps/desktop && bun run tauri:build)

rm -rf "$OUT" && mkdir -p "$OUT/cli"
cp target/release/pawl.exe LICENSE README.md "$OUT/cli/"
(cd "$OUT/cli" && powershell -NoProfile -Command \
  "Compress-Archive -Force -Path pawl.exe,LICENSE,README.md -DestinationPath ../pawl-cli-$VERSION-$TRIPLE.zip")
rm -rf "$OUT/cli"
BUNDLE="apps/desktop/src-tauri/target/release/bundle"
cp "$BUNDLE"/nsis/*.exe "$BUNDLE"/msi/*.msi "$OUT/"
(cd "$OUT" && sha256sum -- * > SHA256SUMS.txt)
ls -l "$OUT"
