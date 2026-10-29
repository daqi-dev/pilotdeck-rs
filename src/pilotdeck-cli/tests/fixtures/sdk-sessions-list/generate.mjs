// Re-extract portable Node 22 captures. Never executes TS or a sandbox.
import { readFileSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
assert.equal(process.version, 'v22.22.0', '#983 extraction runtime');
const legacy = process.argv[3] === '--legacy-plan';
assert.ok(process.argv.length === 3 || (process.argv.length === 4 && legacy), '#983 extraction arguments: OUTPUT [--legacy-plan]');
if (legacy) assert.ok(process.env.PDRS_D1_PLAN, '#983 --legacy-plan requires PDRS_D1_PLAN');
const source = legacy ? resolve(process.env.PDRS_D1_PLAN) : undefined;
const bundled = source ? undefined : JSON.parse(readFileSync(new URL('./source.json', import.meta.url)));
const output = resolve(process.argv[2]);
const selected = c => /^(sessions-default|sessions-list)--/.test(c.id) || c.id === 'sessions-unknown--argv';
// The legacy directory and checked-in inputs use the same portable projection.
// These host-only fields never participate in the behavioral extraction below.
const read = path => {
  const bytes = source ? readFileSync(join(source, path)) : Buffer.from(bundled[path]);
  if (path === 'cases.json') return Buffer.from(JSON.stringify(JSON.parse(bytes).filter(selected), null, 2) + '\n');
  if (path.endsWith('/result.json')) {
    const result = JSON.parse(bytes);
    for (const key of ['argv', 'env', 'profileSha256', 'processTree']) delete result[key];
    return Buffer.from(JSON.stringify(result, null, 2) + '\n');
  }
  return bytes;
};
const json = path => JSON.parse(read(path));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const inputs = json('cases.json');
assert.equal(inputs.length, 77, '#983 registered 76 cells and unknown leaf');
assert.equal(read('raw/node-version.stdout.bin').toString(), 'v22.22.0\n', '#983 source Node version');
const batches = (source ? readdirSync(join(source, 'raw')) : [...new Set(Object.keys(bundled).map(p => p.split('/')[1]))])
  .filter(n => /^host-b\d+(-r\d+)?$/.test(n));
const prefix = /^\(node:[0-9]+\) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n/;
const cases = inputs.map(input => {
  const candidates = batches.map(b => `raw/${b}/cases/${input.id}`).filter(p => {
    try { return json(`${p}/result.json`).status === 'OBSERVED'; } catch { return false; }
  });
  assert.equal(candidates.length, 1, `#983 unique OBSERVED source: ${input.id}`);
  const directory = candidates[0];
  const load = name => read(`${directory}/${name}`);
  const result = JSON.parse(load('result.json'));
  assert.deepEqual(JSON.parse(load('case.json')), input, `#983 same input ${input.id}`);
  assert.deepEqual(JSON.parse(load('initial.json')), JSON.parse(load('final.json')), '#983 unchanged TS disk');
  assert.ok(result.cleanup.every(c => c.allStopped && !c.remaining.length && !c.errors.length), '#983 source cleanup');
  const stdout = load('stdout.bin'), stderr = load('stderr.bin');
  assert.equal(sha(stdout), result.stdoutSha256, '#983 source stdout digest');
  assert.equal(sha(stderr), result.stderrSha256, '#983 source stderr digest');
  const mock = input.mockGateway ? load('mock.stdout.jsonl').toString().trim().split('\n').map(JSON.parse) : [];
  const wire = mock.filter(r => ['send','receive'].includes(r.kind)).map(r => ({kind:r.kind, frame:r.frame}));
  // IDs are observations, not CLI output: bind each replay response to the actual UUID.
  const ids = wire.filter(r => r.kind === 'receive' && r.frame.type === 'request').map(r => r.frame.id);
  assert.ok(ids.length <= 1, '#983 one ordinary request');
  for (const row of wire) if (row.frame.id !== undefined) {
    assert.equal(row.frame.id, ids[0], '#983 response/request binding');
    row.frame.id = '<request>';
  }
  for (const row of wire) row.raw = JSON.stringify(row.frame);
  const files = ['case.json','result.json','stdout.bin','stderr.bin','initial.json','final.json','events.json', ...(input.mockGateway ? ['mock.stdout.jsonl'] : [])];
  return {input, source:directory, sha256:Object.fromEntries(files.map(name => [name,sha(load(name))])),
    stdout:stdout.toString(), stderrRaw:stderr.toString(), stderr:stderr.toString().replace(prefix,''),
    exit:result.exitCode, signal:result.signal, connections:result.gateway?.connections ?? 0,
    wire, close:mock.filter(r => r.kind === 'close').map(({code,reason}) => ({code,reason})),
    disk:JSON.parse(load('initial.json'))};
});
const oracle = {sourceCommit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', nodeVersion:'v22.22.0',
  casesSha256:sha(read('cases.json')), normalization:'specs/cli-parity/normalization.md §6 only', cases};
mkdirSync(output, {recursive:true});
writeFileSync(join(output,'oracle.json'), JSON.stringify(oracle,null,2)+'\n', {flag:'wx'});
console.log(`#983 extracted ${cases.length} observed cases from Node v22.22.0 originals`);
