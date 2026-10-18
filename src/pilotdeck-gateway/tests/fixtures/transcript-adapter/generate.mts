// Run from repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
import { access, appendFile, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory or a cross-test scratch directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', 'baseline must be the fixed PilotDeck commit');
const SOURCES = [
  'src/gateway/storage/AsyncTranscriptStorageAdapter.ts',
  'src/session/transcript/JsonlTranscriptWriter.ts',
  'src/session/events/SessionRuntime.ts',
  'src/session/events/SessionEventLogValidation.ts',
  'src/session/events/SessionDomainValidation.ts',
  'src/session/storage/ProjectSessionStorage.ts',
  'src/session/persistence/JsonlSessionPersistence.ts',
  'src/session/transcript/TranscriptReader.ts',
  'src/session/projection/AgentTranscriptProjections.ts',
  'src/pilot/paths.ts',
];
const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { createGatewayAsyncTranscriptStorageAdapter } = await load(SOURCES[0]);
const { JsonlSessionPersistence } = await load('src/session/persistence/JsonlSessionPersistence.ts');
const { readTranscript } = await load('src/session/transcript/TranscriptReader.ts');

const NOW = '2026-10-18T00:00:00.000Z';
const now = () => new Date(NOW);
// A fixed absolute project key keeps the slugged project id machine-independent;
// the project directory itself is never touched by transcript storage.
const PROJECT_ROOT = '/pdrs-fixture/project';
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}/g;

/**
 * Host store for the fixtures: the TS default provider behind the async
 * contract (JsonlSessionPersistence appends, readTranscript reads), plus a
 * call log and injected failures. Only listed capabilities exist.
 */
function fixtureStore(config: any) {
  const calls: any[] = [];
  let appends = 0;
  let reads = 0;
  const persistences = new Map<string, any>();
  const log = (method: string, key: any, extra: any = {}) => calls.push({ method, key: { ...key }, ...extra });
  const store: any = {
    async append(key: any, entry: any) {
      const n = ++appends;
      log('append', key, { entry });
      if (config.failBefore?.includes(n)) throw new Error(`injected append failure ${n}`);
      let persistence = persistences.get(key.transcriptPath);
      if (!persistence) persistences.set(key.transcriptPath, persistence = new JsonlSessionPersistence({ path: key.transcriptPath }));
      await persistence.append(entry);
      if (config.failAfter?.includes(n)) throw new Error(`injected append failure ${n} after write`);
    },
    async read(key: any, options: any) {
      const n = ++reads;
      log('read', key, options === undefined ? {} : { options });
      if (config.failRead?.includes(n)) throw new Error(`injected read failure ${n}`);
      return readTranscript(key.transcriptPath, options);
    },
  };
  const caps: string[] = config.caps ?? [];
  if (caps.includes('has')) store.has = async (key: any) => { log('has', key); return access(key.transcriptPath).then(() => true, () => false); };
  if (caps.includes('delete')) store.delete = async (key: any) => { log('delete', key); await rm(key.transcriptPath, { force: true }); };
  if (caps.includes('deleteSession')) store.deleteSession = async (key: any) => {
    log('deleteSession', key);
    await rm(key.transcriptPath, { force: true });
    await rm(join(dirname(key.transcriptPath), basename(key.transcriptPath).replace(/\.jsonl$/, '')), { recursive: true, force: true });
  };
  if (caps.includes('replace')) store.replace = async (key: any, entries: any[]) => {
    log('replace', key, { entries });
    await mkdir(dirname(key.transcriptPath), { recursive: true, mode: 0o700 });
    await writeFile(key.transcriptPath, entries.map((entry) => `${JSON.stringify(entry)}\n`).join(''), { mode: 0o600 });
  };
  if (config.list) store.list = async (key: any) => { log('list', key); return config.list.map((record: any) => ({ ...record })); };
  return { store, calls };
}

