// Real TS client → MITM → real TS sidecar → loopback mock, never a Rust oracle.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#969 generate: requires Node v22.22.0');
assert.equal(process.env.LIVE_E2E_TEST_NO_ABORT, undefined, '#969 generate: negative hook must be absent');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const fixture = dirname(fileURLToPath(import.meta.url));
const output = resolve(process.argv[2]);
mkdirSync(output, {recursive:true});
mkdirSync(join(output, 'expected'), {recursive:true});
const capture = spawnSync(process.env.LIVE_E2E_PYTHON ?? 'python3',
  ['-B', join(fixture, 'capture.py'), join(output, 'capture')],
  {encoding:'utf8', timeout:900_000, env:process.env, maxBuffer:8 * 1024 * 1024});
assert.equal(capture.status, 0, `#969 generate: capture failed: ${capture.error ?? ''}\n${capture.stdout}\n${capture.stderr}`);
process.stdout.write(capture.stdout);
const cells = JSON.parse(readFileSync(join(output, 'capture/observations.json'), 'utf8'));
const expectedSha256: Record<string, string> = {};
for (const [name, value] of Object.entries(cells)) {
  const result = (value as any).result;
  const injection = name.split('-').slice(1).join('-');
  const error = injection === 'drop' ? 'Sidecar event sequence mismatch: expected 1, got 2.'
    : injection === 'duplicate' ? 'Sidecar event sequence mismatch: expected 1, got 0.' : null;
  assert.equal(result.error, error, `#969 capture ${name}: unexpected transport failure cannot become a golden`);
  const bytes = JSON.stringify(value, null, 2) + '\n';
  writeFileSync(join(output, 'expected', `${name}.json`), bytes, {flag:'wx'});
  expectedSha256[name] = createHash('sha256').update(bytes).digest('hex');
}
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/agent/modules/transport/agentLoopSidecarClient.ts',
  'src/agent/modules/transport/agentLoopSidecarServer.ts',
  'src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts',
  'src/cli/pilotdeck-agent-loop-sidecar.ts',
]);
writeFileSync(join(output, 'oracle.json'), JSON.stringify({sourceCommit, node:process.version, sourceSha256, expectedSha256}, null, 2) + '\n', {flag:'wx'});
console.log('#969 generated 12 complete TS evidence cells');
