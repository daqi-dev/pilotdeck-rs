import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
const [baseline, output] = process.argv.slice(2);
const observations = ['recovery', 'catch-props'].map(shape => ({ shape, depth: 10_000,
  ...JSON.parse(execFileSync(process.execPath,
    [new URL('./record_depth.mjs', import.meta.url).pathname, baseline, '-', shape, '10000'], { encoding: 'utf8' })) }));
writeFileSync(output, JSON.stringify({ node: process.version, yaml: '2.9.0', observations }, null, 2) + '\n');