const ok = { ok: true };
const errorOf = (error: any) => ({ error: { name: error.name, ...(error.code === undefined ? {} : { code: error.code }), message: error.message } });
// V8 JSON.parse wording is not claimed; every other diagnostic message is compared.
const diagnostic = (d: any) => ({ code: d.code, severity: d.severity, ...(d.line === undefined ? {} : { line: d.line }), ...(d.code === 'transcript_line_invalid' ? {} : { message: d.message }) });
const readOut = (read: any) => ({ entries: read.entries, diagnostics: read.diagnostics.map(diagnostic) });
const CAPABILITIES = ['transcriptExists', 'deleteTranscript', 'deleteSessionTranscripts', 'replaceTranscript'];

async function runStep(ctx: any, step: any): Promise<any> {
  try {
    const writer = step.writer ? ctx.writers[step.writer].writer : ctx.storage?.transcript;
    const sessionId = ctx.sessionId;
    switch (step.op) {
      case 'open': {
        ctx.sessionId = step.sessionId;
        const s = ctx.storage = ctx.adapter.createSessionStorage({ projectRoot: PROJECT_ROOT, pilotHome: ctx.pilotHome, sessionId: step.sessionId, now });
        return {
          paths: { chatDir: s.chatDir, transcriptPath: s.transcriptPath, projectionCheckpointPath: s.projectionCheckpointPath, toolResultsDir: s.toolResultsDir, fileHistoryDir: s.fileHistoryDir, subagentsDir: s.subagentsDir },
          externalTranscriptStore: s.externalTranscriptStore,
          capabilities: CAPABILITIES.filter((name) => typeof s[name] === 'function'),
        };
      }
      case 'restore': {
        const read = await ctx.storage.restore();
        return { ...readOut(read), state: ctx.storage.transcript.snapshotState() };
      }
      case 'accepted': await writer.recordAcceptedInput(sessionId, step.turnId, step.messages, step.metadata); return ok;
      case 'metadata': await writer.recordSessionMetadata(sessionId, step.turnId, step.metadata); return ok;
      case 'status': await writer.recordAgentStatusMessage(sessionId, step.turnId, step.status); return ok;
      case 'turnResult': await writer.recordTurnResult(sessionId, step.turnId, step.result); return ok;
      case 'event': await writer.recordSessionEvent(sessionId, step.turnId, step.event); return ok;
      case 'entry': await writer.recordEntry(step.entry); return ok;
      case 'restoreState': writer.restoreState(step.sequence, step.lastEntryId); return ok;
      case 'state': return { state: writer.snapshotState() };
      case 'close': await writer.close(); return ok;
      case 'read': return readOut(await ctx.storage.readTranscript(step.maxBytes === undefined ? undefined : { maxBytes: step.maxBytes }));
      case 'readAt': return readOut(await ctx.storage.readTranscriptAtPath(join(ctx.chatDir, step.path)));
      case 'has': return { exists: await ctx.storage.transcriptExists() };
      case 'delete': await ctx.storage.deleteTranscript(); return ok;
      case 'deleteSession': await ctx.storage.deleteSessionTranscripts(); return ok;
      case 'replace': await ctx.storage.replaceTranscript(step.entries); return ok;
      case 'sidechain': {
        const handle = writer.forSubagent(step.subagentId);
        ctx.writers[step.as] = handle;
        return { subagentId: handle.subagentId, transcriptPath: handle.transcriptPath, relative: writer.relativeSubagentPath(step.subagentId) };
      }
      case 'list': {
        if (!ctx.adapter.listSessions) return { absent: true };
        return { sessions: await ctx.adapter.listSessions({ projectRoot: PROJECT_ROOT, pilotHome: ctx.pilotHome, ...(step.limit === undefined ? {} : { limit: step.limit }), ...(step.offset === undefined ? {} : { offset: step.offset }) }) };
      }
      case 'concurrent': {
        // Every append is enqueued synchronously before any of them settles.
        const started = step.steps.map((inner: any) => runStep(ctx, inner));
        return { outcomes: await Promise.all(started) };
      }
      default: throw new Error(`unknown step ${step.op}`);
    }
  } catch (error: any) {
    return errorOf(error);
  }
}

