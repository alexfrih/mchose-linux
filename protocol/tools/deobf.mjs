// Resolve obfuscator.io string-array lookups in the M HUB bundle.
// Pattern per group: array fn + decoder fn + rotation IIFE, then `const alias = decoder`.
import { readFileSync, writeFileSync } from "node:fs";
import vm from "node:vm";

const src = readFileSync(process.argv[2], "utf8");

// 1. array providers: function NAME() { const S = [...]; return (NAME = function () { return S; })(); }
const arrayRe = /function (\w+)\(\) \{\n  const S = \[[\s\S]*?\];\n  return \(\1 = function \(\) \{\n    return S;\n  \}\)\(\);\n\}/g;
const arrays = [...src.matchAll(arrayRe)].map((m) => ({ name: m[1], start: m.index, end: m.index + m[0].length, text: m[0] }));

// 2. decoders: function NAME(a, b) { const Z = ARRAYFN(); return (NAME = function (...) {...})(a, b); }
const decRe = /function (\w+)\((\w+), (\w+)\) \{\n  const (\w+) = (\w+)\(\);\n  return \(\1 = function [\s\S]*?\n  \}\)\(\2, \3\);\n\}/g;
const decoders = [...src.matchAll(decRe)].map((m) => ({ name: m[1], arrayFn: m[5], start: m.index, end: m.index + m[0].length, text: m[0] }));

// 3. rotation IIFEs
const iifeRe = /\(function \(\) \{\n  const S = (\w+),\n    M = (\w+)\(\);\n  for \(;;\)[\s\S]*?\n\}\)\(\);/g;
const iifes = [...src.matchAll(iifeRe)].map((m) => ({ decoder: m[1], arrayFn: m[2], start: m.index, end: m.index + m[0].length, text: m[0] }));

// 4. aliases: const X = Y;  where Y is a decoder name
const aliasRe = /\bconst (\w+) = (\w+);/g;
const aliases = [...src.matchAll(aliasRe)].map((m) => ({ from: m[1], to: m[2] }));

const resolved = new Map(); // decoderName -> function
for (const iife of iifes) {
  const dec = decoders.find((d) => d.name === iife.decoder);
  const arr = arrays.find((a) => a.name === iife.arrayFn);
  if (!dec || !arr) { console.error(`skip iife: ${iife.decoder}/${iife.arrayFn}`); continue; }
  const code = `${arr.text}\n${dec.text}\n${iife.text}\n__out = ${dec.name};`;
  const ctx = { __out: null, console: { log() {} } };
  try {
    vm.createContext(ctx);
    vm.runInContext(code, ctx, { timeout: 10000 });
    if (typeof ctx.__out === "function") resolved.set(dec.name, ctx.__out);
  } catch (e) { console.error(`eval failed for ${dec.name}: ${e.message}`); }
}

// map aliases onto resolved decoders
const table = new Map(resolved);
for (let pass = 0; pass < 5; pass++) {
  for (const a of aliases) if (table.has(a.to) && !table.has(a.from)) table.set(a.from, table.get(a.to));
}
console.error(`resolved decoders: ${[...table.keys()].join(", ")}`);

// 5. substitute NAME(1234) -> "string"
let hits = 0, misses = 0;
const names = [...table.keys()].sort((a, b) => b.length - a.length);
const callRe = new RegExp(`\\b(${names.join("|")})\\((\\d+)\\)`, "g");
const out = src.replace(callRe, (whole, name, num) => {
  try {
    const v = table.get(name)(Number(num));
    if (typeof v === "string") { hits++; return JSON.stringify(v); }
  } catch {}
  misses++; return whole;
});
console.error(`substituted ${hits}, unresolved ${misses}`);
writeFileSync(process.argv[3], out);
