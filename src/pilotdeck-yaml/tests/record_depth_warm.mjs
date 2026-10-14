// Secondary evidence for JIT-dependent gray intervals. This does not replace
// the five fresh-process lower-bound measurements used for Rust constants.
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { writeFileSync } from 'node:fs';
import { source } from './record_depth.mjs';
const [baseline, output] = process.argv.slice(2);
if (Number(process.versions.node.split('.')[0]) !== 22 || process.execArgv.length) throw Error('Use default Node v22');
const { parseDocument } = createRequire(import.meta.url)(resolve(baseline, 'node_modules/yaml'));
const fixture = { node: process.version, yaml: '2.9.0', execArgv: [],
  method: '32 warmup parses at depth 512 per shape, then 5 binary searches in the same default-stack process', shapes: {} };
for (const shape of ['flow-seq', 'flow-map', 'block-seq', 'block-map', 'mixed']) {
  for (let i = 0; i < 32; i++) parseDocument(source(shape, 512), { prettyErrors: false });
  const runs = [];
  for (let run = 0; run < 5; run++) {
    let lo = 0, hi = 8192;
    while (hi - lo > 1) {
      const mid = Math.floor((hi + lo) / 2);
      const doc = parseDocument(source(shape, mid), { prettyErrors: false });
      if (doc.errors.some(e => e.code === 'RESOURCE_EXHAUSTION')) hi = mid;
      else lo = mid;
    }
    runs.push(hi);
  }
  fixture.shapes[shape] = { minimum: Math.min(...runs), maximum: Math.max(...runs), runs };
  console.error(shape, runs);
}
writeFileSync(output, JSON.stringify(fixture, null, 2) + '\n');
