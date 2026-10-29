// Re-extract portable Node 22 captures. Never executes TS or a sandbox.
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
assert.equal(process.version, 'v22.22.0', '#1028 extraction runtime');
assert.equal(process.argv.length, 3, '#1028 extraction arguments: OUTPUT');
const bundled = JSON.parse(readFileSync(new URL('./source.json', import.meta.url)));
const output = resolve(process.argv[2]);
const read = path => { assert.ok(Object.hasOwn(bundled,path), `#1028 source missing ${path}`); return Buffer.from(bundled[path]); };
const json = path => JSON.parse(read(path));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
function verifyEvents(events, stdout, stderr, id) {
  for (const [stream, bytes] of [['stdout', stdout], ['stderr', stderr]]) {
    const observed = Buffer.concat(events.filter(event => event.stream === stream)
      .map(event => Buffer.from(event.bytesBase64, 'base64')));
    assert.deepEqual(observed, bytes, `#1028 raw ${stream}/events bytes ${id}`);
  }
}
const inputs = json('cases.json');
assert.equal(inputs.length, 39, '#1028 registered 39 messages cells');
assert.equal(read('raw/node-version.stdout.bin').toString(), 'v22.22.0\n', '#1028 source Node version');
const batches = [...new Set(Object.keys(bundled).map(p => p.split('/')[1]))]
  .filter(n => /^host-b\d+(-r\d+)?$/.test(n));
const prefix = /^\(node:[0-9]+\) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n/;
const cases = inputs.map(input => {
  const candidates = batches.map(b => `raw/${b}/cases/${input.id}`).filter(p => {
    try { return json(`${p}/result.json`).status === 'OBSERVED'; } catch { return false; }
  });
  assert.equal(candidates.length, 1, `#1028 unique OBSERVED source: ${input.id}`);
  const directory = candidates[0];
  const load = name => read(`${directory}/${name}`);
  const result = JSON.parse(load('result.json'));
  assert.deepEqual(JSON.parse(load('case.json')), input, `#1028 same input ${input.id}`);
  assert.deepEqual(JSON.parse(load('initial.json')), JSON.parse(load('final.json')), '#1028 unchanged TS disk');
  assert.ok(result.cleanup.every(c => c.allStopped && !c.remaining.length && !c.errors.length), '#1028 source cleanup');
  const stdout = load('stdout.bin'), stderr = load('stderr.bin');
  assert.equal(sha(stdout), result.stdoutSha256, '#1028 source stdout digest');
  assert.equal(sha(stderr), result.stderrSha256, '#1028 source stderr digest');
  verifyEvents(JSON.parse(load('events.json')), stdout, stderr, input.id);
  const mock = input.mockGateway ? load('mock.stdout.jsonl').toString().trim().split('\n').map(JSON.parse) : [];
  const wire = mock.filter(r => ['send','receive'].includes(r.kind)).map(r => ({kind:r.kind, frame:r.frame}));
  // IDs are observations, not CLI output: bind each replay response to the actual UUID.
  const ids = wire.filter(r => r.kind === 'receive' && r.frame.type === 'request').map(r => r.frame.id);
  assert.ok(ids.length <= 1, '#1028 one ordinary request');
  for (const row of wire) if (row.frame.id !== undefined) {
    assert.equal(row.frame.id, ids[0], '#1028 response/request binding');
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
assert.equal(supplemental.nodeVersion, 'v22.22.0', '#1028 live Node version');
assert.equal(supplemental.cases.length, 13, '#1028 registered live shape and ID cells');
for (const file of [
  'dist/src/cli/pilotdeck.js',
  'dist/src/cli/sdkCli.js',
  'packages/sdk/dist/index.js',
  'packages/sdk/dist/client.js',
]) {
  assert.match(supplemental.sourceSha256[file] ?? '', /^[0-9a-f]{64}$/, `#1028 compiled provenance ${file}`);
}
for (const captured of supplemental.cases) {
  const item = structuredClone(captured);
  verifyEvents(item.events, Buffer.from(item.stdout), Buffer.from(item.stderrRaw), item.input.id);
  delete item.events; // The original events remain in source.json, before normalization.
  const requests = item.wire.filter(row=>row.kind==='receive' && row.frame.type==='request');
  assert.ok(requests.length<=1, '#1028 one live RPC');
  const id = requests[0]?.frame.id;
  for (const row of item.wire) {
    if (row.frame.id !== undefined) {
      assert.equal(row.frame.id,id,'#1028 live response/request binding');
      row.frame.id='<request>';
    }
    row.raw = row.raw ? row.raw.replace(id,'<request>') : JSON.stringify(row.frame);
    // Reply values can contain lone UTF-16 surrogates; preserve the actual wire
    // JSON string instead of requiring serde_json to materialize those strings.
    if (row.kind === 'send') delete row.frame;
  }
  assert.equal(item.signal,null,'#1028 supplemental exited without signal');
  item.stderr=item.stderrRaw.replace(prefix,'');
  item.source='supplemental.json';
  item.sha256={'supplemental.json':sha(read('supplemental.json'))};
  cases.push(item);
}
const oracle = {sourceCommit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', nodeVersion:'v22.22.0',
  casesSha256:sha(read('cases.json')), normalization:'specs/cli-parity/normalization.md §6 only', supplementalSourceSha256:supplemental.sourceSha256, cases};
mkdirSync(output, {recursive:true});
writeFileSync(join(output,'oracle.json'), JSON.stringify(oracle,null,2)+'\n', {flag:'wx'});
console.log(`#1028 extracted ${cases.length} observed cases from Node v22.22.0 originals`);
