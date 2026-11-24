#!/usr/bin/env bash
# Serialize bun installs across parallel agents (mkdir is atomic).
# Usage (from anywhere):
#   bash scripts/bun-locked.sh install
#   bash scripts/bun-locked.sh add zod --cwd packages/hub-client
#   bash scripts/bun-locked.sh add -d vitest --cwd apps/vscode
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCK="$ROOT/.locks/bun"
mkdir -p "$ROOT/.locks"
for i in $(seq 1 360); do
  if mkdir "$LOCK" 2>/dev/null; then break; fi
  if [ -d "$LOCK" ] && [ -n "$(find "$LOCK" -maxdepth 0 -mmin +15 2>/dev/null)" ]; then rmdir "$LOCK" 2>/dev/null || true; fi
  sleep 3
done
trap 'rmdir "$LOCK" 2>/dev/null || true' EXIT
cd "$ROOT"
bun "$@"