// Every file under the scenario dir (UTF-8 byte order), so stray temporaries fail too.
async function snapshotFiles(root: string): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  const walk = async (rel: string) => {
    const entries = await readdir(join(root, rel), { withFileTypes: true });
    entries.sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
    for (const entry of entries) {
      const child = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await walk(child);
      else out[child] = await readFile(join(root, child), 'utf8');
    }
  };
  await walk('');
  return out;
}

/** Machine-independent text: scenario dir → "<dir>", each v4 UUID → "<id-N>" by first appearance. */
function normalize(value: any, dir: string): any {
  const ids = new Map<string, string>();
  const text = JSON.stringify(value).split(dir).join('<dir>').replace(UUID, (id) => {
    if (!ids.has(id)) ids.set(id, `<id-${ids.size + 1}>`);
    return ids.get(id)!;
  });
  return JSON.parse(text);
}

async function runScenario(scratch: string, name: string, config: any) {
  const dir = join(scratch, name);
  await mkdir(dir, { recursive: true });
  const pilotHome = join(dir, 'home');
  const { store, calls } = fixtureStore(config.store ?? {});
  const adapter = createGatewayAsyncTranscriptStorageAdapter({ store });
  const chatDir = adapter.getProjectChatDir({ projectRoot: PROJECT_ROOT, pilotHome });
  for (const [rel, text] of Object.entries(config.files ?? {})) {
    const path = join(chatDir, rel);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, text as string);
  }
  const ctx = { adapter, pilotHome, chatDir, storage: undefined, sessionId: undefined, writers: {} };
  const steps = [];
  for (const step of config.steps) steps.push({ ...step, outcome: await runStep(ctx, step) });
  return normalize({ name, store: config.store ?? {}, initialFiles: config.files ?? {}, chatDir, steps, calls, files: await snapshotFiles(dir) }, dir);
}

const user = (text: string) => ({ role: 'user', content: [{ type: 'text', text }] });
const turnResult = { sessionId: 's1', turnId: 'turn-1', status: 'completed', usage: { inputTokens: 1 }, permissionDenials: [] };
const seed = (sequence: number, extra: any = {}) => ({ type: 'session_metadata', sessionId: 's1', turnId: 'turn-0', sequence, createdAt: NOW, entryId: `seed-${sequence}`, parentEntryId: sequence === 1 ? null : `seed-${sequence - 1}`, metadata: {}, ...extra });
const line = (entry: any) => `${JSON.stringify(entry)}\n`;
const open = (sessionId = 's1') => ({ op: 'open', sessionId });
const accepted = (text: string, turnId = 'turn-1', metadata?: any) => ({ op: 'accepted', turnId, messages: [user(text)], ...(metadata === undefined ? {} : { metadata }) });
const metadata = (value: any, extra: any = {}) => ({ op: 'metadata', turnId: 'turn-1', metadata: value, ...extra });
const ALL = ['has', 'delete', 'deleteSession', 'replace'];

const kind = (o: any): string =>
  o.error ? `error:${o.error.code ?? o.error.name}`
  : o.outcomes ? `concurrent:${o.outcomes.map(kind).join('|')}`
  : o.paths ? `open:${o.capabilities.length}`
  : o.state && o.entries ? `restored:${o.entries.length}/${o.diagnostics.length}@${o.state.sequence}`
  : o.state ? `state:${o.state.sequence}`
  : o.entries ? `read:${o.entries.length}/${o.diagnostics.length}`
  : 'exists' in o ? `exists:${o.exists}`
  : o.absent ? 'absent'
  : o.sessions ? `list:${o.sessions.map((s: any) => s.sessionId).join(',')}`
  : o.relative !== undefined ? 'sidechain'
  : 'ok';

