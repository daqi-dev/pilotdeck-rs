// Deterministic extraction of captured originals; does not re-execute TS.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { validate, validateFiles } from './contract.mjs';

assert.equal(process.version, 'v22.22.0', '#1145 generator Node version');
assert.equal(process.argv.length, 3, '#1145 generator OUTPUT');
const here = dirname(fileURLToPath(import.meta.url));
const bytes = readFileSync(join(here, 'source.json'));
const source = JSON.parse(bytes);
const definitions = JSON.parse(readFileSync(join(here, 'cases.json'), 'utf8'));
validate(source, definitions);
validateFiles(source, here, process.env.PDRS_BASELINE);
const oracle = { sourceSha256: createHash('sha256').update(bytes).digest('hex'),
  sourceCommit: source.sourceCommit, nodeVersion: source.nodeVersion, kind: source.kind,
  normalization: 'none; raw capture including stderr bootstrap warnings retained',
  matrixCells: definitions.length,
  gaps: source.gaps.map(({ id, status, reason }) => ({ id, status, reason })),
  cases: source.captures };
const output = resolve(process.argv[2]);
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(`#1145 extracted ${oracle.cases.length} real TS signal observations`);
