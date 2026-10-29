// Re-extract portable Node 22 captures. Never executes TS or a sandbox.
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
assert.equal(process.version, 'v22.22.0', '#1058 extraction runtime');
assert.equal(process.argv.length, 3, '#1058 extraction arguments: OUTPUT');
const bundled = JSON.parse(readFileSync(new URL('./source.json', import.meta.url)));
const output = resolve(process.argv[2]);
const read = path => { assert.ok(Object.hasOwn(bundled,path), `#1058 source missing ${path}`); return Buffer.from(bundled[path]); };
const json = path => JSON.parse(read(path));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const inputs = json('cases.json');
assert.equal(inputs.length, 36, '#1058 registered observed run cells');
assert.equal(read('raw/node-version.stdout.bin').toString(), 'v22.22.0\n', '#1058 source Node version');
const batches = [...new Set(Object.keys(bundled).map(p => p.split('/')[1]))]
  .filter(n => /^host-b\d+(-r\d+)?$/.test(n));
const prefix = /^\(node:[0-9]+\) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n/;
const cases = inputs.map(input => {
  const candidates = batches.map(b => `raw/${b}/cases/${input.id}`).filter(p => {
    try { return json(`${p}/result.json`).status === 'OBSERVED'; } catch { return false; }
  });
  assert.equal(candidates.length, 1, `#1058 unique OBSERVED source: ${input.id}`);
  const directory = candidates[0];
  const load = name => read(`${directory}/${name}`);
  const result = JSON.parse(load('result.json'));
  assert.deepEqual(JSON.parse(load('case.json')), input, `#1058 same input ${input.id}`);
  assert.deepEqual(JSON.parse(load('initial.json')), JSON.parse(load('final.json')), '#1058 unchanged TS disk');
  assert.ok(result.cleanup.every(c => c.allStopped && !c.remaining.length && !c.errors.length), '#1058 source cleanup');
  const stdout = load('stdout.bin'), stderr = load('stderr.bin');
  assert.equal(sha(stdout), result.stdoutSha256, '#1058 source stdout digest');
  assert.equal(sha(stderr), result.stderrSha256, '#1058 source stderr digest');
  const mock = input.mockGateway ? load('mock.stdout.jsonl').toString().trim().split('\n').map(JSON.parse) : [];
  // Preserve original UUIDs in the source fixture. Replay binds only envelope
  // id and params.runId to freshly generated UUIDs; no stdout normalization.
  const wire = mock.filter(r => ['connection','send','receive','close'].includes(r.kind));
  for (const row of wire) if (row.frame) row.raw = JSON.stringify(row.frame);
  const files = ['case.json','result.json','stdout.bin','stderr.bin','initial.json','final.json','events.json', ...(input.mockGateway ? ['mock.stdout.jsonl'] : [])];
  return {input, source:directory, sha256:Object.fromEntries(files.map(name => [name,sha(load(name))])),
    stdout:stdout.toString(), stderrRaw:stderr.toString(), stderr:stderr.toString().replace(prefix,''),
    exit:result.exitCode, signal:result.signal, connections:result.gateway?.connections ?? 0,
    wire, close:mock.filter(r => r.kind === 'close').map(({code,reason}) => ({code,reason})),
    disk:JSON.parse(load('initial.json'))};
});
const supplemental = json('supplemental.json');
assert.equal(supplemental.nodeVersion,'v22.22.0','#1058 supplemental Node version');
assert.equal(supplemental.kind,'real-ts-function-probe-in-process-wire','#1058 supplemental scope');
assert.equal(supplemental.cases.length,32,'#1058 supplemental cells');
for(const item of supplemental.cases) {
  // Exceptional model resolution belongs to P06/P08: retain observations in
  // source.json without treating socket-double exit as real lifecycle parity.
  if(['model-disabled','model-ambiguous'].includes(item.input.axis)) continue;
  // Rust's serde_json metadata cannot represent lone UTF-16 surrogates. Keep
  // the exact wire JSON in raw; only retain event runId for structural binding.
  // The full original frame remains byte-for-byte in source.json.
  for(const row of item.wire) if(row.frame?.event) {
    const runId = row.frame.event.runId;
    row.frame.event = runId === undefined ? {} : {runId};
  }
  item.stderr = item.stderrRaw.replace(prefix,'');
  item.sha256 = {'supplemental.json':sha(read('supplemental.json'))};
  cases.push(item);
}
const oracle = {sourceCommit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', nodeVersion:'v22.22.0',
  casesSha256:sha(read('cases.json')), normalization:'specs/cli-parity/normalization.md §6 only', supplementalSourceSha256:supplemental.sourceSha256, cases};
mkdirSync(output, {recursive:true});
writeFileSync(join(output,'oracle.json'), JSON.stringify(oracle,null,2)+'\n', {flag:'wx'});
console.log(`#1058 extracted ${cases.length} observed run cases from Node v22.22.0 originals`);
