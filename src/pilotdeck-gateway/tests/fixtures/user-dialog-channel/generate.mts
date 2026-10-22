// Real TypeScript oracle for the Rust Gateway user-dialog journal and channel.
// Executes the fixed implementation; never copies its persistence code.
import assert from 'node:assert/strict';
import { existsSync, statSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, rm, unlink, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE is required');
assert.ok(process.argv[2], 'output directory is required');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const files = [
  'src/gateway/user-dialog/GatewayUserDialogJournal.ts',
  'src/gateway/user-dialog/GatewayUserDialogChannel.ts',
  'src/gateway/user-dialog/GatewayUserDialogBus.ts',
  'src/tool/dialog/FormDialogSchema.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, files);
const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { GatewayUserDialogJournal, createGatewayUserDialogJournal } = await load('src/gateway/user-dialog/GatewayUserDialogJournal.ts');
const { GatewayUserDialogBus } = await load('src/gateway/user-dialog/GatewayUserDialogBus.ts');
const { GatewayUserDialogChannel } = await load('src/gateway/user-dialog/GatewayUserDialogChannel.ts');

const initialMs = Date.parse('2026-10-18T00:00:00.000Z');
const pollIntervalMs = 100;
const clock = { ms: initialMs };
const RealDate = Date;
class ControlledDate extends RealDate {
  constructor(...args: any[]) { super(...(args.length ? args : [clock.ms])); }
  static now() { return clock.ms; }
}
(globalThis as any).Date = ControlledDate;

type Timer = { id: number; due: number; every: number; fn: () => void };
let timers = new Map<number, Timer>();
let timerSequence = 1;
(globalThis as any).setInterval = (fn: () => void, every: number) => {
  const id = timerSequence++;
  timers.set(id, { id, due: clock.ms + every, every, fn });
  return id;
};
(globalThis as any).clearInterval = (id: number) => { timers.delete(id); };

async function flush(): Promise<void> {
  for (let index = 0; index < 8; index++) await Promise.resolve();
  await new Promise((done) => setImmediate(done));
  for (let index = 0; index < 4; index++) await Promise.resolve();
}
async function advance(ms: number): Promise<void> {
  clock.ms += ms;
  const due = [...timers.values()].filter((timer) => timer.due <= clock.ms)
    .sort((left, right) => left.due - right.due || left.id - right.id);
  for (const timer of due) {
    if (!timers.has(timer.id)) continue;
    timer.due = clock.ms + timer.every;
    timer.fn();
    await flush();
  }
}
const clone = (value: any): any => value === undefined ? null : JSON.parse(JSON.stringify(value));
function errorOf(error: any): any {
  const message = error instanceof Error ? error.message : String(error);
  if (error && typeof error === 'object' && typeof error.code === 'string') return '<io>';
  if (/^Unable to read Gateway user-dialog journal /.test(message)) return 'journal-read';
  return message;
}
async function readTextOrNull(path: string): Promise<string | null> {
  try { return await readFile(path, 'utf8'); } catch { return null; }
}
async function writeRaw(path: string, text: string): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, text);
}

type Config = {
  journal: boolean;
  store: boolean;
  owner: boolean;
  take: boolean;
  ownerTtlMs: number;
  journalRecordFail: boolean;
  journalRemoveFail: boolean;
  storePutFail: boolean;
  storeRemoveFail: boolean;
  emitFail: boolean;
  busObserverFail: boolean;
  claim: 'ok' | 'false' | 'throw';
  renew: 'ok' | 'false' | 'throw';
  release: 'ok' | 'throw';
  takeMode: 'ok' | 'throw';
  abortOnPut: boolean;
};

function makeConfig(input: any = {}): Config {
  return {
    journal: true, store: true, owner: true, take: true, ownerTtlMs: 5000,
    journalRecordFail: false, journalRemoveFail: false,
    storePutFail: false, storeRemoveFail: false,
    emitFail: false, busObserverFail: false,
    claim: 'ok', renew: 'ok', release: 'ok', takeMode: 'ok', abortOnPut: false,
    ...input,
  };
}