// --- TS↔Rust cross test (driven by the ignored Rust test) ---
const crossCtx = async (dir: string) => {
  const pilotHome = join(dir, 'home');
  const { store } = fixtureStore({});
  const adapter = createGatewayAsyncTranscriptStorageAdapter({ store });
  const ctx: any = { adapter, pilotHome, chatDir: adapter.getProjectChatDir({ projectRoot: PROJECT_ROOT, pilotHome }), writers: {} };
  return ctx;
};
const must = (outcome: any) => { assert.ok(!outcome.error, JSON.stringify(outcome)); return outcome; };
if (process.argv[3] === '--cross-ts-write') {
  const ctx = await crossCtx(output);
  must(await runStep(ctx, open('cross')));
  must(await runStep(ctx, accepted('from TS')));
  must(await runStep(ctx, metadata({ title: 'TS title' })));
  must(await runStep(ctx, { op: 'sidechain', subagentId: 'kid', as: 'kid' }));
  must(await runStep(ctx, metadata({ tag: 'ts-kid' }, { writer: 'kid' })));
  // Crash debris: an unterminated record at the primary tail.
  await appendFile(ctx.storage.transcriptPath, '{"type":"partial');
  console.log(JSON.stringify({ chatDir: ctx.chatDir }));
  process.exit(0);
}
if (process.argv[3] === '--cross-ts-verify') {
  const ctx = await crossCtx(output);
  must(await runStep(ctx, open('cross')));
  const restored = must(await runStep(ctx, { op: 'restore' }));
  assert.equal(restored.entries.length, 3, 'two TS entries and one Rust entry');
  assert.deepEqual(restored.diagnostics.map((d: any) => [d.code, d.line]), [['transcript_line_invalid', 3]]);
  assert.equal(restored.state.sequence, 3);
  for (let i = 1; i < 3; i++) assert.equal(restored.entries[i].parentEntryId, restored.entries[i - 1].entryId);
  assert.equal(restored.entries[2].metadata.title, 'from Rust');
  const kid = must(await runStep(ctx, { op: 'readAt', path: 'cross/subagents/kid.jsonl' }));
  assert.deepEqual(kid.entries.map((e: any) => [e.sequence, e.metadata.tag]), [[1, 'ts-kid'], [2, 'rust-kid']]);
  assert.equal(kid.entries[1].parentEntryId, kid.entries[0].entryId);
  assert.equal(kid.entries[1].sessionId, 'cross');
  must(await runStep(ctx, metadata({ aiTitle: 'TS after Rust' })));
  assert.equal(must(await runStep(ctx, { op: 'state' })).state.sequence, 4);
  console.log('PASS: real TS adapter restored Rust appends past crash debris, continued the sequence/parent chain and read the Rust sidechain continuation');
  process.exit(0);
}

await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const scenarios: any[] = [];
async function scenario(name: string, config: any, expected: string[]) {
  const result = await runScenario(scratch, name, config);
  scenarios.push(result);
  const kinds = result.steps.map((s: any) => kind(s.outcome));
  console.log(`${name}: ${kinds.join(' ')}`);
  // Every step must reach the branch the scenario claims.
  assert.deepEqual(kinds, expected, name);
  return result;
}
const appendCalls = (result: any) => result.calls.filter((c: any) => c.method === 'append');

