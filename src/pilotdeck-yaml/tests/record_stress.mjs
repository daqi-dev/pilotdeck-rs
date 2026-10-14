// Each stress observation uses a fresh Node v22 process and its default stack.
import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
const [baseline, output] = process.argv.slice(2);
if (Number(process.versions.node.split('.')[0]) !== 22 || process.execArgv.length) throw Error('Use default Node v22');
const observations = [];
for (const shape of ['flow-seq', 'flow-map', 'block-seq', 'block-map', 'mixed', 'alias-chain']) {
  for (const depth of [10_000, 100_000, 1_000_000]) {
    const observation = JSON.parse(execFileSync(process.execPath,
      [new URL('./record_depth.mjs', import.meta.url).pathname, baseline, '-', shape, String(depth)],
      { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 }));
    observations.push({ shape, depth, ...observation });
    console.error(shape, depth, observation.errors.map(e => e.code), observation.conversion?.message);
    // Preserve completed evidence even if a later Node probe cannot complete.
    writeFileSync(output, JSON.stringify({ node: process.version, yaml: '2.9.0', execArgv: [], observations }, null, 2) + '\n');
  }
}
