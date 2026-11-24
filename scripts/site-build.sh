#!/usr/bin/env bash
# Assemble the product page (site/) with the pet sprites and screenshots into dist/site.
# GitHub Pages deploys that folder (.github/workflows/pages.yml). Preview locally with:
#   bash scripts/site-build.sh && python -m http.server 4321 -d dist/site
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/dist/site"
cd "$ROOT"
rm -rf "${OUT:?}"
mkdir -p "$OUT/shots"
cp -R site/. "$OUT/"
for s in dog cat bird robot; do
  mkdir -p "$OUT/pets/$s"
  cp "assets/pets/$s/"*.png "$OUT/pets/$s/"
done
cp docs/screenshots/pet-strip.png docs/screenshots/panel-approvals.png "$OUT/shots/"
touch "$OUT/.nojekyll"
echo "site ready in dist/site"
