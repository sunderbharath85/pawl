#!/usr/bin/env bash
# Fetch the prebuilt sherpa-onnx static library archive that the `sherpa-onnx-sys` build script links into the
# desktop app, check it against a pinned SHA-256, and point the build at it.
#
# Without this, sherpa-onnx-sys downloads the archive (~25-128 MB of native code) itself, with no checksum.
# With SHERPA_ONNX_ARCHIVE_DIR set it copies the archive from that directory instead, and
# apps/desktop/src-tauri/.cargo/config.toml sets that variable for every build, so a build without a verified
# archive fails ("SHERPA_ONNX_ARCHIVE_DIR does not contain expected archive") instead of downloading one.
#
# Usage: bash scripts/fetch-sherpa.sh [target-triple]     (default: the host triple, or $TARGET)
#   Prints the archive directory on stdout (everything else goes to stderr). In GitHub Actions it also appends
#   SHERPA_ONNX_ARCHIVE_DIR to $GITHUB_ENV. An existing SHERPA_ONNX_ARCHIVE_DIR is used as the directory.
#
# Bumping sherpa-onnx: download the new archives, check them against the release's asset digests
# (gh api repos/k2-fsa/sherpa-onnx/releases/tags/v<version> --jq '.assets[] | [.name, .digest] | @tsv'),
# then update VERSION and the hashes below in the same change as Cargo.lock.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAURI="$ROOT/apps/desktop/src-tauri"

VERSION="2.0.3"
# Static archives (the sherpa-onnx crate's default "static" feature), verified 2026-11-24 against GitHub's
# asset digests for https://github.com/k2-fsa/sherpa-onnx/releases/tag/v2.0.3
pinned_sha256() {
  case "$1" in
    "sherpa-onnx-v$VERSION-win-x64-static-MT-Release-lib.tar.bz2") echo 2dfaeacf899e8d21584caab54a55933b481b5dccbb3615e92daecc6f0618e781 ;;
    "sherpa-onnx-v$VERSION-osx-x64-static-lib.tar.bz2") echo aa2669f175bc3b496788308e211e1bbb792ca7ba654f79db69bf20bef1d597a0 ;;
    "sherpa-onnx-v$VERSION-osx-arm64-static-lib.tar.bz2") echo c86468ad8f79b679550f0865cc1a67747f58b8cc8780623ed7d8367fd4b541a3 ;;
    "sherpa-onnx-v$VERSION-linux-x64-static-lib.tar.bz2") echo f927e695d29b8c7ea3fda97eed215204b210fd57f9faf990a93377bac098ad57 ;;
    *) return 1 ;;
  esac
}

log() { echo "[fetch-sherpa] $*" >&2; }
die() { log "error: $*"; exit 1; }

# The version Cargo.lock resolved must be the one pinned here.
LOCKED="$(awk '/^name = "sherpa-onnx-sys"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$TAURI/Cargo.lock")"
[ -n "$LOCKED" ] || die "sherpa-onnx-sys not found in apps/desktop/src-tauri/Cargo.lock"
[ "$LOCKED" = "$VERSION" ] || die "Cargo.lock has sherpa-onnx-sys $LOCKED but this script pins $VERSION: update the pins"

TRIPLE="${1:-${TARGET:-}}"
if [ -z "$TRIPLE" ]; then
  TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
fi
case "$TRIPLE" in
  x86_64-pc-windows-msvc) PLATFORM="win-x64-static-MT-Release-lib" ;;
  x86_64-apple-darwin) PLATFORM="osx-x64-static-lib" ;;
  aarch64-apple-darwin) PLATFORM="osx-arm64-static-lib" ;;
  x86_64-unknown-linux-gnu) PLATFORM="linux-x64-static-lib" ;;
  *) die "no pinned sherpa-onnx archive for $TRIPLE (add one: download it, check its digest, pin it here)" ;;
esac
ARCHIVE="sherpa-onnx-v$VERSION-$PLATFORM.tar.bz2"
WANT="$(pinned_sha256 "$ARCHIVE")" || die "no pinned hash for $ARCHIVE"
URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/v$VERSION/$ARCHIVE"

DIR="${SHERPA_ONNX_ARCHIVE_DIR:-$TAURI/target/sherpa-onnx-archives}"
mkdir -p "$DIR"
DIR="$(cd "$DIR" && pwd)"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{ print $1 }'
}

DEST="$DIR/$ARCHIVE"
if [ -f "$DEST" ] && [ "$(sha256_of "$DEST")" = "$WANT" ]; then
  log "$ARCHIVE already present and verified"
else
  rm -f "$DEST"
  TMP="$DEST.part.$$"
  trap 'rm -f "$TMP"' EXIT
  log "downloading $URL"
  curl --fail --location --silent --show-error --retry 3 --proto '=https' --tlsv1.2 -o "$TMP" "$URL"
  GOT="$(sha256_of "$TMP")"
  if [ "$GOT" != "$WANT" ]; then
    die "SHA-256 mismatch for $ARCHIVE: expected $WANT, got $GOT (the file was deleted)"
  fi
  mv -f "$TMP" "$DEST"
  trap - EXIT
  log "verified $ARCHIVE ($WANT)"
fi

# Windows paths for cargo when running under Git Bash / MSYS.
OUT="$DIR"
if command -v cygpath >/dev/null 2>&1; then
  OUT="$(cygpath -m "$DIR")"
fi
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "SHERPA_ONNX_ARCHIVE_DIR=$OUT" >> "$GITHUB_ENV"
fi
echo "$OUT"
