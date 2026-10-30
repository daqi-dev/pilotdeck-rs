// Real TS generation twice with the generator's own reproducible snapshot clock.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, readdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';

assert.ok(process.argv[2], 'provide a fresh output directory');
const output = resolve(process.argv[2]);
await mkdir(output); // Refuse to reuse old evidence.
const argv = [...process.argv];
try {
  // A single real process gives both independent generations the same live pid.
  for (const pass of ['first', 'second']) {
    process.argv = [argv[0], argv[1], join(output, pass)];
    await import(new URL(`./generate.mts?verify=${pass}`, import.meta.url).href);
  }
} finally {
  process.argv = argv;
}
const files = (await readdir(join(output, 'first'))).sort();
assert.deepEqual((await readdir(join(output, 'second'))).sort(), files);
const manifest: string[] = [];
for (const file of files) {
  const first = await readFile(join(output, 'first', file));
  assert.deepEqual(await readFile(join(output, 'second', file)), first, `${file}: nondeterministic bytes`);
  const hash = createHash('sha256').update(first).digest('hex');
  manifest.push(`${hash}  ${file}\n`);
  if (file === 'oracle.json') console.log(`oracle.json SHA-256 (both runs): ${hash}`);
}
console.log(`fixture manifest SHA-256 (both runs): ${createHash('sha256').update(manifest.join('')).digest('hex')}`);
console.log(`PASS: ${files.length} real TS fixture files byte-identical across two generations`);
