// Re-extract the coordinator's Node 22 captures. Never executes TS or a sandbox.
import { readFileSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
assert.equal(process.version, 'v22.22.0', '#983 extraction runtime');
const source = resolve(process.env.PDRS_D1_PLAN ?? '/Users/da/ws/pilotdeck-rs/_tmp/20261028-issue-123-d1/plan');
const output = resolve(process.argv[2]);
const read = path => readFileSync(join(source, path));
const json = path => JSON.parse(read(path));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const inputs = json('cases.json').filter(c => /^(sessions-default|sessions-list)--/.test(c.id) || c.id === 'sessions-unknown--argv');
assert.equal(inputs.length, 77, '#983 registered 76 cells and unknown leaf');
assert.equal(read('raw/node-version.stdout.bin').toString(), 'v22.22.0\n', '#983 source Node version');
const batches = readdirSync(join(source, 'raw')).filter(n => /^host-b\d+(-r\d+)?$/.test(n));
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