async function runChannelCase(name: string, spec: any): Promise<any> {
  clock.ms = initialMs;
  timers = new Map();
  timerSequence = 1;
  const config = makeConfig(spec.config);
  const initialConfig = clone(config);
  const directory = await mkdtemp(join(output, '.channel-'));
  const journalPath = join(directory, 'sessions', 'transcript.jsonl.dialogs.json');
  await mkdir(resolve(journalPath, '..'), { recursive: true });
  const logs: any = { journal: [], store: [], owner: [], take: [], emit: [], bus: [], uuid: [] };
  const storeMap = new Map<string, any>();
  const ownerMap = new Map<string, any>();
  const answers = new Map<string, any>();
  let controller: AbortController | undefined;
  const slots: any = {};

  const actualJournal = new GatewayUserDialogJournal(journalPath, { now: () => new Date(clock.ms) });
  const journal = {
    record(event: any) {
      logs.journal.push(['record', clone(event)]);
      if (config.journalRecordFail) throw new Error('journal record failed');
      actualJournal.record(event);
    },
    remove(requestId: string) {
      logs.journal.push(['remove', requestId]);
      if (config.journalRemoveFail) throw new Error('journal remove failed');
      actualJournal.remove(requestId);
    },
    recover() { return actualJournal.recover(); },
    clear() { actualJournal.clear(); },
  };
  const storeKey = { projectRoot: '/fixture/project', pilotHome: '/fixture/home', sessionId: 'session' };
  const store = {
    put(key: any, dialog: any) {
      assert.deepEqual(key, storeKey);
      logs.store.push(['put', clone(dialog)]);
      if (config.abortOnPut) controller?.abort();
      if (config.storePutFail) throw new Error('store put failed');
      storeMap.set(dialog.request.requestId, clone(dialog));
    },
    remove(key: any, requestId: string) {
      assert.deepEqual(key, storeKey);
      logs.store.push(['remove', requestId]);
      if (config.storeRemoveFail) throw new Error('store remove failed');
      storeMap.delete(requestId);
    },
  };
  const takeStoredAnswer = (requestId: string) => {
    logs.take.push([requestId]);
    if (config.takeMode === 'throw') throw new Error('take failed');
    const answer = answers.get(requestId);
    if (!answer) return undefined;
    answers.delete(requestId);
    return clone(answer);
  };
  const claimStoredOwner = (requestId: string, ownerId: string) => {
    logs.owner.push(['claim', requestId, ownerId]);
    if (config.claim === 'throw') throw new Error('claim exploded');
    if (config.claim === 'false') return false;
    const current = ownerMap.get(requestId);
    if (current && current.expiresAt > clock.ms && current.ownerId !== ownerId) return false;
    ownerMap.set(requestId, { ownerId, expiresAt: clock.ms + config.ownerTtlMs });
    return true;
  };
  const renewStoredOwner = (requestId: string, ownerId: string) => {
    logs.owner.push(['renew', requestId, ownerId]);
    if (config.renew === 'throw') throw new Error('renew exploded');
    if (config.renew === 'false') return false;
    const current = ownerMap.get(requestId);
    if (!current || current.ownerId !== ownerId || current.expiresAt <= clock.ms) return false;
    current.expiresAt = clock.ms + config.ownerTtlMs;
    return true;
  };
  const releaseStoredOwner = (requestId: string, ownerId: string) => {
    logs.owner.push(['release', requestId, ownerId]);
    if (config.release === 'throw') throw new Error('release exploded');
    const current = ownerMap.get(requestId);
    if (!current || current.ownerId !== ownerId) return false;
    ownerMap.delete(requestId);
    return true;
  };
  const emit = (event: any) => {
    logs.emit.push(clone(event));
    if (config.emitFail) throw new Error('emit failed');
  };
  const bus = new GatewayUserDialogBus({
    now: () => new Date(clock.ms),
    uuid: () => 'bus-lease',
    onChange: (event: any) => {
      logs.bus.push(clone(event));
      if (config.busObserverFail) throw new Error('observer offline');
    },
  });
  const channel = new GatewayUserDialogChannel({
    sessionKey: 'session',
    bus,
    emit,
    ...(config.journal ? { journal } : {}),
    ...(config.store ? { store, storeKey } : {}),
    ...(config.take ? { takeStoredAnswer } : {}),
    ...(config.owner ? { claimStoredOwner, renewStoredOwner, releaseStoredOwner } : {}),
    uuid: () => {
      const id = `uuid-${logs.uuid.length + 1}`;
      logs.uuid.push(id);
      return id;
    },
  });

  const idFor = (action: any) => slots[action.slot ?? 'default']?.requestId;
  const begin = (action: any) => {
    if (action.signal) controller = new AbortController();
    const request = buildRequest(action.kind, action.request, action.signal ? controller!.signal : undefined);
    const before = logs.uuid.length;
    const promise = (channel as any)[`request${action.kind[0].toUpperCase()}${action.kind.slice(1)}`](request);
    assert.ok(logs.uuid.length >= before + 2);
    const slot = action.slot ?? 'default';
    slots[slot] = { requestId: logs.uuid[before], promise, outcome: null };
    promise.then(
      (value: any) => { slots[slot].outcome = { ok: true, value: clone(value) }; },
      (error: any) => { slots[slot].outcome = { ok: false, error: errorOf(error) }; },
    );
  };
  const run = async (action: any) => {
    let result: any = null;
    try {
      switch (action.op) {
        case 'begin': begin(action); break;
        case 'abort': controller!.abort(); break;
        case 'tick': await advance(action.ms); result = clock.ms; break;
        case 'set': Object.assign(config, action.config); break;
        case 'claim': result = bus.claim('session', idFor(action), action.ttl ?? 1000, action.lease); break;
        case 'releaseBus': result = bus.release('session', idFor(action), action.lease); break;
        case 'check': {
          const entry = bus.peek('session', idFor(action));
          result = { accepted: entry ? entry.accepts(action.value) : null, pending: bus.pendingCount('session') };
          break;
        }
        case 'respond': {
          const consumed = bus.consumeForResponse('session', idFor(action), action.lease, action.cancel ? 'cancelled' : 'answered');
          if (!consumed.entry) result = { consumed: false, reason: consumed.reason };
          else if (action.cancel) {
            consumed.entry.resolve({ type: 'cancelled', ...(action.reason !== undefined ? { reason: action.reason } : {}) });
            result = { consumed: true, cancelled: true };
          } else {
            consumed.entry.resolve({ type: 'answered', value: action.value });
            result = { consumed: true, answered: true };
          }
          break;
        }
        case 'submitAnswer': {
          const requestId = idFor(action);
          answers.set(requestId, {
            requestId,
            result: action.result,
            submittedAt: new Date().toISOString(),
          });
          break;
        }
        case 'recover': result = actualJournal.recover(); break;
        case 'corruptJournal': {
          await unlink(journalPath);
          await mkdir(journalPath);
          break;
        }
        case 'rejectSession': bus.rejectSession('session', action.reason ?? 'closed'); break;
        case 'snapshot': break;
        default: throw new Error(`unhandled ${action.op}`);
      }
    } catch (error) { result = { error: errorOf(error) }; }
    await flush();
    return {
      action,
      result: clone(result),
      observation: {
        outcomes: Object.fromEntries(Object.entries(slots).map(([slot, value]: any) => [slot, value.outcome])),
        pending: bus.list('session'),
        pendingCount: bus.pendingCount('session'),
        journalBytes: await readTextOrNull(journalPath),
        journalExists: existsSync(journalPath),
        store: [...storeMap.entries()].map(([id, value]) => [id, value]),
        owners: [...ownerMap.entries()].map(([id, value]) => ({ id, ...value, active: value.expiresAt > clock.ms })),
        answers: [...answers.entries()].map(([id, value]) => [id, value]),
        logs: clone(logs),
      },
    };
  };
  const steps = [];
  for (const action of spec.actions) steps.push(await run(action));
  await rm(directory, { recursive: true, force: true });
  return { name, config: initialConfig, steps };
}

