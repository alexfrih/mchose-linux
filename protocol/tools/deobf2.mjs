// Scope-aware resolution of obfuscator.io string-array lookups.
// Builds the decoder table by evaluating each (array fn + decoder fn + rotation IIFE) group,
// then walks the AST replacing every call to a binding that resolves to one of those decoders.
import { readFileSync, writeFileSync } from "node:fs";
import vm from "node:vm";
import { parse } from "@babel/parser";
import _traverse from "@babel/traverse";
import _generate from "@babel/generator";
const traverse = _traverse.default ?? _traverse;
const generate = _generate.default ?? _generate;

const src = readFileSync(process.argv[2], "utf8");

const arrayRe = /function (\w+)\(\) \{\n  const S = \[[\s\S]*?\];\n  return \(\1 = function \(\) \{\n    return S;\n  \}\)\(\);\n\}/g;
const decRe = /function (\w+)\((\w+), (\w+)\) \{\n  const (\w+) = (\w+)\(\);\n  return \(\1 = function [\s\S]*?\n  \}\)\(\2, \3\);\n\}/g;
const iifeRe = /\(function \(\) \{\n  const S = (\w+),\n    M = (\w+)\(\);\n  for \(;;\)[\s\S]*?\n\}\)\(\);/g;

const arrays = [...src.matchAll(arrayRe)].map((m) => ({ name: m[1], text: m[0] }));
const decoders = [...src.matchAll(decRe)].map((m) => ({ name: m[1], arrayFn: m[5], text: m[0] }));
const iifes = [...src.matchAll(iifeRe)].map((m) => ({ decoder: m[1], arrayFn: m[2], text: m[0] }));

const table = new Map(); // root decoder name -> fn
for (const iife of iifes) {
  const dec = decoders.find((d) => d.name === iife.decoder);
  const arr = arrays.find((a) => a.name === iife.arrayFn);
  if (!dec || !arr) continue;
  const ctx = { __out: null };
  vm.createContext(ctx);
  try {
    vm.runInContext(`${arr.text}\n${dec.text}\n${iife.text}\n__out = ${dec.name};`, ctx, { timeout: 10000 });
    if (typeof ctx.__out === "function") table.set(dec.name, ctx.__out);
  } catch (e) { console.error(`eval ${dec.name}: ${e.message}`); }
}
console.error(`root decoders: ${[...table.keys()].join(", ")}`);

const ast = parse(src, { sourceType: "module", errorRecovery: true });

// Resolve a binding name in scope to a decoder fn, following `const a = b` alias chains.
function decoderFor(name, scope) {
  for (let depth = 0; depth < 12; depth++) {
    if (table.has(name)) {
      const binding = scope?.getBinding(name);
      // a root decoder is a top-level function declaration; a local alias shadows it
      if (!binding || binding.path.isFunctionDeclaration()) return table.get(name);
    }
    const binding = scope?.getBinding(name);
    if (!binding) return table.has(name) ? table.get(name) : null;
    if (binding.path.isFunctionDeclaration()) return table.get(binding.path.node.id.name) ?? null;
    const init = binding.path.isVariableDeclarator() ? binding.path.node.init : null;
    if (!init || init.type !== "Identifier") return null;
    name = init.name;
    scope = binding.scope.parent ?? binding.scope;
  }
  return null;
}

let hits = 0, misses = 0;
traverse(ast, {
  CallExpression(path) {
    const { callee, arguments: args } = path.node;
    if (callee.type !== "Identifier") return;
    if (args.length !== 1 || args[0].type !== "NumericLiteral") return;
    const fn = decoderFor(callee.name, path.scope);
    if (!fn) { misses++; return; }
    let v;
    try { v = fn(args[0].value); } catch { misses++; return; }
    if (typeof v !== "string") { misses++; return; }
    path.replaceWith({ type: "StringLiteral", value: v });
    hits++;
  },
});
console.error(`substituted ${hits}, skipped ${misses}`);
writeFileSync(process.argv[3], generate(ast, { comments: true, jsescOption: { minimal: true } }).code);
