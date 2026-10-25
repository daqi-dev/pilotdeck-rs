import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import {sources, dependencyFingerprint, validate} from './observations.mts';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const dependency = dependencyFingerprint(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const output = resolve(process.argv[2]);
await mkdir(output, {recursive: true});
let failure: any;
const {stdout, stderr} = await promisify(execFile)(process.execPath, [
  '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
  fileURLToPath(new URL('./probe.mts', import.meta.url)),
], {env: {...process.env, PDRS_BASELINE: baseline}, timeout: 180000, maxBuffer: 16 * 1024 * 1024}).catch(error => {failure = error; return error;});
// Raw absolute timer evidence is separate from deterministic semantic goldens.
await writeFile(join(output, 'raw.ndjson'), stdout, {flag: 'wx'});
await writeFile(join(output, 'probe.stderr.log'), stderr, {flag: 'wx'});
if (failure) throw new Error(`probe failed; raw evidence retained in ${output}: ${failure.message.split('\n')[0]}`);
const observations = stdout.trim().split('\n').map(line => JSON.parse(line));
const cases = validate(observations, sourceSha256, dependency);
assert.deepEqual(dependencyFingerprint(baseline), dependency, 'dependency changed during capture');
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await writeFile(join(output, 'oracle.json'), JSON.stringify({sourceCommit, sourceSha256, node: process.version, dependencies: [dependency], cases}, null, 2) + '\n', {flag: 'wx'});
console.log(`recorded ${cases.length} network timing cells; raw evidence: ${output}/raw.ndjson`);