function buildRequest(kind: string, request: any, signal?: AbortSignal) {
  const base: any = {
    toolCallId: request.toolCallId ?? 'tool-1',
    toolName: request.toolName ?? `request_${kind}`,
    prompt: request.prompt ?? '继续？\ud800',
    ...(signal ? { signal } : {}),
  };
  switch (kind) {
    case 'input':
      return { ...base, ...(request.placeholder !== undefined ? { placeholder: request.placeholder } : {}),
        ...(request.allowEmpty !== undefined ? { allowEmpty: request.allowEmpty } : {}) };
    case 'select':
      return { ...base, choices: request.choices, ...(request.defaultValue !== undefined ? { defaultValue: request.defaultValue } : {}) };
    case 'confirm':
      return { ...base, ...(request.confirmLabel !== undefined ? { confirmLabel: request.confirmLabel } : {}),
        ...(request.cancelLabel !== undefined ? { cancelLabel: request.cancelLabel } : {}),
        ...(request.defaultValue !== undefined ? { defaultValue: request.defaultValue } : {}) };
    case 'form':
      return { ...base, schema: request.schema };
    default: throw new Error(`unknown kind ${kind}`);
  }
}

async function runJournalCase(name: string, spec: any): Promise<any> {
  clock.ms = initialMs;
  const directory = await mkdtemp(join(output, '.journal-'));
  const journalPath = join(directory, 'transcript.jsonl.dialogs.json');
  if (spec.initial !== undefined) {
    if (spec.initial.directory) await mkdir(journalPath, { recursive: true });
    else await writeRaw(journalPath, spec.initial.text);
  }
  const journal = new GatewayUserDialogJournal(journalPath, { now: () => new Date(clock.ms) });
  const steps = [];
  for (const action of spec.actions) {
    let result: any = null;
    try {
      switch (action.op) {
        case 'record': journal.record(action.event); break;
        case 'remove': journal.remove(action.id); break;
        case 'recover': result = journal.recover(); break;
        case 'clear': journal.clear(); break;
        case 'snapshot': break;
        case 'corruptJournal': {
          if (existsSync(journalPath)) await unlink(journalPath);
          await mkdir(journalPath);
          break;
        }
        default: throw new Error(`unhandled ${action.op}`);
      }
    } catch (error) { result = { error: errorOf(error) }; }
    let isDirectory = false;
    try { isDirectory = statSync(journalPath).isDirectory(); } catch {}
    steps.push({
      action,
      result: clone(result),
      observation: {
        bytes: await readTextOrNull(journalPath),
        exists: existsSync(journalPath),
        directory: isDirectory,
      },
    });
  }
  await rm(directory, { recursive: true, force: true });
  return { name, initial: spec.initial ?? null, steps };
}

