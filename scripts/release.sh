#!/usr/bin/env bash
# Build a release for this machine (or for TARGET, a Rust target triple): the `pawl` hub/CLI and the desktop
# installers, with `pawl` bundled next to the desktop app as a Tauri sidecar (the app looks for it there).
# Output: dist/release/ with the installers, a CLI archive and SHA256SUMS-<triple>.txt.
# The desktop app links the sherpa-onnx voice engine; its native archive is fetched and checked against a pinned
# SHA-256 first (scripts/fetch-sherpa.sh). Voice models are never bundled (scripts/check-no-models.sh).
# Usage: bash scripts/release.sh            (host target)
#        TARGET=x86_64-apple-darwin bash scripts/release.sh
# CI runs this on every platform (.github/workflows/release.yml).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
HOST="$(rustc -vV | sed -n 's/^host: //p')"
TRIPLE="${TARGET:-$HOST}"
EXE=""
case "$TRIPLE" in *windows*) EXE=".exe" ;; esac
TARGET_ARGS=()
BIN_DIR="target/release"
BUNDLE_DIR="apps/desktop/src-tauri/target/release/bundle"
if [ -n "${TARGET:-}" ]; then
  TARGET_ARGS=(--target "$TARGET")
  BIN_DIR="target/$TARGET/release"
  BUNDLE_DIR="apps/desktop/src-tauri/target/$TARGET/release/bundle"
fi
OUT="dist/release"
echo "== Pawl $VERSION for $TRIPLE"

cargo build --release -p pawl-hub ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"}
mkdir -p apps/desktop/src-tauri/binaries
cp "$BIN_DIR/pawl$EXE" "apps/desktop/src-tauri/binaries/pawl-$TRIPLE$EXE"
rm -rf "$BUNDLE_DIR"
SHERPA_ONNX_ARCHIVE_DIR="$(bash scripts/fetch-sherpa.sh "$TRIPLE")"
export SHERPA_ONNX_ARCHIVE_DIR
(cd apps/desktop && bun run tauri build --config src-tauri/tauri.release.json ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"})
# The bundles, and the folders the Windows installers are built from (NSIS / WiX scripts and staged files).
CHECK_DIRS=("$BUNDLE_DIR")
for d in "${BUNDLE_DIR%/bundle}/nsis" "${BUNDLE_DIR%/bundle}/wix"; do
  if [ -d "$d" ]; then CHECK_DIRS+=("$d"); fi
done
bash scripts/check-no-models.sh "${CHECK_DIRS[@]}"

rm -rf "$OUT"
mkdir -p "$OUT"
STAGE="$(mktemp -d)"
cp "$BIN_DIR/pawl$EXE" LICENSE README.md "$STAGE/"
case "$TRIPLE" in
  *windows*)
    ZIP="$ROOT/$OUT/pawl-cli-$VERSION-$TRIPLE.zip"
    WIN_STAGE="$(cygpath -w "$STAGE" 2>/dev/null || echo "$STAGE")"
    WIN_ZIP="$(cygpath -w "$ZIP" 2>/dev/null || echo "$ZIP")"
    powershell -NoProfile -Command "Compress-Archive -Force -Path '$WIN_STAGE\\*' -DestinationPath '$WIN_ZIP'"
    cp "$BUNDLE_DIR"/nsis/*.exe "$BUNDLE_DIR"/msi/*.msi "$OUT/"
    ;;
  *apple-darwin)
    tar -czf "$OUT/pawl-cli-$VERSION-$TRIPLE.tar.gz" -C "$STAGE" .
    cp "$BUNDLE_DIR"/dmg/*.dmg "$OUT/"  # named Pawl_<version>_<aarch64|x64>.dmg
    ;;
  *linux*)
    tar -czf "$OUT/pawl-cli-$VERSION-$TRIPLE.tar.gz" -C "$STAGE" .
    cp "$BUNDLE_DIR"/deb/*.deb "$BUNDLE_DIR"/rpm/*.rpm "$BUNDLE_DIR"/appimage/*.AppImage "$OUT/"
    ;;
  *)
    echo "unsupported target $TRIPLE" >&2
    exit 1
    ;;
esac
rm -rf "$STAGE"
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | sed 's/ \*/  /'; }
(cd "$OUT" && for f in *; do case "$f" in SHA256SUMS*) ;; *) sha "$f" ;; esac; done > "SHA256SUMS-$TRIPLE.txt")
ls -l "$OUT"
