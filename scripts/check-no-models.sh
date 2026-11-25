#!/usr/bin/env bash
# Voice models are never shipped with Pawl: users download them in Settings (voice_download). This fails if a model
# file or model archive (.onnx, .ort, .bz2, .rknn, tokens.txt, bpe.model) is tracked in git, or is in any folder given
# as an argument. For those folders (built bundles and the installers' staging folders) it checks:
#   - every file name in the tree (a macOS .app, the deb/AppImage staging trees, ...);
#   - the file lists of NSIS (.nsi) and WiX (.wxs) installer scripts (the Windows installers are built from them);
#   - the contents of .deb and .rpm packages (when dpkg-deb / rpm is installed).
# Usage: bash scripts/check-no-models.sh [dir ...]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PATTERN='(\.(onnx|ort|bz2|rknn)|(^|[/\\])tokens\.txt|(^|[/\\])bpe\.model)$'
# The same names inside installer scripts and package listings (any position on a line).
IN_TEXT='(\.(onnx|ort|rknn)|\.tar\.bz2|tokens\.txt|bpe\.model)([^A-Za-z0-9_]|$)'
status=0

fail() {
  echo "error: $1 (voice models are downloaded by users in Settings, never shipped):" >&2
  echo "$2" >&2
  status=1
}

if git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  tracked="$(git -C "$ROOT" ls-files | grep -iE "$PATTERN" || true)"
  [ -z "$tracked" ] || fail "model files are tracked in git" "$tracked"
fi

for dir in "$@"; do
  if [ ! -d "$dir" ]; then
    echo "error: no such directory: $dir" >&2
    status=1
    continue
  fi
  found="$(find "$dir" -type f | grep -iE "$PATTERN" || true)"
  [ -z "$found" ] || fail "model files inside $dir" "$found"

  while IFS= read -r -d '' script; do
    hits="$(grep -iE "$IN_TEXT" "$script" || true)"
    [ -z "$hits" ] || fail "installer script $script installs model files" "$hits"
  done < <(find "$dir" -type f \( -iname '*.nsi' -o -iname '*.wxs' \) -print0)

  while IFS= read -r -d '' pkg; do
    if command -v dpkg-deb >/dev/null 2>&1; then
      hits="$(dpkg-deb -c "$pkg" | grep -iE "$IN_TEXT" || true)"
      [ -z "$hits" ] || fail "package $pkg contains model files" "$hits"
    else
      echo "[check-no-models] dpkg-deb not installed: $pkg not listed (its staging tree is checked)" >&2
    fi
  done < <(find "$dir" -type f -iname '*.deb' -print0)

  while IFS= read -r -d '' pkg; do
    if command -v rpm >/dev/null 2>&1; then
      hits="$(rpm -qlp "$pkg" 2>/dev/null | grep -iE "$IN_TEXT" || true)"
      [ -z "$hits" ] || fail "package $pkg contains model files" "$hits"
    else
      echo "[check-no-models] rpm not installed: $pkg not listed" >&2
    fi
  done < <(find "$dir" -type f -iname '*.rpm' -print0)
done

[ "$status" -eq 0 ] && echo "[check-no-models] ok" >&2
exit "$status"
