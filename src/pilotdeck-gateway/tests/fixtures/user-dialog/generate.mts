// Executes the fixed implementation; never copies its persistence code.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE is required');
assert.ok(process.argv[2], 'output directory is required');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { FileGatewayUserDialogStore: Store } = await load('src/gateway/user-dialog/FileGatewayUserDialogStore.ts');
const { GatewayUserDialogBus: Bus } = await load('src/gateway/user-dialog/GatewayUserDialogBus.ts');
const { InProcessGateway: Gateway } = await load('src/gateway/client/InProcessGateway.ts');
const key = { projectRoot: '/fixture/项目/' + 'long'.repeat(35), pilotHome: '/fixture/home', sessionId: 'session/../中文\ud800' };
const request = { type: 'user_dialog_request', requestId: 'dialog-1', dialogKind: 'input', toolCallId: 'tool-1', toolName: 'request_user_input', prompt: '继续？\ud800', extra: { z: 1e-7, a: -0, '10': 'ten', '2': 'two' } };
const createdAt = '2026-10-18T00:00:00.000Z';
const initialMs = Date.parse(createdAt);
const makeStore = (directory: string, clock: { ms: number }) => new Store({ directory, now: () => new Date(clock.ms), uuid: () => 'fixed-id' });
// Accessing the actual private path builder verifies the storage identity too.
const pathFor = (store: any) => store.recordPath(key);
if (process.argv[3] === '--verify-rust') {
  const store = makeStore(output, { ms: initialMs });
  const listed = await store.list(key);
  // TS 自身写盘用 JSON.stringify（FileGatewayUserDialogStore.ts:362），-0 落盘即 0；期望取 TS 自己往返后的值，仍全字段严格比对
  const roundTripped = JSON.parse(JSON.stringify(request));
  assert.deepEqual(listed, [{ request: roundTripped, createdAt }]);
  assert.deepEqual(await store.listLive(key), listed);
  assert.equal((await store.claimLive(key, { requestId: request.requestId, ttlMs: 1000 })).reason, 'claimed');
  const answer = await store.takeLiveAnswer(key, request.requestId);
  assert.deepEqual(answer, { requestId: request.requestId, result: { behavior: 'answered', value: { z: '中文\ud800', a: 1e-7 } }, submittedAt: createdAt });
  assert.equal(await store.takeLiveAnswer(key, request.requestId), undefined);
  await store.put(key, { request: { ...request, requestId: 'ts-after-rust' }, createdAt });
  console.log('PASS: real TS read Rust request/owner/renderer lease/answer, consumed answer once, and continued writing');
  process.exit(0);
}
await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
try {
  const clock = { ms: initialMs };
  let store = makeStore(scratch, clock);
  const relativePath = relative(scratch, pathFor(store));
  const steps: any[] = [];
  async function step(action: any) {
    let result: any;
    try {
      const id = action.id ?? request.requestId;
      switch (action.op) {
        case 'put': result = await store.put(key, { request: action.request ?? request, createdAt: action.createdAt ?? createdAt }); break;
        case 'list': result = await store.list(key); break;
        case 'listLive': result = await store.listLive(key); break;
        case 'claim': result = await store.claimLive(key, { requestId: id, ttlMs: action.ttl, leaseId: action.lease }); break;
        case 'release': result = await store.releaseLive(key, { requestId: id, leaseId: action.lease }); break;
        case 'owner': result = await store.claimLiveOwner(key, { requestId: id, ownerId: action.owner, ttlMs: action.ttl }); break;
        case 'renewOwner': result = await store.renewLiveOwner(key, { requestId: id, ownerId: action.owner, ttlMs: action.ttl }); break;
        case 'releaseOwner': result = await store.releaseLiveOwner(key, { requestId: id, ownerId: action.owner }); break;
        case 'submit': result = await store.submitLiveAnswer(key, { requestId: id, leaseId: action.lease, result: action.result }); break;
        case 'take': result = await store.takeLiveAnswer(key, id); break;
        case 'remove': result = await store.remove(key, id); break;
        case 'clear': result = await store.clear(key); break;
        case 'advance': clock.ms += action.ms; result = clock.ms; break;
        case 'reopen': store = makeStore(scratch, clock); break;
        default: throw new Error(`unhandled ${action.op}`);
      }
    } catch (error) { result = { error: (error as Error).message }; }
    let disk = null;
    try { disk = await readFile(pathFor(store), 'utf8'); } catch (error) { if ((error as any).code !== 'ENOENT') throw error; }
    steps.push({ action, result: result ?? null, disk });
  }
  for (const action of [
    { op: 'list' }, { op: 'claim', id: 'missing', ttl: -1 },
    { op: 'put' }, { op: 'listLive' },
    { op: 'owner', owner: 'owner-A', ttl: 1000 },
    { op: 'owner', owner: 'owner-B', ttl: -1 },
    { op: 'claim', ttl: 3000 },
    { op: 'claim', ttl: -1 },
    { op: 'submit', lease: 'wrong', result: { behavior: 'answered', value: 'rejected lease' } },
    { op: 'submit', lease: 'fixed-id', result: { behavior: 'answered', value: { z: '中文\ud800', a: 1e-7 } } },
  ]) await step(action);
  await writeFile(join(output, 'hosted.json'), await readFile(pathFor(store)));
  for (const action of [
    { op: 'put', request: { ...request, prompt: 'replacement' }, createdAt: 'not-a-date' },
    { op: 'reopen' }, { op: 'list' }, { op: 'listLive' },
    { op: 'take' }, { op: 'take' },
    { op: 'release', lease: 'wrong' }, { op: 'releaseOwner', owner: 'wrong' },
    { op: 'renewOwner', owner: 'owner-A', ttl: 1000 },
    { op: 'advance', ms: 1000 }, { op: 'listLive' },
    { op: 'claim', ttl: 1000 },
    { op: 'renewOwner', owner: 'owner-A', ttl: 1000 },
    { op: 'release', lease: 'fixed-id' },
    { op: 'claim', ttl: 0 },
    { op: 'claim', ttl: 2000 }, { op: 'claim', lease: 'fixed-id', ttl: 4000 },
    { op: 'owner', owner: 'owner-B', ttl: 10000 },
    { op: 'advance', ms: 4000 }, { op: 'release', lease: 'fixed-id' },
    { op: 'listLive' },
    { op: 'submit', result: { behavior: 'cancelled', reason: 'first' } },
    { op: 'submit', result: { behavior: 'cancelled', reason: '' } },
    { op: 'take' }, { op: 'releaseOwner', owner: 'owner-B' },
    { op: 'submit', result: { behavior: 'answered', value: 42 } },
    { op: 'take' }, // Store deliberately does not validate an input answer.
    { op: 'put', request: { ...request, requestId: 'dialog-2' } },
    { op: 'remove', id: 'absent' }, { op: 'remove' }, { op: 'remove', id: 'dialog-2' },
    { op: 'release', lease: 'absent' }, { op: 'renewOwner', owner: 'owner-A', ttl: 0 },
    { op: 'clear' },
  ]) await step(action);

  // Real bus including notification throws, cleanup errors, and renderer claims.
  let now = initialMs;
  const events: any[] = [];
  let settleFails = false, settled = 0, rejected: string[] = [], resolved: any[] = [];
  const bus = new Bus({ now: () => new Date(now), uuid: () => 'bus-lease', onChange: (event: any) => { events.push(event); throw new Error('observer offline'); } });
  const pending = () => ({ requestId: request.requestId, dialogKind: request.dialogKind, toolCallId: request.toolCallId, toolName: request.toolName, event: request, accepts: (value: unknown) => typeof value === 'string', resolve: (answer: any) => resolved.push(answer), reject: (error: Error) => rejected.push(error.message), onSettled: () => { if (settleFails) throw new Error('settle failed'); settled++; } });
  const busSteps: any[] = [];
  for (const action of [
    { op: 'register' }, { op: 'claim', ttl: 1000 }, { op: 'list' }, { op: 'claim', ttl: 1000 },
    { op: 'consume', lease: 'wrong' }, { op: 'release', lease: 'wrong' },
    { op: 'claim', ttl: 2000, lease: 'bus-lease' }, { op: 'advance', ms: 2000 },
    { op: 'list' }, { op: 'release', lease: 'bus-lease' },
    { op: 'claim', ttl: 1000 }, { op: 'release', lease: 'bus-lease' },
    { op: 'claim', ttl: 1000 }, { op: 'settleFails', value: true },
    { op: 'consume', lease: 'bus-lease' }, { op: 'list' },
    { op: 'settleFails', value: false }, { op: 'consume', lease: 'bus-lease' },
    { op: 'consume' }, { op: 'register' }, { op: 'claim', ttl: 1000 },
    { op: 'settleFails', value: true }, { op: 'reject' }, { op: 'list' },
  ]) {
    let result: any;
    try {
      switch (action.op) {
        case 'register': bus.register('session', pending()); break;
        case 'claim': result = bus.claim('session', request.requestId, action.ttl, action.lease); break;
        case 'list': result = bus.list('session'); break;
        case 'consume': { const value = bus.consumeForResponse('session', request.requestId, action.lease); result = value.entry ? { entry: value.entry.event } : { reason: value.reason }; break; }
        case 'release': result = bus.release('session', request.requestId, action.lease); break;
        case 'advance': now += action.ms; result = now; break;
        case 'settleFails': settleFails = action.value; break;
        case 'reject': bus.rejectSession('session', 'closed'); break;
      }
    } catch (error) { result = { error: (error as Error).message }; }
    busSteps.push({ action, result: result ?? null, events: structuredClone(events), count: bus.pendingCount('session'), settled, rejected: [...rejected] });
  }

  // Invoke the real caller methods without constructing a provider/runtime host.
  // These fixtures describe the contract seam; no WS/session runtime is started.
  const gateway: any = Object.create(Gateway.prototype);
  gateway.userDialogBus = bus;
  gateway.options = {};
  settleFails = false;
  bus.register('session', pending());
  const responses = [];
  for (const value of [42, '']) {
    try { responses.push({ value, result: await gateway.respondUserDialog({ sessionKey: 'session', requestId: request.requestId, result: { behavior: 'answered', value } }), pending: bus.pendingCount('session') }); }
    catch (error) { responses.push({ value, error: { code: (error as any).code, message: (error as Error).message }, pending: bus.pendingCount('session') }); }
  }
  const recovered = { type: 'user_dialog_terminated', request, reason: 'gateway_restarted', terminatedAt: createdAt, recovery: 'next_turn_context' };
  gateway.options = { listHostedUserDialogs: () => [request], listRecoveredUserDialogs: () => [recovered] };
  bus.register('session', pending());
  const hostedProjection = await gateway.listUserDialogs({ sessionKey: 'session' });
  gateway.options.listHostedUserDialogs = () => [];
  const liveProjection = await gateway.listUserDialogs({ sessionKey: 'session' });
  bus.consume('session', request.requestId);
  const recoveredProjection = await gateway.listUserDialogs({ sessionKey: 'session' });

  // TS reader admission and normalization, based on its real generated record.
  const golden = JSON.parse(await readFile(join(output, 'hosted.json'), 'utf8'));
  const readCases: any[] = [];
  for (const [name, mutate] of Object.entries({
    'extras': (v: any) => { v.extra = true; v.dialogs[0].extra = true; v.dialogs[0].lease.extra = true; },
    'minimal-request': (v: any) => { v.dialogs[0].request = { type: 'user_dialog_request', requestId: request.requestId, dialogKind: 'custom' }; },
    'date-parse': (v: any) => { v.dialogs[0].owner.expiresAt = '18 Oct 2026 00:01:00 GMT'; },
    'empty-reason': (v: any) => { v.dialogs[0].answer.result = { behavior: 'cancelled', reason: '' }; },
    'undefined-answer': (v: any) => { v.dialogs[0].answer.result = { behavior: 'answered' }; },
    'duplicate': (v: any) => { v.dialogs.push(structuredClone(v.dialogs[0])); },
    'version': (v: any) => { v.version = 2; },
    'bad-expiry': (v: any) => { v.dialogs[0].lease.expiresAt = 'invalid'; },
    'null-owner': (v: any) => { v.dialogs[0].owner = null; },
    'answer-id': (v: any) => { v.dialogs[0].answer.requestId = 'other'; },
  })) {
    const record = structuredClone(golden);
    mutate(record);
    await mkdir(join(pathFor(store), '..'), { recursive: true });
    const disk = JSON.stringify(record);
    await writeFile(pathFor(store), disk);
    let result: any;
    try { result = { list: await store.list(key), live: await store.listLive(key), answer: await store.takeLiveAnswer(key, request.requestId) }; }
    catch (error) { result = { invalid: (error as Error).message.endsWith(': invalid store record') }; }
    readCases.push({ name, disk, result });
  }
  const files = ['src/gateway/user-dialog/FileGatewayUserDialogStore.ts', 'src/gateway/user-dialog/GatewayUserDialogBus.ts', 'src/gateway/user-dialog/GatewayUserDialogStore.ts', 'src/gateway/user-dialog/GatewayUserDialogChannel.ts', 'src/gateway/client/InProcessGateway.ts', 'src/cli/ProjectRuntimeRegistry.ts'];
  const sourceSha256 = Object.fromEntries(await Promise.all(files.map(async path => [path, createHash('sha256').update(await readFile(join(baseline, path))).digest('hex')])));
  const command = 'PDRS_BASELINE=/absolute/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/user-dialog/generate.mts src/pilotdeck-gateway/tests/fixtures/user-dialog';
  await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, command, key, request, createdAt, initialMs, relativePath, steps, busSteps, responses, resolved, hostedProjection, liveProjection, recoveredProjection, readCases }, null, 2) + '\n');
  console.log(`PASS: ${steps.length} file-store steps, ${busSteps.length} bus steps, ${readCases.length} reader cases, real response validation and hosted/live/recovered projections`);
} finally { await rm(scratch, { recursive: true, force: true }); }
