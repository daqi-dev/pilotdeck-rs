import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Run from repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
import { access, copyFile, mkdir, mkdtemp, readFile, readdir, rm, utimes, writeFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory or Rust registry path');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const source = 'src/gateway/run/RunRegistry.ts';
const sourceSha256 = await baselineSourceHashes(baseline, [source]);
const { FileRunRegistry } = await import(pathToFileURL(join(baseline, source)).href);

// i32::MAX is a valid pid number that no process holds (ESRCH); i32::MAX + 1
// makes Node's process.kill throw ERR_INVALID_ARG_TYPE, which TS treats as alive.
const DEAD = 2147483647;
const HUGE = 2147483648;
const ref = (runId: string, projectKey = 'project', sessionKey = 'session') => ({ sessionKey, projectKey, runId });
// No projectKey at all (a default parameter would fill an explicit undefined).
const bare = (runId: string, sessionKey: string) => ({ sessionKey, runId });
const accept = (r: any, requestMaterial: string) => ({ op: 'accept', input: { ...r, requestMaterial } });
const append = (r: any, event: unknown) => ({ op: 'append', input: r, event });
const completed = (runId: string, finishReason = 'completed') => ({ type: 'turn_completed', runId, usage: {}, finishReason });
const exists = (path: string) => access(path).then(() => true, () => false);
// Mask only values this run wrote: its own pid and timestamps absent from the initial file.
// The snapshot clock below guarantees new writes cannot collide with seed timestamps.
// Initial files never carry this process's pid (realSnapshot rewrites it), so every
// ownerPid equal to process.pid is "<self>"; any other pid is kept verbatim.
const initialValues = (text: string) => {
  const pids = new Set([...text.matchAll(/"ownerPid":([0-9]+)/g)].map(m => m[1]));
  assert.ok(!pids.has(String(process.pid)), 'initial file must not contain the generator pid');
  return { times: new Set([...text.matchAll(/"(?:acceptedAt|updatedAt)":"([^"\\]*)"/g)].map(m => m[1])) };
};
// Error messages name the scratch path; fixtures carry "<dir>" instead.
const portable = (message: string) => message.split(scratch).join('<dir>');
function mask(record: any, { times }: ReturnType<typeof initialValues>) {
  if (!record || typeof record !== 'object') return record ?? null;
  const masked = structuredClone(record);
  if (masked.ownerPid === process.pid) masked.ownerPid = '<self>';
  for (const key of ['acceptedAt', 'updatedAt']) if (typeof masked[key] === 'string' && !times.has(masked[key])) masked[key] = '<new>';
  return masked;
}
async function runStep(registry: any, step: any, times: ReturnType<typeof initialValues>) {
  try {
    switch (step.op) {
      case 'accept': { const r = await registry.accept(step.input); return { duplicate: r.duplicate, record: mask(r.record, times) }; }
      case 'append': await registry.append({ ...step.input, event: step.event }); return { ok: true };
      case 'get': return { record: mask(await registry.get(step.input), times) };
      case 'events': return { events: await registry.events({ ...step.input, ...(step.afterSeq === undefined ? {} : { afterSeq: step.afterSeq }), ...(step.limit === undefined ? {} : { limit: step.limit }) }) };
      case 'mark': await registry.markOrphansInterrupted(); return { ok: true };
      default: throw new Error(`unknown step ${step.op}`);
    }
  } catch (error: any) {
    return { error: { name: error.name, code: error.code ?? null, message: portable(error.message) } };
  }
}

if (process.argv[3] === '--verify-rust') {
  // Rust holds a fresh `<path>.lock` while this process starts: TS must wait on
  // the shared exclusive-create protocol instead of reading or writing through it.
  const lockPath = `${output}.lock`;
  assert.ok(await exists(lockPath), 'Rust must hold the registry lock before TS starts');
  console.log('WAITING');
  const startedAt = Date.now();
  const registry = new FileRunRegistry(output);
  const run1 = await registry.accept({ ...ref('run-1'), requestMaterial: 'request-1' });
  const waitedMs = Date.now() - startedAt;
  assert.ok(waitedMs >= 200, `TS did not wait for the Rust lock (${waitedMs} ms)`);
  assert.equal(run1.duplicate, true);
  assert.equal(run1.record.state, 'completed');
  assert.deepEqual((await registry.events(ref('run-1'))).map((e: any) => [e.seq, e.event.type]), [[1, 'input_accepted'], [2, 'assistant_text_delta'], [3, 'turn_completed']]);
  assert.equal((await registry.events(ref('run-1')))[1].event.text, '中文\ud800');
  await assert.rejects(registry.accept({ ...ref('run-1'), requestMaterial: 'other' }), (e: any) => e.code === 'conflict');
  // The Rust test process is alive: its accepted run is not fenced. A dead owner is.
  assert.equal((await registry.get(ref('rust-live'))).state, 'accepted');
  assert.equal((await registry.get(ref('rust-dead'))).state, 'interrupted');
  assert.equal((await registry.get(ref('rust-dead'))).revision, 3);
  await registry.append({ ...ref('rust-live'), event: completed('rust-live') });
  await registry.accept({ ...ref('ts-run'), requestMaterial: 'ts' });
  await registry.append({ ...ref('ts-run'), event: { type: 'input_accepted', runId: 'ts-run' } });
  assert.equal(await exists(lockPath), false);
  console.log(`PASS: real TS waited ${waitedMs} ms on the Rust lock, read Rust records/events, fenced only the dead owner and continued the snapshot`);
  process.exit(0);
}

await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const scenarios: any[] = [];
async function scenario(name: string, options: { initial?: string | Buffer; lock?: { contents: string; ageMs: number }; steps?: any[] }) {
  const path = join(scratch, `${name}.json`);
  const lockPath = `${path}.lock`;
  const initialFile = options.initial === undefined ? null : `${name}.json`;
  if (initialFile) {
    await writeFile(path, options.initial!);
    await copyFile(path, join(output, initialFile), constants.COPYFILE_EXCL);
  }
  if (options.lock) {
    await writeFile(lockPath, options.lock.contents);
    const old = new Date(Date.now() - options.lock.ageMs);
    await utimes(lockPath, old, old);
  }
  const initialText = initialFile ? await readFile(path, 'utf8') : '';
  const times = initialValues(initialText);
  const registry = new FileRunRegistry(path);
  let openError = null;
  // Readiness is the TS open boundary: constructor recovery, awaited by every call.
  try { await registry.ready; } catch (error: any) { openError = { name: error.name, message: portable(error.message) }; }
  const steps = [];
  if (!openError) for (const step of options.steps ?? []) steps.push({ ...step, outcome: await runStep(registry, step, times) });
  let after = null;
  if (await exists(path)) {
    after = `${name}.after.json`;
    await copyFile(path, join(output, after), constants.COPYFILE_EXCL);
  }
  const lockLeft = await exists(lockPath);
  const leftovers = (await readdir(scratch)).filter(f => f.startsWith(`${name}.json.`) && f.endsWith('.tmp'));
  assert.deepEqual(leftovers, []);
  scenarios.push({ name, initial: initialFile, lock: options.lock ?? null, openError, steps, after, lockLeft });
  console.log(`${name}: open=${openError ? `${openError.name}: ${openError.message}` : 'OK'} steps=${steps.length} after=${after ?? '-'} lockLeft=${lockLeft}`);
}
// A small valid snapshot written by the real registry, for lock and paging cases.
// Seed owners still equal to this process's pid become DEAD, so no initial file
// carries a machine-specific pid (seeds left untouched are all terminal, which
// recovery never fences).
async function realSnapshot(build: (registry: any) => Promise<void>, edit?: (snapshot: any) => void) {
  const path = join(scratch, `.seed-${scenarios.length}-${Math.random().toString(16).slice(2)}.json`);
  await build(new FileRunRegistry(path));
  let bytes = await readFile(path, 'utf8');
  if (edit) {
    const snapshot = JSON.parse(bytes);
    edit(snapshot);
    bytes = JSON.stringify(snapshot);
  }
  return bytes.replace(new RegExp(`"ownerPid":${process.pid}(?![0-9.eE])`, 'g'), `"ownerPid":${DEAD}`);
}
// Every step must reach the branch its scenario claims, not miss the record:
// the last scenario's outcomes, one kind per step.
const kind = (o: any) =>
  o.error ? o.error.code ?? o.error.name
  : 'duplicate' in o ? (o.duplicate ? 'duplicate' : 'created')
  : 'record' in o ? (o.record === null ? 'absent' : 'record')
  : 'events' in o ? `events:${o.events.length}` : 'ok';
function expectKinds(expected: string[]) {
  const last = scenarios.at(-1);
  assert.equal(last.openError, null, last.name);
  assert.deepEqual(last.steps.map((s: any) => kind(s.outcome)), expected, last.name);
  // A 'record' step must have read the run it asked for, not a neighbour.
  for (const s of last.steps) {
    const r = s.outcome.record;
    if (r && typeof r === 'object' && !Array.isArray(r) && 'runId' in r) assert.equal(String(r.runId), String(s.input.runId), `${last.name} read another run`);
  }
  return last.steps.map((s: any) => s.outcome);
}
const afterSnapshot = async () => JSON.parse(await readFile(join(output, scenarios.at(-1).after), 'utf8'));

// #1129: every no-argument Date construction gets a distinct snapshot millisecond,
// including seeds. Keep Date.now(), Date() and explicit Date arguments native:
// lock deadlines, stale-lock checks and mtime still use the real wall clock.
const NativeDate = Date;
let tick = 0;
const SnapshotDate = new Proxy(NativeDate, {
  construct(target, args, newTarget) {
    return Reflect.construct(target, args.length ? args : [1760745600000 + tick++], newTarget);
  },
});
globalThis.Date = SnapshotDate;
try {
  // Fresh registry: duplicate, conflict, every state transition and RunRegistryError branch.
  const p = (runId: string) => ref(runId);
  const noProject = bare('run-2', 'session');
  await scenario('normal', {
    steps: [
      accept(p('run-1'), 'request-1'),
      accept(p('run-1'), 'request-1'),
      accept(p('run-1'), 'other'),
      append(p('run-1'), { type: 'input_accepted', runId: 'run-1' }),
      { op: 'get', input: p('run-1') },
      append(p('run-1'), { type: 'assistant_text_delta', runId: 'run-1', text: '中文\ud800', nested: { z: 0, a: 1, 1: 'index-first' } }),
      append(p('run-1'), completed('run-1')),
      append(p('run-1'), { type: 'assistant_text_delta', runId: 'run-1', text: 'late' }),
      accept(noProject, 'request-2'),
      append(noProject, { type: 'error', runId: 'run-2', code: 'admission_rejected', message: 'busy', recoverable: true }),
      { op: 'get', input: noProject },
      append(noProject, completed('run-2', 'task_budget')),
      accept(ref('run-2', ''), 'request-2'),
      accept(p('run-3'), 'request-3'),
      append(p('run-3'), completed('run-3', 'aborted_streaming')),
      append(p('run-3'), completed('run-3')),
      accept(p('run-4'), 'request-4'),
      append(p('run-4'), { type: 'error', runId: 'run-4', code: 'result_unknown', message: 'lost', recoverable: false }),
      append(p('run-4'), { type: 'input_accepted', runId: 'run-4' }),
      append(p('missing'), { type: 'input_accepted', runId: 'missing' }),
      append(ref('run-1', 'other-project'), { type: 'input_accepted', runId: 'run-1' }),
      accept(p('run-5'), 'request-5'),
      { op: 'get', input: p('run-5') },
      accept(p('run-6'), 'request-6'),
      append(p('run-6'), 'raw-event'),
      { op: 'get', input: p('run-6') },
      { op: 'get', input: p('missing') },
      { op: 'events', input: p('run-1') },
      { op: 'mark' },
    ],
  });
  expectKinds([
    'created', 'duplicate', 'conflict', 'ok', 'record', 'ok', 'ok', 'terminal',
    'created', 'ok', 'record', 'ok', 'duplicate',
    'created', 'ok', 'terminal', 'created', 'ok', 'terminal', 'not_found', 'not_found',
    'created', 'record', 'created', 'ok', 'record', 'absent', 'events:3', 'ok',
  ]);

  // Restart: real TS records whose owners are rewritten, then reopened.
  const owners: [string, unknown, unknown[]][] = [
    ['dead-accepted', DEAD, []],
    ['dead-running', DEAD, [{ type: 'input_accepted', runId: 'orphan-dead-running' }]],
    ['absent', undefined, []],
    ['zero', 0, [{ type: 'input_accepted', runId: 'orphan-zero' }]],
    ['negative', -5, []],
    ['fraction', 1.5, []],
    ['string', '1', []],
    ['init', 1, [{ type: 'input_accepted', runId: 'orphan-init' }]],
    ['huge', HUGE, []],
    ['dead-completed', DEAD, [completed('orphan-dead-completed')]],
    ['dead-failed', DEAD, [{ type: 'error', runId: 'orphan-dead-failed', code: 'boom', message: 'x', recoverable: false }]],
  ];
  const orphanInitial = await realSnapshot(async registry => {
    for (const [name, , events] of owners) {
      await registry.accept({ ...ref(`orphan-${name}`), requestMaterial: name });
      for (const event of events) await registry.append({ ...ref(`orphan-${name}`), event });
    }
  }, snapshot => {
    for (const record of snapshot.records) {
      const owner = owners.find(([name]) => `orphan-${name}` === record.runId)![1];
      if (owner === undefined) delete record.ownerPid; else record.ownerPid = owner;
    }
  });
  await scenario('orphans', {
    initial: orphanInitial,
    steps: [
      ...owners.map(([name]) => ({ op: 'get', input: ref(`orphan-${name}`) })),
      // A crashed run stays fenced: resubmission is a duplicate, never a new execution.
      accept(ref('orphan-dead-running'), 'dead-running'),
      { op: 'events', input: ref('orphan-dead-running') },
      append(ref('orphan-dead-running'), completed('orphan-dead-running')),
      accept(ref('orphan-dead-completed'), 'dead-completed'),
      { op: 'mark' },
    ],
  });

  // Paging and event-entry coercion of the TS filter/slice.
  const pagingInitial = await realSnapshot(async registry => {
    await registry.accept({ ...ref('run-page'), requestMaterial: 'page' });
    await registry.append({ ...ref('run-page'), event: { type: 'input_accepted', runId: 'run-page' } });
    await registry.append({ ...ref('run-page'), event: { type: 'assistant_text_delta', runId: 'run-page', text: 'partial' } });
    await registry.append({ ...ref('run-page'), event: completed('run-page') });
  }, snapshot => {
    snapshot.records.push({ sessionKey: 'session', projectKey: 'project', runId: 'run-weird', requestMaterial: 'weird', state: 'completed', revision: 1, lastSeq: 9 });
    snapshot.events['project\0session\0run-weird'] = [{ seq: '2', event: 'string-seq' }, { seq: null, event: 'null-seq' }, { seq: true, event: 'bool-seq' }, { event: 'missing-seq' }, 5, { seq: ' 0x3 ', event: 'hex-seq' }, { seq: [4], event: 'array-seq' }, { seq: '1e0', event: 'exp-seq' }, { seq: 'x', event: 'nan-seq' }];
    snapshot.events['project\0session\0ghost'] = [{ seq: 1, event: { type: 'input_accepted' } }];
  });
  const page = (extra: any) => ({ op: 'events', input: ref('run-page'), ...extra });
  await scenario('paging', {
    initial: pagingInitial,
    steps: [
      page({}), page({ afterSeq: 1 }), page({ limit: 2 }), page({ afterSeq: 0, limit: 0 }), page({ limit: -1 }), page({ limit: -10 }),
      page({ limit: 1.5 }), page({ limit: -0.5 }), page({ afterSeq: 1.5 }), page({ afterSeq: -1, limit: 500 }), page({ afterSeq: 3 }),
      { op: 'events', input: ref('run-weird') }, { op: 'events', input: ref('run-weird'), afterSeq: 1 },
      { op: 'events', input: ref('ghost') }, { op: 'get', input: ref('ghost') },
    ],
  });

  // Snapshot shapes the TS loader accepts, then rewrites on the next mutation.
  await scenario('quirk-duplicates', {
    initial: JSON.stringify({ schemaVersion: 1, extra: true, records: [
      { sessionKey: 's', runId: 'dup', requestMaterial: 'first', custom: 1 },
      { sessionKey: 's', runId: 'other', requestMaterial: 'other' },
      { sessionKey: 's', runId: 'dup', requestMaterial: 'second', custom: 2 },
    ], events: {} }),
    steps: [accept(bare('dup', 's'), 'second'), accept(bare('dup', 's'), 'first'), accept(bare('q', 's'), 'q')],
  });
  assert.equal(expectKinds(['duplicate', 'conflict', 'created'])[0].record.custom, 2);
  await scenario('quirk-empty-project', {
    initial: JSON.stringify({ schemaVersion: 1, records: [
      { sessionKey: 's', runId: 'r', requestMaterial: 'm', state: 'completed' },
      { projectKey: '', sessionKey: 's', runId: 'r', requestMaterial: 'n', state: 'completed' },
    ], events: {} }),
    steps: [{ op: 'get', input: bare('r', 's') }, accept(ref('r', '', 's'), 'n'), accept(bare('r', 's'), 'm'), accept(bare('q', 's'), 'q')],
  });
  assert.equal(expectKinds(['record', 'duplicate', 'conflict', 'created'])[0].record.requestMaterial, 'n');
  await scenario('quirk-coerce', {
    initial: JSON.stringify({ schemaVersion: 1, records: [
      0,
      { sessionKey: 7, runId: 'r', requestMaterial: 'm', state: 'running', ownerPid: HUGE, revision: '1', lastSeq: null },
      { projectKey: null, sessionKey: 's', runId: ['a', null, 'b'], requestMaterial: 'm', state: 'accepted', ownerPid: DEAD, revision: true },
      { sessionKey: 's', runId: 'obj', requestMaterial: 'm', state: 'running', ownerPid: HUGE, revision: {}, lastSeq: [2] },
    ], events: {} }),
    steps: [
      { op: 'get', input: bare('r', '7') },
      append(bare('r', '7'), { type: 'input_accepted' }),
      { op: 'get', input: bare('r', '7') },
      { op: 'get', input: ref('a,,b', '', 's') },
      append(bare('a,,b', 's'), { type: 'input_accepted' }),
      append(bare('obj', 's'), { type: 'input_accepted' }),
      { op: 'get', input: bare('obj', 's') },
      { op: 'get', input: bare('undefined', 'undefined') },
      accept(bare('undefined', 'undefined'), 'replaces-falsy'),
    ],
  });
  {
    const o = expectKinds(['record', 'ok', 'record', 'record', 'terminal', 'ok', 'record', 'absent', 'created']);
    // null + 1, "1" + 1, true + 1 (open fence), [2] + 1, {} + 1.
    assert.deepEqual([o[2].record.lastSeq, o[2].record.revision], [1, '11']);
    assert.deepEqual([o[3].record.state, o[3].record.revision], ['interrupted', 2]);
    assert.deepEqual([o[6].record.lastSeq, o[6].record.revision], ['21', '[object Object]1']);
    // The new record takes the falsy record's first position.
    assert.equal((await afterSnapshot()).records[0].requestMaterial, 'replaces-falsy');
  }
  // A falsy record is absent to get/append, but `events` checks Map.has, and
  // accept replaces it in place, appending after the events already stored.
  const falsy = bare('undefined', 'undefined');
  await scenario('quirk-falsy', {
    initial: JSON.stringify({ schemaVersion: 1, records: [
      { sessionKey: 's', runId: 'before', requestMaterial: 'm', state: 'completed' },
      false,
      { sessionKey: 's', runId: 'after', requestMaterial: 'm', state: 'completed' },
    ], events: { '\0undefined\0undefined': [{ seq: 1, event: { type: 'kept-for-falsy' } }] } }),
    steps: [
      { op: 'get', input: falsy }, append(falsy, { type: 'input_accepted' }), { op: 'events', input: falsy },
      accept(falsy, 'replaces-falsy'), append(falsy, { type: 'input_accepted' }), { op: 'get', input: falsy }, { op: 'events', input: falsy },
    ],
  });
  expectKinds(['absent', 'not_found', 'events:1', 'created', 'ok', 'record', 'events:2']);
  assert.deepEqual((await afterSnapshot()).records.map((r: any) => r.runId), ['before', 'undefined', 'after']);
  // An array record is an object to JS: append's writes land on the in-memory
  // array, JSON.stringify drops them, so only the event list changes on disk.
  await scenario('quirk-array-record', {
    initial: JSON.stringify({ schemaVersion: 1, records: [
      ['x', 1],
      { sessionKey: 's', runId: 'ok', requestMaterial: 'm', state: 'completed' },
    ], events: {} }),
    steps: [
      { op: 'get', input: falsy }, accept(falsy, 'm'), append(falsy, { type: 'input_accepted' }), append(falsy, completed('undefined')),
      { op: 'get', input: falsy }, { op: 'events', input: falsy, afterSeq: -1 }, { op: 'events', input: falsy }, { op: 'mark' },
    ],
  });
  {
    const o = expectKinds(['record', 'conflict', 'ok', 'ok', 'record', 'events:2', 'events:0', 'ok']);
    assert.deepEqual(o[4].record, ['x', 1]);
    const after = await afterSnapshot();
    assert.deepEqual(after.records[0], ['x', 1]);
    assert.deepEqual(after.events['\0undefined\0undefined'].map((e: any) => e.seq), [null, null]);
  }
  await scenario('quirk-events-array', {
    initial: JSON.stringify({ schemaVersion: 1, records: [
      { sessionKey: 's', runId: 'r', requestMaterial: 'm', state: 'running', ownerPid: HUGE, revision: 1, lastSeq: 1 },
    ], events: [[{ seq: 1, event: { type: 'kept-under-index-key' } }]] }),
    steps: [append(bare('r', 's'), { type: 'input_accepted' }), { op: 'events', input: bare('r', 's') }],
  });
  expectKinds(['ok', 'events:1']);
  assert.deepEqual(Object.keys((await afterSnapshot()).events), ['0', '\0s\0r']);
  await scenario('quirk-events-string', {
    initial: JSON.stringify({ schemaVersion: 1, records: [], events: 'ab' }),
    steps: [accept(bare('q', 's'), 'q')],
  });
  expectKinds(['created']);
  // Object.entries splits a string by UTF-16 code unit: CJK, a surrogate pair
  // split into two lone halves, and a lone surrogate each become one entry.
  await scenario('quirk-events-utf16', {
    initial: JSON.stringify({ schemaVersion: 1, records: [], events: '中😀\ud800' }),
    steps: [accept(bare('q', 's'), 'q')],
  });
  expectKinds(['created']);
  assert.deepEqual(Object.values((await afterSnapshot()).events), ['中', '\ud83d', '\ude00', '\ud800']);
  await scenario('quirk-lists', {
    initial: JSON.stringify({ schemaVersion: 1, records: [
      { sessionKey: 's', runId: 'str', requestMaterial: 'm', state: 'running', ownerPid: HUGE, revision: 1, lastSeq: 0 },
      { sessionKey: 's', runId: 'nul', requestMaterial: 'm', state: 'running', ownerPid: HUGE, revision: 1, lastSeq: 1 },
      { sessionKey: 's', runId: 'ok', requestMaterial: 'm', state: 'running', ownerPid: HUGE, revision: 1, lastSeq: 0 },
      { sessionKey: 's', runId: 'nil', requestMaterial: 'm', state: 'running', ownerPid: HUGE, revision: 1, lastSeq: 0 },
    ], events: { '\0s\0str': 'oops', '\0s\0nul': [null], '\0s\0nil': null } }),
    steps: [
      append(bare('str', 's'), { type: 'input_accepted' }),
      append(bare('nul', 's'), { type: 'input_accepted' }),
      { op: 'events', input: bare('str', 's') },
      { op: 'events', input: bare('nul', 's') },
      append(bare('ok', 's'), null),
      { op: 'get', input: bare('ok', 's') },
      // A null event table reads as empty (eventMap.get(key) ?? []).
      { op: 'events', input: bare('nil', 's') },
      append(bare('nil', 's'), { type: 'input_accepted' }),
      { op: 'events', input: bare('nil', 's') },
    ],
  });
  {
    const o = expectKinds(['TypeError', 'ok', 'TypeError', 'TypeError', 'TypeError', 'record', 'events:0', 'ok', 'events:1']);
    assert.equal(o[5].record.lastSeq, 0);
    assert.deepEqual((await afterSnapshot()).events['\0s\0nul'].map((e: any) => e?.seq ?? e), [null, 2]);
  }
  // Node readFile('utf8') replaces the invalid byte; the next write re-encodes U+FFFD.
  const utf8 = JSON.stringify({ schemaVersion: 1, records: [{ sessionKey: 's', runId: 'r', requestMaterial: 'invalid-byte-marker', state: 'completed' }], events: {} }).split('invalid-byte-marker');
  await scenario('quirk-invalid-utf8', {
    initial: Buffer.concat([Buffer.from(utf8[0]), Buffer.from([0xff]), Buffer.from(utf8[1])]),
    steps: [accept(bare('r', 's'), '\ufffd'), accept(bare('q', 's'), 'q')],
  });
  expectKinds(['duplicate', 'created']);

  // Bad tail and invalid shapes: readiness rejects, bytes untouched, lock released.
  // normal.after.json carries this run's pid; seed bad-tail with it fenced to DEAD.
  const normalBytes = Buffer.from((await readFile(join(output, 'normal.after.json'), 'utf8')).replace(new RegExp(`"ownerPid":${process.pid}(?![0-9.eE])`, 'g'), `"ownerPid":${DEAD}`));
  for (const [name, initial] of Object.entries({
    'bad-tail': normalBytes.subarray(0, normalBytes.length - 9),
    'bad-empty': '',
    'bad-schema': JSON.stringify({ schemaVersion: 2, records: [], events: {} }),
    'bad-records': JSON.stringify({ schemaVersion: 1, records: {}, events: {} }),
    'bad-events-null': JSON.stringify({ schemaVersion: 1, records: [], events: null }),
    'bad-events-missing': JSON.stringify({ schemaVersion: 1, records: [] }),
    'bad-events-zero': JSON.stringify({ schemaVersion: 1, records: [], events: 0 }),
    'bad-null': 'null',
    'bad-null-record': JSON.stringify({ schemaVersion: 1, records: [null], events: {} }),
  })) {
    await scenario(name, { initial, steps: [{ op: 'get', input: p('run-1') }] });
    const last = scenarios.at(-1);
    assert.ok(last.openError, `${name} must reject`);
    assert.deepEqual(await readFile(join(output, last.after)), Buffer.from(initial));
  }

  // Lock protocol (RunRegistry.ts:110-146): only a stale (>60 s) lock of a dead
  // or unparsable owner is stolen; a stale live owner times out after 30 s.
  const lockSnapshot = await realSnapshot(async registry => {
    await registry.accept({ ...ref('locked'), requestMaterial: 'locked' });
    await registry.append({ ...ref('locked'), event: completed('locked') });
  });
  await scenario('lock-stale-dead', { initial: lockSnapshot, lock: { contents: JSON.stringify({ pid: DEAD, acquiredAt: '2026-10-18T00:00:00.000Z' }), ageMs: 120_000 }, steps: [accept(ref('after-lock'), 'x')] });
  await scenario('lock-stale-garbage', { initial: lockSnapshot, lock: { contents: 'not json', ageMs: 120_000 }, steps: [accept(ref('after-lock'), 'x')] });
  await scenario('lock-stale-live', { initial: lockSnapshot, lock: { contents: '{ "pid" :\u00a01 }', ageMs: 120_000 }, steps: [] });

  assert.equal(scenarios.length, 25, 'run-registry oracle scenario count');
  const oracle = JSON.stringify({ sourceCommit, sourceSha256, generatorPid: process.pid, dead: DEAD, huge: HUGE, scenarios }, null, 2) + '\n';
  // Fixtures are machine-independent: no generation path survives in the oracle.
  for (const dir of [scratch, output, baseline]) assert.ok(!oracle.includes(dir), `oracle.json contains ${dir}`);
  await writeFile(join(output, 'oracle.json'), oracle, { flag: 'wx' });
  console.log(`PASS: generated ${scenarios.length} real TS FileRunRegistry scenarios`);
} finally {
  globalThis.Date = NativeDate;
  await rm(scratch, { recursive: true, force: true });
}
