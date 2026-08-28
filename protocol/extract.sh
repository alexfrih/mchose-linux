#!/usr/bin/env bash
# Turn the two protocol-bearing chunks into something readable.
#   app.pretty.js    the mouse protocol: X3 (reads), Rat (writes), parsers
#   index.clean.js   the other MCHOSE protocol family, deobfuscated
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here/mhub-bundle"

npx --yes prettier@3 --no-config --parser babel app-chunk.js > app.pretty.js

index=$(ls index-*.js | head -1)
npx --yes prettier@3 --no-config --parser babel "$index" > index.pretty.js
(cd "$here/tools" && npm install --silent)
node "$here/tools/deobf2.mjs" index.pretty.js index.deobf.js
npx --yes prettier@3 --no-config --parser babel index.deobf.js > index.clean.js

echo "readable sources in $here/mhub-bundle"
