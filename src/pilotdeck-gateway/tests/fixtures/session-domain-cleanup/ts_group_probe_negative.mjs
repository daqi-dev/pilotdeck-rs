#!/usr/bin/env node
// Negative control for the Node signal gate in ts_group_probe.mjs.
//
// It copies the probe to a temp file, removes only the ownership check in the
// two signal entries, and replaces the real senders with a recorder.  The
// mutated copy must then fail its own term-early expectation (a reaped leader
// is "signalled"), proving the gate is load-bearing.  No real signal is sent
// by the mutated copy.  The repository source is never touched.
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const here = fileURLToPath(new URL('.', import.meta.url));
const source = readFileSync(join(here, 'ts_group_probe.mjs'), 'utf8');
const gate = '  if (!identityHeld(entry)) {';
const groupSend = '    process.kill(-entry.pid, signal);';
const childSend = '    entry.child.kill(signal);';
const termRun = "  results.push(await run('term', folder, inFlight));\n";
for (const [needle, count] of [[gate, 2], [groupSend, 1], [childSend, 1], [termRun, 1]]) {
  const seen = source.split(needle).length - 1;
  if (seen !== count) {
    console.error(JSON.stringify({ status: 'INVALID', needle, seen, want: count }));
    process.exit(2);
  }
}
let mutated = source
  .replaceAll(gate, '  if (false) {  // mutant: gate removed')
  .replace(groupSend, "    globalThis.__pdrs164FakeSends.push(['group', -entry.pid, signal]);")
  .replace(childSend, "    globalThis.__pdrs164FakeSends.push(['child', entry.pid, signal]);")
  .replace(termRun, '')
  .replace('const folder = mkdtempSync',
           'globalThis.__pdrs164FakeSends = [];\nconst folder = mkdtempSync')
  .replace('console.log(JSON.stringify({ status, results }, null, 2));',
           'console.log(JSON.stringify({ status, results, fakeSends: globalThis.__pdrs164FakeSends }, null, 2));');

const folder = mkdtempSync(join(tmpdir(), 'pdrs164-node-neg-'));
const target = join(folder, 'ts_group_probe_mutated.mjs');
writeFileSync(target, mutated, { flag: 'wx' });
let run;
try {
  run = spawnSync(process.execPath, [target], { encoding: 'utf8', timeout: 120000 });
} finally {
  rmSync(folder, { recursive: true, force: true });
}
let parsed = null;
try {
  parsed = JSON.parse(run.stdout);
} catch (error) {
  console.error(JSON.stringify({ status: 'INVALID', error: String(error),
    stdout: run.stdout, stderr: run.stderr }));
  process.exit(2);
}
const early = (parsed.results || []).find((row) => row && row.mode === 'term-early');
const ok = parsed.status !== 'PASS'
  && early && early.signal === 'SIGTERM'
  && early.signalRefused === null
  && (parsed.fakeSends || []).some((row) => row[0] === 'group');
console.log(JSON.stringify({
  status: ok ? 'KILLED' : 'SURVIVED',
  mutatedStatus: parsed.status,
  early: early || null,
  fakeSends: parsed.fakeSends || [],
  exitCode: run.status,
}, null, 2));
process.exitCode = ok ? 0 : 1;
