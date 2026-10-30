// #1129: execute the real generator with an ambient clock stuck in one millisecond.
// The generator must separate its snapshot writes without changing lock deadlines.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

assert.equal(process.version, 'v22.22.0', '#1129 pinned Node runtime');
assert.ok(process.argv[2], '#1129 provide a fresh output directory');
const output = resolve(process.argv[2]);
const NativeDate = Date;
const frozenMs = 1760745600000;
const SameMillisecondDate = new Proxy(NativeDate, {
  construct(target, args, newTarget) {
    return Reflect.construct(target, args.length ? args : [frozenMs], newTarget);
  },
});
globalThis.Date = SameMillisecondDate;
try {
  assert.equal(new Date().getTime(), frozenMs, '#1129 ambient time is fixed');
  assert.equal(new Date().getTime(), frozenMs, '#1129 ambient time stays fixed');
  await import('./generate.mts');
  assert.equal(Date, SameMillisecondDate, '#1129 generator restores its caller clock');
  assert.equal(Date.now, NativeDate.now, '#1129 wall-clock deadlines stay native');
  assert.equal(new Date(123).getTime(), 123, '#1129 explicit Date arguments stay native');
} finally {
  globalThis.Date = NativeDate;
}
const oracle = JSON.parse(await readFile(join(output, 'oracle.json'), 'utf8'));
const scenario = (name: string) => {
  const found = oracle.scenarios.find((s: any) => s.name === name);
  assert.ok(found, `#1129 scenario exists: ${name}`);
  return found;
};
const orphans = scenario('orphans');
const seed = JSON.parse(await readFile(join(output, orphans.initial), 'utf8'));
const checks: any[] = [];
for (const step of orphans.steps) {
  if (step.op !== 'get') continue;
  const original = seed.records.find((r: any) => r.runId === step.input.runId);
  assert.ok(original, '#1129 orphan seed exists');
  const record = step.outcome.record;
  assert.ok(record, '#1129 orphan get returned a record');
  checks.push({ runId: record.runId, state: record.state, seedAcceptedAt: original.acceptedAt,
    seedUpdatedAt: original.updatedAt, acceptedAt: record.acceptedAt, updatedAt: record.updatedAt });
  console.log(JSON.stringify(checks.at(-1)));
  assert.equal(record.acceptedAt, original.acceptedAt, '#1129 seed acceptedAt stays unmasked');
  assert.equal(record.updatedAt, record.state === 'interrupted' ? '<new>' : original.updatedAt,
    '#1129 same-millisecond recovery masks only the new timestamp');
}
for (const name of ['lock-stale-dead', 'lock-stale-garbage']) {
  const record = scenario(name).steps[0].outcome.record;
  assert.ok(record, '#1129 stale lock accept returned a record');
  console.log(JSON.stringify({ scenario: name, acceptedAt: record.acceptedAt, updatedAt: record.updatedAt }));
  assert.equal(record.acceptedAt, '<new>', '#1129 same-millisecond accept masks acceptedAt');
  assert.equal(record.updatedAt, '<new>', '#1129 same-millisecond accept masks updatedAt');
}
assert.equal(checks.length, 11, '#1129 all orphan timestamp cases checked');
console.log('PASS: #1129 same-millisecond timestamps are masked and seed timestamps preserved');
