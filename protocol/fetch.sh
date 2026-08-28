#!/usr/bin/env bash
# Re-fetch the M HUB web driver bundle and regenerate the readable sources.
# The two chunks below carry the whole HID protocol; the rest of the app is UI.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/mhub-bundle"
base="https://www.mchose.com.cn"

mkdir -p "$out"
# The hashed names change on every deploy. Read them out of index.html.
curl -fsSL "$base/" -o "$out/index.html"
grep -o '/assets/[A-Za-z0-9_.-]*\.js' "$out/index.html" | sort -u | while read -r asset; do
  curl -fsSL "$base$asset" -o "$out/$(basename "$asset")"
done
# purify.es-*.js is the app chunk despite the name: X3, Rat, the parsers.
cp "$out"/purify.es-*.js "$out/app-chunk.js"
"$here/extract.sh"
