# Where the protocol came from

MCHOSE's "M HUB" driver is a Vue SPA at `mchose.com.cn` that talks to the mouse
over WebHID. The protocol is therefore in its JavaScript, obfuscated but not
encrypted.

- `fetch.sh` downloads the bundle and regenerates everything below.
- `extract.sh` prettifies and deobfuscates it.
- `tools/deobf2.mjs` resolves obfuscator.io string-array lookups scope-aware,
  via Babel: it evaluates each (array function + decoder + rotation IIFE) group
  in a VM to rebuild the decoder, then walks the AST replacing every call whose
  binding resolves to one of them. 1502 of 1874 lookups on the index chunk.
  `tools/deobf.mjs` is the earlier text-substitution version, kept because it
  is easier to read.

The protocol is spread over two chunks, and they are two different protocols:

- `app-chunk.js` (shipped as `purify.es-*.js`, which is a misnomer) holds the
  one the L7 Pro uses: `X3` is the read command table, `Rat` the write table,
  and the `binary-parser` schemas next to them define every field. This is what
  `../PROTOCOL.md` documents.
- `index-*.js` holds a second family, for the MCHOSE devices with a screen:
  templated frames with a sum checksum and a `dpi/50 - 1` encoding. Documented
  here only so the next reader does not mistake one for the other, which is
  easy to do since both live behind the same UI.

There is a third protocol, and it does not live in this bundle at all: the G3
family (G3 V2, G3 V2 Pro, G3 A, A7e) is driven by a separate iframe app at
`mchose.com.cn/gmouse` (Next.js). `mhub-bundle/gmouse/` has its chunks; the
transport and protocol classes are in `page.pretty.js`, the per-model configs
(DPI range, rate lists, sensor-flag bits) in dynamically loaded chunks whose
hashes come from the webpack runtime chunk. It is the 64-byte-frame protocol
documented in `../PROTOCOL.md` under "The G3 family".

Generated files (`*.pretty.js`, `*.clean.js`, `*.deobf.js`, `node_modules`) are
not tracked; run `extract.sh` to get them back.
