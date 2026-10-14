// Node v22 default stack, fresh process for every binary-search observation.
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

export function source(shape, n) {
  switch (shape) {
    case 'flow-seq': return '['.repeat(n) + '0' + ']'.repeat(n);
    case 'flow-map': return '{k:'.repeat(n) + '0' + '}'.repeat(n);
    case 'block-seq': return '- '.repeat(n) + '0';
    case 'block-map': return '? '.repeat(n) + '0\n';
    case 'recovery': return '['.repeat(n) + '0' + ']'.repeat(n - 1) + ', 7]';
    case 'catch-props': return '['.repeat(662) + '&cut !!str ' + '['.repeat(n - 662) + '0' + ']'.repeat(n);
    case 'mixed': {
      // A fixed block prefix followed by alternating flow seq/map keeps the
      // shape at the eventual catch frame stable as total depth increases.
      const blocks = Math.min(n, 2), flows = n - blocks;
      let prefix = '', suffix = '';
      for (let i = 0; i < blocks; i++) prefix += i % 2 ? '? ' : '- ';
      for (let i = 0; i < flows; i++) {
        prefix += i % 2 ? '{k:' : '[';
        suffix = (i % 2 ? '}' : ']') + suffix;
      }
      return prefix + '0' + suffix + '\n';
    }
    // A flat chain of anchored collections, not syntactic nesting. Node's
    // default maxAliasCount fails at 101 links before stack exhaustion.
    case 'alias-chain': return '[&a0 [0], ' + Array.from({ length: n }, (_, i) =>
      `&a${i + 1} [*a${i}]`).join(', ') + ']';
    case 'merge-chain': return '%YAML 1.1\n---\n[&a0 {x: 0}, ' + Array.from({ length: n }, (_, i) =>
      `&a${i + 1} {<<: *a${i}}`).join(', ') + ']';
    default: throw Error(shape);
  }
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
const [baseline, output, shape, n] = process.argv.slice(2);
if (Number(process.versions.node.split('.')[0]) !== 22 || process.execArgv.length)
  throw Error('Use Node v22 with the default stack and no exec flags');
const require = createRequire(import.meta.url);
const yaml = require(resolve(baseline, 'node_modules/yaml'));
if (yaml.parseDocument == null || JSON.parse(readFileSync(resolve(baseline, 'node_modules/yaml/package.json'))).version !== '2.9.0')
  throw Error('Use yaml 2.9.0');
const diagnostic = e => ({ name: e.name, code: e.code ?? null, message: e.message, pos: e.pos ?? null });
if (shape) {
  const doc = yaml.parseDocument(source(shape, Number(n)), { prettyErrors: false });
  const errors = doc.errors.map(diagnostic);
  let conversion = null;
  if (!errors.length && (shape === 'alias-chain' || shape === 'merge-chain')) {
    try { doc.toJSON(); } catch (e) { conversion = diagnostic(e); }
  }
  // Observe the catch replacement without serializing a deeply recursive AST.
  let token = doc.contents, collections = 0;
  while (token?.items) {
    collections++;
    const first = token.items[0];
    token = yaml.isPair(first) ? (first.value ?? first.key) : first;
  }
  console.log(JSON.stringify({ errors, warnings: doc.warnings.map(diagnostic), conversion,
    range: doc.range,
    tail: shape === 'recovery' ? { value: doc.contents.items[1].value, range: doc.contents.items[1].range } : null,
    replacement: token && {
    value: token.value ?? null, range: token.range, anchor: token.anchor ?? null,
    tag: token.tag ?? null, collections
  }}));
} else {
  const observe = (s, depth) => JSON.parse(execFileSync(process.execPath,
    [new URL(import.meta.url).pathname, baseline, '-', s, String(depth)],
    { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 }));
  const fixture = { node: process.version, yaml: '2.9.0', execArgv: process.execArgv,
    method: '5 binary searches; fresh default-stack process per observation; depth = collection count', shapes: {} };
  for (const s of ['flow-seq', 'flow-map', 'block-seq', 'block-map', 'mixed', 'alias-chain']) {
    const runs = [];
    for (let run = 0; run < 5; run++) {
      let lo = 0, hi = s === 'alias-chain' ? 1024 : 8192;
      const exhausted = d => observe(s, d).errors.some(e => e.code === 'RESOURCE_EXHAUSTION');
      if (!exhausted(hi)) {
        runs.push({ firstResourceExhaustion: null, searchedThrough: hi, observation: observe(s, hi) });
        continue;
      }
      while (hi - lo > 1) {
        const mid = Math.floor((hi + lo) / 2);
        if (exhausted(mid)) hi = mid; else lo = mid;
      }
      runs.push({ firstResourceExhaustion: hi, below: observe(s, hi - 1), at: observe(s, hi) });
    }
    const depths = runs.map(r => r.firstResourceExhaustion).filter(d => d != null);
    fixture.shapes[s] = { minimum: depths.length ? Math.min(...depths) : null,
      maximum: depths.length ? Math.max(...depths) : null, runs };
    console.error(s, fixture.shapes[s].minimum, fixture.shapes[s].maximum);
  }
  writeFileSync(output, JSON.stringify(fixture, null, 2) + '\n');
}

}