const events = {
  input: {
    type: 'user_dialog_request', requestId: 'dialog-1', dialogKind: 'input',
    toolCallId: 'tool-1', toolName: 'request_user_input', prompt: '继续？\ud800',
    placeholder: '说点什么', allowEmpty: true,
    extra: { z: 1e-7, a: -0, '10': 'ten', '2': 'two' },
  },
  select: {
    type: 'user_dialog_request', requestId: 'dialog-2', dialogKind: 'select',
    toolCallId: 'tool-2', toolName: 'request_user_select', prompt: '选一个',
    choices: [{ value: 'alpha' }, { value: 'beta', label: 'B' }, { value: '中文', description: 'd' }],
  },
  form: {
    type: 'user_dialog_request', requestId: 'dialog-3', dialogKind: 'form',
    toolCallId: 'tool-3', toolName: 'request_user_form', prompt: '填表',
    schema: { type: 'object', properties: { name: { type: 'string' } }, required: ['name'], additionalProperties: false },
  },
};

await mkdir(output, { recursive: true });

const channelCases = [
  await runChannelCase('input-answered', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: { placeholder: '说点什么', allowEmpty: true } },
      { op: 'snapshot' },
      { op: 'claim', ttl: 1000 },
      { op: 'snapshot' },
      { op: 'check', value: 42 },
      { op: 'check', value: '' },
      { op: 'check', value: 'hello' },
      { op: 'respond', lease: 'bus-lease', value: '你好\ud800' },
      { op: 'snapshot' },
      { op: 'tick', ms: 500 },
      { op: 'recover' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('input-minimal', {
    config: { store: false, owner: false, take: false },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
      { op: 'respond', value: 'ok' },
      { op: 'snapshot' },
      { op: 'recover' },
    ],
  }),
  await runChannelCase('select-answered', {
    config: {},
    actions: [
      { op: 'begin', kind: 'select', request: { choices: [{ value: 'alpha' }, { value: 'beta', label: 'B' }, { value: '中文', description: 'd' }], defaultValue: 'alpha' } },
      { op: 'snapshot' },
      { op: 'claim', ttl: 1000 },
      { op: 'check', value: 'gamma' },
      { op: 'check', value: 'beta' },
      { op: 'respond', lease: 'bus-lease', value: 'beta' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('confirm-answered', {
    config: {},
    actions: [
      { op: 'begin', kind: 'confirm', request: { confirmLabel: '好', cancelLabel: '', defaultValue: false } },
      { op: 'snapshot' },
      { op: 'check', value: 'yes' },
      { op: 'check', value: true },
      { op: 'claim', ttl: 1000 },
      { op: 'respond', lease: 'bus-lease', value: true },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('form-answered', {
    config: {},
    actions: [
      { op: 'begin', kind: 'form', request: { schema: { type: 'object', properties: { name: { type: 'string' } }, required: ['name'], additionalProperties: false } } },
      { op: 'snapshot' },
      { op: 'check', value: {} },
      { op: 'check', value: { name: '中文\ud800' } },
      { op: 'claim', ttl: 1000 },
      { op: 'respond', lease: 'bus-lease', value: { name: '中文\ud800' } },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('abort-after-register', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: { prompt: 'cancel me' }, signal: true },
      { op: 'snapshot' },
      { op: 'claim', ttl: 1000 },
      { op: 'abort' },
      { op: 'snapshot' },
      { op: 'tick', ms: 500 },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('abort-during-store-put', {
    config: { abortOnPut: true },
    actions: [
      { op: 'begin', kind: 'input', request: { prompt: 'abort during persist' }, signal: true },
      { op: 'snapshot' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('claim-false', {
    config: { claim: 'false' },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('claim-throws', {
    config: { claim: 'throw' },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('journal-record-fails', {
    config: { journalRecordFail: true },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('store-put-fails', {
    config: { storePutFail: true },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
      { op: 'recover' },
    ],
  }),
  await runChannelCase('emit-throws', {
    config: { emitFail: true },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
      { op: 'claim', ttl: 1000 },
      { op: 'respond', lease: 'bus-lease', value: 'late' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('journal-remove-fails', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'set', config: { journalRemoveFail: true } },
      { op: 'respond', value: 'answered' },
      { op: 'snapshot' },
      { op: 'recover' },
    ],
  }),
  await runChannelCase('store-remove-fails', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'set', config: { storeRemoveFail: true } },
      { op: 'respond', value: 'answered' },
      { op: 'snapshot' },
      { op: 'recover' },
    ],
  }),
  await runChannelCase('release-throws', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'set', config: { release: 'throw' } },
      { op: 'respond', value: 'answered' },
      { op: 'snapshot' },
      { op: 'recover' },
    ],
  }),
  await runChannelCase('two-renderers', {
    config: { take: false },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'claim', ttl: 1000 },
      { op: 'claim', ttl: 5000 },
      { op: 'respond', lease: 'other', value: 'stolen' },
      { op: 'snapshot' },
      { op: 'releaseBus', lease: 'wrong' },
      { op: 'releaseBus', lease: 'bus-lease' },
      { op: 'respond', value: 'after-release' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('cross-gateway-answered', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'submitAnswer', result: { behavior: 'answered', value: '跨进程' } },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
      { op: 'tick', ms: 100 },
    ],
  }),
  await runChannelCase('cross-gateway-invalid', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'submitAnswer', result: { behavior: 'answered', value: 42 } },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('cross-gateway-cancelled', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'submitAnswer', result: { behavior: 'cancelled', reason: 'timeout' } },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('renew-failure-then-answer', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'set', config: { renew: 'false' } },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
      { op: 'set', config: { renew: 'ok' } },
      { op: 'submitAnswer', result: { behavior: 'answered', value: 'recovered' } },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('take-throws-then-answer', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'set', config: { takeMode: 'throw' } },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
      { op: 'set', config: { takeMode: 'ok' } },
      { op: 'submitAnswer', result: { behavior: 'answered', value: 'after-error' } },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('owner-lease-timeout', {
    config: { ownerTtlMs: 250 },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
      { op: 'set', config: { renew: 'false' } },
      { op: 'tick', ms: 100 },
      { op: 'tick', ms: 100 },
      { op: 'tick', ms: 100 },
      { op: 'snapshot' },
      { op: 'recover' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('crash-recover', {
    config: {},
    actions: [
      { op: 'begin', kind: 'select', request: { choices: [{ value: 'a' }, { value: 'b', label: 'B' }], defaultValue: 'b' } },
      { op: 'snapshot' },
      { op: 'recover' },
      { op: 'recover' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('reject-session', {
    config: {},
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'rejectSession', reason: 'turn_closed' },
      { op: 'snapshot' },
    ],
  }),
  await runChannelCase('bus-observer-throws', {
    config: { busObserverFail: true },
    actions: [
      { op: 'begin', kind: 'input', request: {} },
      { op: 'snapshot' },
    ],
  }),
];

const journalSpecs = [
  {
    name: 'roundtrip',
    actions: [
      { op: 'record', event: events.input },
      { op: 'record', event: events.select },
      { op: 'snapshot' },
      { op: 'record', event: { ...events.input, prompt: 'replacement' } },
      { op: 'snapshot' },
      { op: 'remove', id: 'absent' },
      { op: 'recover' },
      { op: 'remove', id: 'dialog-1' },
      { op: 'recover' },
      { op: 'remove', id: 'dialog-2' },
      { op: 'recover' },
      { op: 'clear' },
    ],
  },
  {
    name: 'recover-fields',
    actions: [
      { op: 'record', event: { ...events.form } },
      { op: 'recover' },
      { op: 'snapshot' },
    ],
  },
  {
    name: 'clone-on-record',
    actions: [
      { op: 'record', event: events.input },
      { op: 'record', event: { ...events.input, extra: { changed: true } } },
      { op: 'snapshot' },
    ],
  },
  ...['{"version":2,"dialogs":[]}', '{"version":1,"dialogs":{}}',
    '{"version":1,"dialogs":[{"type":"user_dialog_request"}]}', '{broken']
    .map((text, index) => ({
      name: `invalid-${index}`,
      initial: { text },
      actions: [{ op: 'recover' }, { op: 'record', event: events.input }],
    })),
  { name: 'missing', actions: [{ op: 'recover' }, { op: 'snapshot' }] },
];

const journalCaseResults = [];
for (const spec of journalSpecs) journalCaseResults.push(await runJournalCase(spec.name, spec));

const creationCases = [
  ['undefined', {}],
  ['empty', { transcriptPath: '' }],
  ['relative', { transcriptPath: 'sessions/transcript.jsonl' }],
  ['absolute', { transcriptPath: '/fixture/sessions/transcript.jsonl' }],
  ['trailing-dot', { transcriptPath: '/fixture/sessions/name.' }],
].map(([name, storage]) => {
  const journal = createGatewayUserDialogJournal(storage as any);
  return { name, transcriptPath: (storage as any).transcriptPath, path: journal ? journal.path : null };
});

const command = 'PDRS_BASELINE=/absolute/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/user-dialog-channel/generate.mts src/pilotdeck-gateway/tests/fixtures/user-dialog-channel';
await writeFile(join(output, 'oracle.json'), JSON.stringify({
  sourceCommit,
  sourceSha256,
  command,
  initialMs,
  pollIntervalMs,
  sessionKey: 'session',
  channelCases,
  journalCases: journalCaseResults,
  creationCases,
}, null, 2) + '\n');
console.log(`PASS: ${channelCases.length} channel cases, ${journalCaseResults.length} journal cases, ${creationCases.length} creation cases`);
