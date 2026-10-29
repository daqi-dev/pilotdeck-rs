// Re-extract portable Node 22 captures. Never executes TS or a sandbox.
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
assert.equal(process.version, 'v22.22.0', '#1029 extraction runtime');
assert.equal(process.argv.length, 3, '#1029 extraction arguments: OUTPUT');
const bundled = JSON.parse(readFileSync(new URL('./source.json', import.meta.url)));
const output = resolve(process.argv[2]);
const read = path => { assert.ok(Object.hasOwn(bundled,path), `#1029 source missing ${path}`); return Buffer.from(bundled[path]); };
const json = path => JSON.parse(read(path));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const inputs = json('cases.json');
assert.equal(inputs.length, 157, '#1029 registered observed settings cells');
assert.equal(read('raw/node-version.stdout.bin').toString(), 'v22.22.0\n', '#1029 source Node version');
const batches = [...new Set(Object.keys(bundled).map(p => p.split('/')[1]))]
  .filter(n => /^host-b\d+(-r\d+)?$/.test(n));
const prefix = /^\(node:[0-9]+\) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n/;
const cases = inputs.map(input => {
  const candidates = batches.map(b => `raw/${b}/cases/${input.id}`).filter(p => {
    try { return json(`${p}/result.json`).status === 'OBSERVED'; } catch { return false; }
  });
  assert.equal(candidates.length, 1, `#1029 unique OBSERVED source: ${input.id}`);
  const directory = candidates[0];
  const load = name => read(`${directory}/${name}`);
  const result = JSON.parse(load('result.json'));
  assert.deepEqual(JSON.parse(load('case.json')), input, `#1029 same input ${input.id}`);
  assert.deepEqual(JSON.parse(load('initial.json')), JSON.parse(load('final.json')), '#1029 unchanged TS disk');
  assert.ok(result.cleanup.every(c => c.allStopped && !c.remaining.length && !c.errors.length), '#1029 source cleanup');
  const stdout = load('stdout.bin'), stderr = load('stderr.bin');
  assert.equal(sha(stdout), result.stdoutSha256, '#1029 source stdout digest');
  assert.equal(sha(stderr), result.stderrSha256, '#1029 source stderr digest');
  const mock = input.mockGateway ? load('mock.stdout.jsonl').toString().trim().split('\n').map(JSON.parse) : [];
  const wire = mock.filter(r => ['send','receive'].includes(r.kind)).map(r => ({kind:r.kind, frame:r.frame}));
  // IDs are observations, not CLI output: bind each replay response to the actual UUID.
  const ids = wire.filter(r => r.kind === 'receive' && r.frame.type === 'request').map(r => r.frame.id);
  assert.ok(ids.length <= 1, '#1029 one ordinary request');
  for (const row of wire) if (row.frame.id !== undefined) {
    assert.equal(row.frame.id, ids[0], '#1029 response/request binding');
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
const supplemental = json('supplemental.json');
assert.equal(supplemental.nodeVersion, 'v22.22.0', '#1029 live Node version');
assert.equal(supplemental.cases.length, 34, '#1029 registered live settings boundary cells');
for (const captured of supplemental.cases) {
  const item = structuredClone(captured);
  if (item.status === 'TRIGGER_NOT_REACHED') {
    assert.ok(item.input.signal, '#1029 unreachable signal retains configuration');
    assert.equal(item.input.signalAt, 'request:unreachable-for-local-branch', '#1029 only local usage has no trigger');
    assert.equal(item.signalDelivered, false, '#1029 never claim undelivered signal');
    assert.equal(item.connections, 0, '#1029 local usage never connects');
    item.applicability = 'local exit observed; signal delivery not applicable (no RPC trigger)';
  }

  const requests = item.wire.filter(row=>row.kind==='receive' && row.frame.type==='request');
  assert.ok(requests.length<=1, '#1029 one live RPC');
  const id = requests[0]?.frame.id;
  for (const row of item.wire) {
    if (row.frame.id !== undefined) {
      assert.equal(row.frame.id,id,'#1029 live response/request binding');
      row.frame.id='<request>';
    }
    row.raw = row.raw ? row.raw.replace(id,'<request>') : JSON.stringify(row.frame);
    // Reply values can contain lone UTF-16 surrogates; preserve the actual wire
    // JSON string instead of requiring serde_json to materialize those strings.
    delete row.frame;
  }
  assert.equal(item.signal,null,'#1029 supplemental exited without signal');
  item.stderr=item.stderrRaw.replace(prefix,'');
  item.source='supplemental.json';
  item.sha256={'supplemental.json':sha(read('supplemental.json'))};
  cases.push(item);
}
const oracle = {sourceCommit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', nodeVersion:'v22.22.0',
  casesSha256:sha(read('cases.json')), normalization:'specs/cli-parity/normalization.md §6 only', supplementalSourceSha256:supplemental.sourceSha256, cases};
mkdirSync(output, {recursive:true});
writeFileSync(join(output,'oracle.json'), JSON.stringify(oracle,null,2)+'\n', {flag:'wx'});
console.log(`#1029 extracted ${cases.length} observed cases from Node v22.22.0 originals`);
