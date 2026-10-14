// Offline Node v22 oracle for the pinned baseline's YAML entry point.
// Usage: node --import <baseline>/node_modules/tsx/dist/loader.mjs <this>
//        <baseline> <output.json>
import { readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';

const baseline = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
if (Number(process.versions.node.split('.')[0]) !== 22) throw Error('Use Node v22');
const { parseDocument } = await import(pathToFileURL(join(baseline, 'node_modules/yaml/dist/index.js')).href);
const pkg = JSON.parse(await readFile(join(baseline, 'node_modules/yaml/package.json'), 'utf8'));
const cases: { id: string; yaml: string }[] = [];
const add = (id: string, yaml: string) => cases.push({id, yaml});
add('empty', '');
add('comment-only', '# comment\n');
add('null-root', 'null\n');
add('tilde-root', '~\n');
add('empty-values', 'empty:\nquoted: ""\nsingle: \'\'\nnull: null\ntilde: ~\n');
add('core-scalars', '[null, Null, NULL, nUlL, ~, true, True, TRUE, tRuE, false, False, FALSE, yes, no, on, off, y, n]\n');
add('core-numbers', '[0, -0, +0, 01, 0o17, 0O17, 0xFF, 0XFF, 0b10, 1_000, 1:20, 1.0, .5, 1., 1e3, +1e3, -2E-2, .inf, -.Inf, +.INF, .nan, .NaN, 9007199254740993, 1e999]\n');
add('quoted-scalars', '["null", \'~\', "true", \'01\', "1e999"]\n');
add('block-scalars', 'literal: |\n  true\n  two\nfolded: >-\n  a\n  b\n');
add('duplicate-string', 'a: 1\na: 2\n');
add('duplicate-quoted', 'a: 1\n"a": 2\n');
add('duplicate-number', '1: a\n01: b\n');
add('duplicate-nan', '.nan: a\n.NaN: b\n');
add('duplicate-null', 'null: a\n~: b\n');
add('key-coercion-collision', '1: a\n"1": b\n');
add('complex-key', '? [a, b]\n: value\n');
add('complex-key-duplicate', '? [a, b]\n: one\n? [a, b]\n: two\n');
add('mapping-key', '? {a: 1}\n: value\n');
add('proto-keys', '__proto__: {x: 1}\nconstructor: 2\ntoString: 3\n');
add('numeric-key-order', 'z: 0\n10: ten\n2: two\na: 1\n');
add('alias-scalar', 'a: &x true\nb: *x\n');
add('alias-map', 'a: &x {nested: [a, b]}\nb: *x\n');
add('alias-redefinition', 'a: &x 1\nb: &x 2\nc: *x\n');
add('alias-unknown', 'a: *missing\n');
add('alias-forward', 'a: *later\nb: &later 1\n');
add('alias-cycle', 'a: &x {self: *x}\n');
add('alias-key', 'a: &x key\n*x: value\n');
add('alias-key-duplicate', 'a: &x a\n*x: second\n');
add('merge-core', 'base: &b {x: 1}\nitem: {<<: *b, y: 2}\n');
add('merge-explicit', 'base: &b {x: 1}\nitem: {!!merge <<: *b, y: 2}\n');
add('merge-quoted', 'base: &b {x: 1}\nitem: {"<<": *b, y: 2}\n');
add('merge-invalid-core', 'item: {<<: 7}\n');
add('merge-invalid-explicit', 'item: {!!merge <<: 7}\n');
add('merge-duplicate-core', 'base: &b {x: 1}\nitem: {<<: *b, <<: *b}\n');
add('explicit-str', '[!!str true, !!str 1, !!str null, !!str "", ! true]\n');
add('explicit-int', '[!!int 01, !!int "10", !!int 1_000]\n');
add('explicit-bool-invalid', 'a: !!bool yes\n');
add('explicit-int-invalid', 'a: !!int nope\n');
add('explicit-null-invalid', 'a: !!null nope\n');
add('unknown-tag-plain', 'a: !custom true\n');
add('unknown-tag-quoted', 'a: !custom "true"\n');
add('unknown-tag-map', 'a: !custom {x: 1}\n');
add('tag-seq', 'a: !!seq [1, 2]\n');
add('tag-set', 'a: !!set {x: null, y: null}\n');
add('tag-omap', 'a: !!omap [{x: 1}, {y: 2}]\n');
add('tag-pairs', 'a: !!pairs [{x: 1}, {x: 2}]\n');
add('tag-timestamp', 'a: !!timestamp 2001-12-15T02:59:43.1Z\n');
add('tag-binary', 'a: !!binary SGVsbG8=\n');
add('tag-directive', '%TAG !e! tag:example.com,2020:\n---\na: !e!test true\n');
add('tag-directives-two', '%TAG !e! tag:example.com,2020:\n%TAG !f! tag:example.org,2021:\n---\na: !e!test true\nb: !f!test false\n');
add('version-11', '%YAML 1.1\n---\na: yes\nb: 012\nc: 1:20\nd: 2001-12-15\n');
add('version-12', '%YAML 1.2\n---\na: yes\nb: 012\nc: 1:20\nd: 2001-12-15\n');
add('version-13', '%YAML 1.3\n---\na: yes\n');
add('version-unknown', '%YAML 9.9\n---\na: yes\n');
add('version-duplicate', '%YAML 1.1\n%YAML 1.2\n---\na: yes\n');
add('merge-11', '%YAML 1.1\n---\nbase: &b {x: 1}\nitem: {<<: *b, y: 2}\n');
add('multiple-docs', 'a: 1\n---\nb: 2\n');
add('multiple-docs-empty', '---\n---\n');
add('document-end', 'a: 1\n...\n');
add('invalid-after-first-doc', 'a: 1\n---\n[unterminated\n');
add('unknown-alias-and-duplicate', 'a: *missing\nb: 1\nb: 2\n');
add('unicode-high-surrogate', 'a: "\\uD800"\n');
add('unicode-low-surrogate', 'a: "\\uDC00"\n');
add('unicode-surrogate-pair', 'a: "\\uD83D\\uDE00"\n');
add('unicode-codepoint', 'a: "\\U0001F600"\n');
add('unicode-out-of-range', 'a: "\\U00110000"\n');
add('unicode-nul-escape', 'a: "\\0"\n');
add('empty-flow-entry', '[a,,b]\n');
add('tab-indent', 'a:\n\tb: 1\n');
add('missing-close', 'a: [1, 2\n');
add('alias-expansion-limit', Array.from({length: 8}, (_, i) => `${String.fromCharCode(97+i)}: &x${i} [${i === 0 ? '0, 0' : `*x${i-1}, *x${i-1}`} ]`).join('\n') + '\n');

// A graph representation retains cycles, shared aliases, nonfinite numbers,
// negative zero and JS UTF-16 strings without relying on JSON value conversion.
function graph(value: unknown) {
  const seen = new Map<object, number>();
  const nodes: unknown[] = [];
  function visit(v: any): any {
    if (v === null) return {type: 'null'};
    if (typeof v === 'string') return {type: 'string', json: JSON.stringify(v)};
    if (typeof v === 'number') return {type: 'number', value: Object.is(v, -0) ? '-0' : String(v)};
    if (typeof v !== 'object') return {type: typeof v, value: String(v)};
    if (seen.has(v)) return {ref: seen.get(v)};
    const id = nodes.length;
    seen.set(v, id);
    nodes.push(null);
    const entries = Object.entries(v).map(([k, item]) => [JSON.stringify(k), visit(item)]);
    nodes[id] = {type: Object.prototype.toString.call(v), entries,
      ...(v instanceof Date ? {iso: v.toISOString()} : {}),
      ...(v instanceof Map ? {map: [...v.entries()].map(([k, val]) => [visit(k), visit(val)])} : {}),
      ...(v instanceof Set ? {set: [...v].map(visit)} : {})};
    return {ref: id};
  }
  const root = visit(value);
  return {root, nodes};
}
const diagnostic = (e: any) => ({name: e.name, code: e.code, message: e.message, pos: e.pos});
const results = cases.map(test => {
  try {
    const doc = parseDocument(test.yaml, {prettyErrors: false});
    const errors = doc.errors.map(diagnostic);
    const warnings = doc.warnings.map(diagnostic);
    if (errors.length) return {...test, phase: 'document.errors', errors, warnings};
    try {
      return {...test, phase: 'value', errors, warnings, value: graph(doc.toJSON())};
    } catch (e) {
      return {...test, phase: 'toJSON.throw', errors, warnings, error: diagnostic(e)};
    }
  } catch (e) {
    return {...test, phase: 'parseDocument.throw', error: diagnostic(e)};
  }
});
const baselineCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], {encoding: 'utf8'}).trim();
await writeFile(output, JSON.stringify({baselineCommit, node: process.version, icu: process.versions.icu, yaml: pkg.version, cases: results}, null, 2) + '\n');
console.log(JSON.stringify({node: process.version, yaml: pkg.version, cases: results.length, output}));