try {
  {
    const r = await scenario('normal', {
      store: { caps: ALL },
      steps: [
        open(), { op: 'restore' }, { op: 'has' },
        accepted(' hello ', 'turn-1', {}),
        metadata({ title: 'T', tag: 'x' }),
        { op: 'status', turnId: 'turn-1', status: { event: 'e', kind: 'status', text: 'x', detail: {} } },
        { op: 'turnResult', turnId: 'turn-1', result: turnResult },
        { op: 'state' }, { op: 'read' }, { op: 'has' },
        { op: 'readAt', path: 's1/subagents/none.jsonl' },
      ],
    }, ['open:4', 'restored:0/1@0', 'exists:false', 'ok', 'ok', 'ok', 'ok', 'state:4', 'read:4/0', 'exists:true', 'read:0/1']);
    const entries = r.steps[8].outcome.entries;
    assert.deepEqual(entries.map((e: any) => e.sequence), [1, 2, 3, 4]);
    assert.equal(entries[0].parentEntryId, null);
    for (let i = 1; i < 4; i++) assert.equal(entries[i].parentEntryId, entries[i - 1].entryId);
    assert.equal('metadata' in entries[0], false, 'empty accepted metadata is omitted');
    assert.equal('detail' in entries[2], false, 'empty status detail is omitted');
  }

  await scenario('restart', {
    steps: [
      open(), accepted('a'), metadata({ title: 'A' }),
      open(), { op: 'restore' }, metadata({ aiTitle: 'B' }), { op: 'state' },
      // A storage that skips restore starts at sequence 1 again (TS writes it).
      open(), metadata({ tag: 'no-restore' }),
      open(), { op: 'restore' },
    ],
  }, ['open:0', 'ok', 'ok', 'open:0', 'restored:2/0@2', 'ok', 'state:3', 'open:0', 'ok', 'open:0', 'error:sequence_not_increasing']);

  {
    const r = await scenario('append-failure', {
      store: { failBefore: [2] },
      steps: [open(), { op: 'restore' }, accepted('a'), metadata({ title: 'lost' }), { op: 'state' }, metadata({ title: 'kept' }), { op: 'state' }, { op: 'read' }],
    }, ['open:0', 'restored:0/1@0', 'ok', 'error:Error', 'state:1', 'ok', 'state:2', 'read:2/0']);
    const calls = appendCalls(r);
    assert.equal(calls.length, 3);
    assert.equal(calls[1].entry.sequence, 2);
    assert.equal(calls[2].entry.sequence, 2, 'a failed append does not publish its sequence');
    assert.equal(calls[2].entry.parentEntryId, calls[0].entry.entryId);
  }

  await scenario('append-uncertain', {
    store: { failAfter: [1] },
    steps: [open(), accepted('written then failed'), { op: 'state' }, metadata({ title: 'second' }), { op: 'read' }, open(), { op: 'restore' }],
  }, ['open:0', 'error:Error', 'state:0', 'ok', 'read:2/0', 'open:0', 'error:sequence_not_increasing']);

  {
    const r = await scenario('concurrent', {
      store: { failBefore: [2] },
      steps: [
        open(),
        { op: 'concurrent', steps: [accepted('one'), metadata({ title: 'two' }), { op: 'status', turnId: 'turn-1', status: { event: 'e', kind: 'error', text: 'three', detail: { n: 3 } } }, { op: 'turnResult', turnId: 'turn-1', result: turnResult }] },
        { op: 'read' }, { op: 'state' },
      ],
    }, ['open:0', 'concurrent:ok|error:Error|ok|ok', 'read:3/0', 'state:3']);
    assert.deepEqual(r.steps[2].outcome.entries.map((e: any) => [e.sequence, e.type]), [[1, 'accepted_input'], [2, 'agent_status_message'], [3, 'turn_result']]);
  }

  await scenario('bad-tail', {
    files: { 's1.jsonl': line(seed(1)) + '{"type":1}\n' + line(seed(2)) + '{"type":"session_metadata","sess' },
    steps: [open(), { op: 'restore' }, metadata({ title: 'after debris' }), { op: 'read' }],
  }, ['open:0', 'restored:2/2@2', 'ok', 'read:3/2']);

  await scenario('legacy', {
    files: { 's1.jsonl': [
      { type: 'accepted_input', sessionId: 's1', turnId: 'turn-0', sequence: 1, createdAt: NOW, messages: [user('old')] },
      { type: 'session_metadata', sessionId: 's1', turnId: 'turn-0', sequence: 2, createdAt: NOW, metadata: { title: 'old' } },
      { type: 'session_metadata', sessionId: 'other-session', turnId: '', sequence: 3, createdAt: NOW, metadata: {} },
    ].map(line).join('') },
    steps: [open(), { op: 'restore' }, metadata({ tag: 'new' }), { op: 'read' }],
  }, ['open:0', 'restored:3/0@3', 'ok', 'read:4/0']);

  for (const [name, text, code] of [
    ['restore-seq-zero', line(seed(0)), 'invalid_sequence'],
    ['restore-seq-negative-zero', '{"type":"session_metadata","sessionId":"s1","turnId":"t","sequence":-0,"createdAt":"' + NOW + '"}\n', 'invalid_sequence'],
    ['restore-seq-fraction', line(seed(1.5)), 'invalid_sequence'],
    ['restore-seq-unsafe', line(seed(9007199254740992)), 'invalid_sequence'],
    ['restore-entry-id-blank', line(seed(1, { entryId: ' 　' })), 'invalid_entry_id'],
    ['restore-entry-id-null', line(seed(1, { entryId: null })), 'invalid_entry_id'],
    ['restore-parent-number', line(seed(1, { parentEntryId: 5 })), 'invalid_parent_entry_id'],
    ['restore-self-parent', line(seed(1, { entryId: 'a', parentEntryId: 'a' })), 'self_parent'],
    ['restore-duplicate-id', line(seed(1, { entryId: 'a' })) + line(seed(2, { entryId: 'a', parentEntryId: 'a-parent' })), 'duplicate_entry_id'],
  ]) {
    await scenario(name, { files: { 's1.jsonl': text }, steps: [open(), { op: 'restore' }, { op: 'state' }] }, ['open:0', `error:${code}`, 'state:0']);
  }

  const recorded = (sequence: any, extra: any = {}) => ({ op: 'entry', entry: { type: 'session_metadata', sessionId: 's1', turnId: 't', sequence, createdAt: NOW, entryId: `r-${sequence}`, parentEntryId: null, metadata: { title: 'R' }, ...extra } });
  await scenario('record-entry', {
    steps: [
      open(), { op: 'restore' }, recorded(5), { op: 'state' },
      recorded(5, { entryId: 'r-6' }), recorded(6, { entryId: 'r-5' }), recorded('7'),
      recorded(7, { entryId: '' }), recorded(7, { entryId: 'z', parentEntryId: 'z' }), recorded(7, { parentEntryId: 3 }),
      metadata({ title: 'after recorded' }), { op: 'state' }, { op: 'read' },
    ],
  }, ['open:0', 'restored:0/1@0', 'ok', 'state:5', 'error:sequence_not_increasing', 'error:duplicate_entry_id', 'error:invalid_sequence', 'error:invalid_entry_id', 'error:self_parent', 'error:invalid_parent_entry_id', 'ok', 'state:6', 'read:2/0']);

  await scenario('restore-state', {
    steps: [
      open(),
      { op: 'restoreState', sequence: -1, lastEntryId: null }, { op: 'restoreState', sequence: 1.5, lastEntryId: null },
      { op: 'restoreState', sequence: 3, lastEntryId: '' }, { op: 'restoreState', sequence: 3, lastEntryId: '  ' },
      { op: 'restoreState', sequence: 3, lastEntryId: 'x' }, { op: 'state' }, metadata({ title: 'four' }),
      { op: 'restoreState', sequence: 0, lastEntryId: null }, metadata({ title: 'one' }), { op: 'read' },
    ],
  }, ['open:0', 'error:invalid_sequence', 'error:invalid_sequence', 'error:invalid_entry_id', 'error:invalid_entry_id', 'ok', 'state:3', 'ok', 'ok', 'ok', 'read:2/0']);

  {
    const r = await scenario('closed', {
      steps: [open(), metadata({ title: 'before close' }), { op: 'close' }, metadata({ title: 'after close' }), { op: 'state' }, { op: 'read' }],
    }, ['open:0', 'ok', 'ok', 'ok', 'state:1', 'read:1/0']);
    assert.equal(appendCalls(r).length, 1, 'appends after close never reach the store');
  }

  {
    const r = await scenario('sidechain', {
      steps: [
        open('web:s/1'),
        { op: 'sidechain', subagentId: 'sub/a', as: 'sub' },
        metadata({ tag: 'child' }, { writer: 'sub' }), { op: 'state', writer: 'sub' }, { op: 'state' },
        { op: 'sidechain', subagentId: 'n/x', as: 'nested', writer: 'sub' },
        metadata({ tag: 'grandchild' }, { writer: 'nested' }),
        { op: 'sidechain', subagentId: 'sub/a', as: 'again' },
        metadata({ tag: 'fresh writer' }, { writer: 'again' }),
        { op: 'readAt', path: 'web:s-1/subagents/sub-a.jsonl' },
        { op: 'readAt', path: 'web:s-1/subagents/sub-a/subagents/n/x.jsonl' },
      ],
    }, ['open:0', 'sidechain', 'ok', 'state:1', 'state:0', 'sidechain', 'ok', 'sidechain', 'ok', 'read:2/0', 'read:1/0']);
    for (const call of appendCalls(r)) assert.equal(call.key.sessionId, 'web:s/1', 'sidechain keys carry the parent session id');
  }

  await scenario('domain', {
    steps: [
      open(),
      { op: 'event', turnId: 'turn-1', event: { type: 'durable_message' } },
      { op: 'event', turnId: 'turn-1', event: { type: 'durable_message', message: null } },
      { op: 'event', turnId: 'turn-1', event: { type: 'durable_message', message: { role: 'user', content: [], metadata: { queueItemId: 'q' } } } },
      { op: 'turnResult', turnId: 'legacy-turn', result: turnResult },
      { op: 'read' }, open(), { op: 'restore' },
    ],
  }, ['open:0', 'error:TypeError', 'error:TypeError', 'ok', 'ok', 'read:2/0', 'open:0', 'restored:2/0@2']);

  await scenario('no-capabilities', {
    steps: [open(), { op: 'list' }, { op: 'restore' }],
  }, ['open:0', 'absent', 'restored:0/1@0']);

  await scenario('replace-delete', {
    store: { caps: ALL },
    steps: [
      open(), accepted('a'),
      { op: 'replace', entries: [seed(1, { entryId: 'rep-1' }), seed(2, { entryId: 'rep-2', parentEntryId: 'rep-1' })] },
      { op: 'read' },
      // Replacement does not reset the writer: its next sequence is 2 again.
      metadata({ title: 'after replace' }), { op: 'read' },
      { op: 'has' }, { op: 'delete' }, { op: 'has' },
      metadata({ title: 'recreated' }),
      { op: 'sidechain', subagentId: 'kid', as: 'kid' }, metadata({ tag: 'kid' }, { writer: 'kid' }),
      { op: 'deleteSession' }, { op: 'has' }, { op: 'read' }, { op: 'readAt', path: 's1/subagents/kid.jsonl' },
    ],
  }, ['open:4', 'ok', 'ok', 'read:2/0', 'ok', 'read:3/0', 'exists:true', 'ok', 'exists:false', 'ok', 'sidechain', 'ok', 'ok', 'exists:false', 'read:0/1', 'read:0/1']);

  await scenario('read-limit', {
    steps: [open(), accepted('a'), { op: 'read', maxBytes: 10 }, { op: 'read' }],
  }, ['open:0', 'ok', 'read:0/1', 'read:1/0']);

  const at = (sequence: number, type: string, extra: any, createdAt = NOW) => ({ type, sessionId: 'x', turnId: 'turn-1', sequence, createdAt, entryId: `l-${sequence}`, parentEntryId: sequence === 1 ? null : `l-${sequence - 1}`, ...extra });
  const listFiles = {
    'a.jsonl': [at(1, 'accepted_input', { messages: [user('first')] }), at(2, 'accepted_input', { messages: [user(' second ')] })].map(line).join(''),
    'b.jsonl': [at(1, 'session_metadata', { metadata: { title: '  ', aiTitle: 'AI', tag: 't', parentSessionId: 'p', forkedFromTurnId: 'f' } }), at(2, 'accepted_input', { messages: [user('b prompt')] })].map(line).join(''),
    'meta.jsonl': [at(1, 'session_metadata', { metadata: { title: 'Custom' } }), at(2, 'session_metadata', { metadata: { title: null, aiTitle: null, tag: null } })].map(line).join(''),
    'bad.jsonl': line(at(1, 'accepted_input', { messages: [user('bad')] })) + '{bad\n',
    'nodate.jsonl': line(at(1, 'accepted_input', { messages: [user('no date')] }, 'not-a-date')),
    'blank.jsonl': line(at(1, 'accepted_input', { messages: [{ role: 'user', content: [{ type: 'text', text: ' \n ' }, { type: 'image', source: 'x' }] }] })),
  };
  const records = [
    { sessionId: 'b', lastModified: 200, fileSize: 10 }, { sessionId: 'a', lastModified: 100 },
    { sessionId: 'missing', lastModified: 300 }, { sessionId: 'bad', lastModified: 400 },
    { sessionId: 'meta', lastModified: 150 }, { sessionId: 'nodate', lastModified: 150 }, { sessionId: 'blank', lastModified: 500 },
  ];
  {
    const r = await scenario('list', {
      store: { list: records }, files: listFiles,
      steps: [
        { op: 'list' }, { op: 'list', limit: 1 }, { op: 'list', offset: 1, limit: 2 }, { op: 'list', limit: 0 },
        { op: 'list', offset: -3, limit: -1 }, { op: 'list', offset: 1.7 }, { op: 'list', limit: 2.5 }, { op: 'list', offset: 5 },
      ],
    }, ['list:b,meta,nodate,a', 'list:b', 'list:meta,nodate', 'list:b,meta,nodate,a', 'list:b,meta,nodate', 'list:meta,nodate,a', 'list:b,meta', 'list:']);
    const [b, meta, nodate, a] = r.steps[0].outcome.sessions;
    assert.equal(b.summary, 'AI');
    assert.equal(meta.customTitle, 'Custom');
    assert.equal(meta.aiTitle, null);
    assert.equal(nodate.createdAt, null);
    assert.deepEqual([a.summary, a.firstPrompt], ['second', 'first']);
  }

  await scenario('list-read-failure', {
    store: { list: [{ sessionId: 'a', lastModified: 1 }, { sessionId: 'b', lastModified: 2 }], failRead: [2] },
    files: { 'a.jsonl': listFiles['a.jsonl'], 'b.jsonl': listFiles['b.jsonl'] },
    steps: [{ op: 'list' }, { op: 'list' }],
  }, ['error:Error', 'list:b,a']);

  // Declared gaps: the Rust adapter must refuse (NodeHostRequired) at the first
  // step whose TS outcome depends on a module this PR does not port.
  await scenario('node-only-turn-started', {
    steps: [open(), { op: 'event', turnId: 'turn-1', event: { type: 'turn_started' } }, { op: 'read' }],
  }, ['open:0', 'ok', 'read:1/0']);
  await scenario('node-only-malformed-restore', {
    steps: [open(), { op: 'accepted', turnId: 'turn-1', messages: 'oops' }, open(), { op: 'restore' }],
  }, ['open:0', 'ok', 'open:0', 'restored:1/0@1']);
  await scenario('node-only-list-shape', {
    store: { list: [{ sessionId: 'x', lastModified: 1 }] },
    files: { 'x.jsonl': line(at(1, 'accepted_input', { messages: [user('x')] })) + line(at(2, 'turn_result', { result: null })) },
    steps: [{ op: 'list' }],
  }, ['error:TypeError']);

  const sourceSha256 = Object.fromEntries(await Promise.all(SOURCES.map(async (path) => [path, createHash('sha256').update(await readFile(join(baseline, path))).digest('hex')])));
  const oracle = JSON.stringify({ sourceCommit, sourceSha256, node: process.version, now: NOW, projectRoot: PROJECT_ROOT, scenarios }, null, 2) + '\n';
  for (const dir of [scratch, output, baseline]) assert.ok(!oracle.includes(dir), `oracle.json contains ${dir}`);
  assert.ok(!new RegExp(UUID.source).test(oracle), 'oracle.json contains an unnormalized UUID');
  await writeFile(join(output, 'oracle.json'), oracle);
  console.log(`PASS: generated ${scenarios.length} real TS AsyncTranscriptStorageAdapter scenarios`);
} finally { await rm(scratch, { recursive: true, force: true }); }
