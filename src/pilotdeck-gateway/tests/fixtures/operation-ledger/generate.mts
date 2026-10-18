import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Run from repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
import { mkdir, mkdtemp, open, readFile, writeFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory or Rust JSONL path');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const files = ['src/agent/modules/transport/sessionOperationLedger.ts', 'src/agent/modules/transport/operationLedger.ts', 'src/session/transcript/JsonlTranscriptWriter.ts', 'src/session/transcript/TranscriptReader.ts', 'src/session/events/SessionRuntime.ts', 'src/session/events/SessionEventLogValidation.ts', 'src/session/persistence/JsonlSessionPersistence.ts', 'src/agent/modules/checkpoint/seedStateProjection.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, files);
const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { SessionAgentLoopOperationLedger: Ledger } = await load('src/agent/modules/transport/sessionOperationLedger.ts');
const { JsonlTranscriptWriter: Writer } = await load('src/session/transcript/JsonlTranscriptWriter.ts');
const { readTranscript, DEFAULT_MAX_TRANSCRIPT_READ_BYTES } = await load('src/session/transcript/TranscriptReader.ts');
const { SessionRuntime } = await load('src/session/events/SessionRuntime.ts');
const { parseAgentLoopSeedStateProjection: parseSeed, serializeAgentLoopSeedStateProjection: serializeSeed } = await load('src/agent/modules/checkpoint/seedStateProjection.ts');
const identity = { runId: 'run-1', operationId: 'op-1', requestId: 'req-1', sessionId: 'session-1', turnId: 'turn-1', binding: { moduleInstanceId: 'module-1', connectionGeneration: 'generation-1' }, idempotencyKey: 'once' };
const seed = { allowedReadFiles: ['a', 'a'], readFileState: { a: { mtimeMs: 1.25, kind: 'text', offset: 0, pages: '1', ignored: true } }, writeSnapshots: { a: { absolutePath: '/offline/a', mtimeMs: 2, contentHash: 'abc', limit: 10 } }, ignored: true };
const known = { ...identity, streamId: 'stream-1', lastAppliedSequence: 4, outcome: 'completed', result: { sessionId: identity.sessionId, turnId: identity.turnId, status: 'completed', text: '中文\ud800', nested: { z: 0, a: 1 } }, messages: [], seedState: parseSeed(seed), code: 'OK', error: null };
const unknown = { ...identity, streamId: 'stream-1', lastAppliedSequence: 2, code: 'DISCONNECTED', error: { message: 'lost' } };
const projected = (recovery: any) => recovery?.state === 'terminal' && recovery.resolution.seedState ? { ...recovery, resolution: { ...recovery.resolution, seedState: serializeSeed(recovery.resolution.seedState) } } : recovery ?? null;
function restore(entries: any[]) {
  const runtime = new SessionRuntime();
  runtime.restore(entries);
  const ledger = new Ledger({ sessionId: identity.sessionId, transcript: new Writer({ path: 'unused', eventStore: runtime }), restoredEntries: entries });
  return { runtime, ledger };
}
if (process.argv[3] === '--verify-rust') {
  const read = await readTranscript(output);
  assert.equal(read.diagnostics.length, 0);
  const { runtime, ledger } = restore(read.entries);
  assert.deepEqual(projected(ledger.recover({ ...identity, requestId: 'new-attempt', binding: { moduleInstanceId: 'new-module', connectionGeneration: 'new-generation' } })), { state: 'terminal', resolution: { outcome: known.outcome, result: known.result, messages: known.messages, seedState: serializeSeed(known.seedState) } });
  assert.equal(runtime.snapshotState().sequence, 4);
  assert.equal(read.entries[0].parentEntryId, null);
  for (let i = 1; i < read.entries.length; i++) assert.equal(read.entries[i].parentEntryId, read.entries[i - 1].entryId);
  for (const entry of read.entries) assert.equal(new Date(entry.createdAt).toISOString(), entry.createdAt);
  // Continue with the real TS writer on Rust's file, including parent/sequence.
  const writer = new Writer({ path: output, now: () => new Date('2026-10-17T00:00:00.000Z'), uuid: () => 'ts-after-rust' });
  const state = runtime.snapshotState();
  writer.restoreState(state.sequence, state.lastEntryId);
  const continued = new Ledger({ sessionId: identity.sessionId, transcript: writer, restoredEntries: read.entries });
  await continued.start({ ...identity, operationId: 'op-2', requestId: 'req-2' });
  await writer.close();
  assert.equal((await readTranscript(output)).entries.at(-1).sequence, 5);
  console.log('PASS: real TS reader/runtime/ledger recovered Rust terminal and TS writer continued sequence/parent chain');
  process.exit(0);
}
await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const cases = [];
try {
  for (const name of ['normal', 'unknown', 'incomplete', 'bad-tail', 'partial', 'legacy', 'bad-middle', 'known-then-unknown', 'seed-order', 'unknown-nonfinite']) {
    const path = join(scratch, name + '.jsonl');
    let counter = 0;
    const writer = new Writer({ path, now: () => new Date('2026-10-17T00:00:00.000Z'), uuid: () => `ts-entry-${++counter}` });
    const ledger = new Ledger({ sessionId: identity.sessionId, transcript: writer });
    await ledger.start(identity);
    await ledger.accept(identityWithStream());
    if (!['incomplete', 'partial'].includes(name)) {
      if (name === 'unknown') await ledger.resultUnknown(unknown);
      else if (name === 'unknown-nonfinite') await ledger.resultUnknown({ ...unknown, lastAppliedSequence: NaN });
      else if (name === 'seed-order') {
        const raw = { ...known, seedState: { readFileState: new Map([['a', { kind: 'text', mtimeMs: 1 }]]) } };
        await ledger.terminal(raw);
        await assert.rejects(ledger.terminal(raw), /different durable terminal/);
      } else await ledger.terminal(known);
    }
    await writer.close();
    let bytes = await readFile(path, 'utf8');
    if (name === 'bad-tail') bytes += '{bad json}\n{"type":"not-an-envelope"}\n';
    if (name === 'partial') bytes += '{"type":"agent_loop_operation_terminal","outcome":"completed"';
    if (name === 'bad-middle') bytes = bytes.replace('\n', '\n{bad middle}\n');
    if (name === 'legacy') {
      const entries = bytes.trimEnd().split('\n').map(line => { const e = JSON.parse(line); delete e.entryId; delete e.parentEntryId; return e; });
      entries.push({ type: 'session_metadata', sessionId: 'other-session', turnId: '', sequence: 9, createdAt: '2026-10-17T00:00:00.000Z', metadata: {} });
      bytes = entries.map(e => JSON.stringify(e)).join('\n') + '\n';
    }
    if (name === 'known-then-unknown') {
      const temporary = join(scratch, 'unknown-source.jsonl');
      const w = new Writer({ path: temporary });
      const l = new Ledger({ sessionId: identity.sessionId, transcript: w });
      await l.start(identity); await l.accept(identityWithStream()); await l.resultUnknown(unknown); await w.close();
      const terminal = (await readTranscript(temporary)).entries.at(-1);
      bytes += JSON.stringify({ ...terminal, sequence: 4, entryId: 'ts-entry-4', parentEntryId: 'ts-entry-3', createdAt: '2026-10-17T00:00:00.000Z' }) + '\n';
    }
    await writeFile(join(output, name + '.jsonl'), bytes);
    const read = await readTranscript(join(output, name + '.jsonl'));
    const { runtime, ledger: restored } = restore(read.entries);
    cases.push({ name, file: name + '.jsonl', diagnostics: read.diagnostics.map(({ code, severity, line }: any) => ({ code, severity, line })), state: runtime.snapshotState(), recovery: projected(restored.recover(identity)) });
  }
  // TS's actual first append after a crash keeps the incomplete bytes, adding LF.
  const partialPath = join(output, 'partial.jsonl');
  const partialRead = await readTranscript(partialPath);
  const partialState = restore(partialRead.entries).runtime.snapshotState();
  const continuationPath = join(scratch, 'continuation.jsonl');
  await writeFile(continuationPath, await readFile(partialPath));
  const writer = new Writer({ path: continuationPath, now: () => new Date('2026-10-17T00:00:00.000Z'), uuid: () => 'ts-entry-3' });
  writer.restoreState(partialState.sequence, partialState.lastEntryId);
  const ledger = new Ledger({ sessionId: identity.sessionId, transcript: writer, restoredEntries: partialRead.entries });
  await ledger.resultUnknown(unknown); await writer.close();
  await writeFile(join(output, 'partial-continued.jsonl'), await readFile(continuationPath));
  // Mutate real TS envelopes, then ask the real ledger to decide admission.
  // Identity includes transport-attempt fields, even though recover() is durable-only.
  const normal = (await readTranscript(join(output, 'normal.jsonl'))).entries;
  const rejectedCases = [];
  const identityFields = ['requestId', 'runId', 'turnId', 'idempotencyKey', 'key-absent', 'key-empty', 'moduleInstanceId', 'connectionGeneration'];
  for (const kind of ['accepted', 'terminal']) {
    for (const field of identityFields) {
      const entries = structuredClone(normal.slice(0, kind === 'accepted' ? 2 : 3));
      const changed = entries.at(-1);
      if (field === 'key-absent') delete changed.idempotencyKey;
      else if (field === 'key-empty') changed.idempotencyKey = '';
      else if (field === 'moduleInstanceId' || field === 'connectionGeneration') changed.binding[field] = 'conflict';
      else changed[field] = 'conflict';
      const name = `identity-${kind}-${field}`;
      const file = name + '.jsonl';
      await writeFile(join(output, file), entries.map(e => JSON.stringify(e)).join('\n') + '\n');
      const read = await readTranscript(join(output, file));
      assert.equal(read.diagnostics.length, 0);
      let error = '';
      try { new Ledger({ sessionId: identity.sessionId, transcript: writer, restoredEntries: read.entries }); }
      catch (caught) { error = (caught as Error).message; }
      assert.match(error, /before it started/);
      rejectedCases.push({ name, file, error });
      console.log(`${name}: ${error}`);
    }
  }
  const seedCases = [];
  for (const [name, input] of Object.entries({
    'invalid-kind': { readFileState: { a: { mtimeMs: 1, kind: 'other' } } },
    'string-mtime': { readFileState: { a: { mtimeMs: '1', kind: 'text' } } },
    'invalid-write': { writeSnapshots: { a: { mtimeMs: 1 } } },
    'null-fields': { allowedReadFiles: null, readFileState: null, writeSnapshots: null },
    'falsy-fields': { allowedReadFiles: false, readFileState: 0, writeSnapshots: '' },
    'optional-fields': { readFileState: { a: { kind: 'text', mtimeMs: 1, offset: 'ignored', ignored: true } } },
  })) {
    const path = join(scratch, `seed-${name}.jsonl`);
    let counter = 0;
    const w = new Writer({ path, now: () => new Date('2026-10-17T00:00:00.000Z'), uuid: () => `ts-entry-${++counter}` });
    const l = new Ledger({ sessionId: identity.sessionId, transcript: w });
    await l.start(identity); await l.accept(identityWithStream());
    const seedState: any = { ...input };
    for (const field of ['readFileState', 'writeSnapshots']) {
      if (seedState[field]) seedState[field] = new Map(Object.entries(seedState[field]));
    }
    let onlineError = null;
    try { await l.terminal({ ...known, seedState }); }
    catch (caught) { onlineError = (caught as Error).message; }
    await w.close();
    const file = `seed-${name}.jsonl`;
    await writeFile(join(output, file), await readFile(path));
    const read = await readTranscript(path);
    let restoreError = null;
    let recovery = null;
    try { recovery = projected(restore(read.entries).ledger.recover(identity)); }
    catch (caught) { restoreError = (caught as Error).message; }
    assert.equal(read.entries.length, 3, 'TS serializes before cloning/validating seed Map values');
    seedCases.push({ name, file, input, onlineError, restoreError, entryCount: read.entries.length, recovery });
    console.log(`seed-${name}: online=${onlineError ?? 'OK'} restore=${restoreError ?? 'OK'} entries=${read.entries.length}`);
  }
  // Node readFile('utf8') replaces invalid bytes; envelope dates are strings only.
  const utf8Entry = { ...normal[0], createdAt: 'not-a-date', annotation: 'invalid-byte-marker' };
  const utf8Parts = JSON.stringify(utf8Entry).split('invalid-byte-marker');
  const utf8Bytes = Buffer.concat([Buffer.from(utf8Parts[0]), Buffer.from([0xff]), Buffer.from(utf8Parts[1] + '\n')]);
  const utf8File = 'invalid-utf8.jsonl';
  await writeFile(join(output, utf8File), utf8Bytes);
  const utf8Read = await readTranscript(join(output, utf8File));
  assert.equal(utf8Read.entries[0].annotation, '\ufffd');
  assert.equal(utf8Read.diagnostics.length, 0);
  const utf8State = restore(utf8Read.entries).runtime.snapshotState();
  console.log(`invalid-utf8: annotation=U+FFFD createdAt=${utf8Read.entries[0].createdAt} diagnostics=0 sequence=${utf8State.sequence}`);
  // Sparse files exercise the real reader's exact limit without storing 50 MiB fixtures.
  assert.equal(DEFAULT_MAX_TRANSCRIPT_READ_BYTES, 50 * 1024 * 1024);
  const readLimits = [];
  for (const sizeBytes of [DEFAULT_MAX_TRANSCRIPT_READ_BYTES, DEFAULT_MAX_TRANSCRIPT_READ_BYTES + 1]) {
    const path = join(scratch, `read-limit-${sizeBytes}.jsonl`);
    const file = await open(path, 'wx');
    try { await file.truncate(sizeBytes); } finally { await file.close(); }
    const read = await readTranscript(path);
    readLimits.push({ sizeBytes, entryCount: read.entries.length, diagnostics: read.diagnostics.map(({ code, severity, line }: any) => ({ code, severity, line })) });
  }
  await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, inputs: { identity, known: { ...known, seedState: serializeSeed(known.seedState) }, unknown }, cases, readLimits, rejectedCases, seedCases, utf8Read: { file: utf8File, entries: utf8Read.entries, diagnostics: utf8Read.diagnostics, state: utf8State } }, null, 2) + '\n');
  console.log(`PASS: generated ${cases.length} real TS ledger fixtures, ${rejectedCases.length} rejected identities, ${seedCases.length} seed cases, UTF-8 case, partial continuation and ${readLimits.length} sparse read limits`);
} finally { await rm(scratch, { recursive: true, force: true }); }
function identityWithStream() { return { ...identity, streamId: 'stream-1' }; }
