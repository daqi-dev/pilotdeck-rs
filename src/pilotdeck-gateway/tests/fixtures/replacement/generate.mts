// Execute unmodified fixed-baseline modules in memory; no writes to baseline/tsx cache.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/gateway/client/GatewayTurnReplacementCoordinator.ts',
  'src/gateway/client/InProcessGateway.ts', 'src/gateway/dialog/errors.ts', 'src/gateway/SessionRouter.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
let timers = new Map<any, () => void>();
const setTimer = (fn: () => void) => { const id = { unref() {} }; timers.set(id, fn); return id; };
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', 'setTimeout', 'clearTimeout', code)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get(_t, key) {
      if (key === '__esModule') return true;
      throw new Error(`unexercised import: ${source} -> ${name}.${String(key)}`);
    } });
  }, module, module.exports, setTimer, (id: any) => timers.delete(id));
  return module.exports;
}
const errors = await load(sources[2]);
const { GatewayTurnReplacementCoordinator } = await load(sources[0], { '../dialog/errors.js': errors });
const { InProcessGateway } = await load(sources[1]);
const { SessionRouter } = await load(sources[3]);
const input = (extra = {}) => ({ sessionKey: 's', projectKey: 'p', expectedTurnId: 'old', replacementTurnId: 'new', extra: 7, ...extra });
const final = (action = 'rollback', extra = {}) => ({ sessionKey: 's', projectKey: 'p', transactionId: 'tx-s', action, ...extra });
const op = (op: string, extra = {}) => ({ op, ...extra });
const replace = (extra = {}) => op('replace', { input: input(), ...extra });
const finalize = (extra = {}) => op('finalize', { input: final(), ...extra });
const claim = (run = 'new') => op('claim', { run });
const fail = (name: string) => op('fail', { name });
const config = (name: string) => op('config', { name });
const specs: any[] = [
  { name: 'replace_abort_close_commit', steps: [op('active', { run: 'old' }), replace(), claim('wrong'), claim(), claim(), op('release', { run: 'wrong' }), finalize(), op('commit', { run: 'wrong' }), fail('finalize'), op('commit', { run: 'new' }), claim()] },
  { name: 'claim_release_rollback', steps: [replace(), claim(), op('release', { run: 'new' }), finalize(), claim()] },
  { name: 'concurrent_replace_and_writer', steps: [op('gate', { name: 'close' }), replace({ start: 'r' }), op('has'), replace(), op('reserve'), op('has'), op('ungate', { name: 'close' }), op('await', { id: 'r' }), op('reserve'), replace(), finalize()] },
  { name: 'different_sessions_independent', steps: [op('gate', { name: 'close' }), replace({ start: 'r' }), op('ungate', { name: 'close' }), replace({ input: input({ sessionKey: 't' }) }), op('await', { id: 'r' }), op('dispose')] },
  { name: 'dispose_during_replace', steps: [op('gate', { name: 'replace' }), replace({ start: 'r' }), op('dispose'), op('ungate', { name: 'replace' }), op('await', { id: 'r' }), claim(), op('release', { run: 'new' }), finalize()] },
  ...['abort', 'close', 'replace'].map(name => ({ name: `replace_failure_${name}`, steps: [op('active', { run: 'old' }), fail(name), replace(), op('has'), op('active'), op('fail'), replace(), finalize()] })),
  { name: 'active_conflict_and_empty', steps: [op('active', { run: 'other' }), replace(), op('active', { run: '' }), replace(), finalize()] },
  { name: 'invalid_replacement_ids', steps: [replace({ input: input({ replacementTurnId: '' }) }), replace({ input: input({ replacementTurnId: ' \ufeff\u2029' }) }), replace({ input: input({ replacementTurnId: 3 }) }), replace({ input: input({ replacementTurnId: '\u0085' }) }), op('dispose')] },
  { name: 'writer_reservations', steps: [op('reserve'), op('has'), op('reserve'), replace(), op('releaseWrite'), op('has'), replace(), op('has'), op('reserve'), op('dispose'), op('reserve'), op('releaseWrite')] },
  { name: 'transaction_identity', steps: [finalize(), replace(), finalize({ input: final('rollback', { transactionId: 'stale' }) }), finalize(), finalize()] },
  { name: 'finalize_active_retry', steps: [replace(), op('active', { run: 'new' }), finalize(), claim(), op('release', { run: 'new' }), op('active'), finalize()] },
  ...['close', 'finalize'].map(name => ({ name: `rollback_failure_${name}`, steps: [replace(), fail(name), finalize(), op('fail'), finalize()] })),
  { name: 'finalize_blocks_claim', steps: [replace(), op('gate', { name: 'finalize' }), finalize({ input: final('commit'), start: 'f' }), claim(), finalize(), op('ungate', { name: 'finalize' }), op('await', { id: 'f' }), claim()] },
  { name: 'timeout_rollback', steps: [replace(), op('expire'), claim()] },
  { name: 'timeout_active_retry', steps: [replace(), op('active', { run: 'new' }), op('expire'), claim(), op('release', { run: 'new' }), op('active'), op('expire'), claim()] },
  { name: 'timeout_failure_retry', steps: [replace(), fail('finalize'), op('expire'), op('fail'), op('expire'), claim()] },
  { name: 'timeout_close_failure', steps: [replace(), fail('close'), op('expire'), op('fail'), op('expire'), claim()] },
  { name: 'claim_cancels_timeout', steps: [replace(), claim(), op('expire'), op('commit', { run: 'new' }), op('expire')] },
  { name: 'release_rearms_timeout', steps: [replace(), claim(), op('release', { run: 'new' }), op('expire')] },
  { name: 'dispose_cancels_timeout', steps: [replace(), op('dispose'), op('expire'), claim(), op('has')] },
  { name: 'missing_ports', steps: [config('none'), op('fork', { input: input() }), replace(), finalize(), config('replaceOnly'), replace(), finalize(), config('all'), op('fork', { input: input() })] },
  { name: 'fork_forward_and_failure', steps: [op('fork', { input: { sessionKey: 's', arbitrary: ['verbatim'] } }), fail('fork'), op('fork', { input: { sessionKey: 't' } })] },
  { name: 'stale_finalize_failure_preserves_new_claim', steps: [replace(), op('gate', { name: 'finalize' }), fail('finalize'), finalize({ input: final('commit'), start: 'f' }), op('dispose'), replace({ input: input({ replacementTurnId: 'newer' }) }), claim('newer'), op('ungate', { name: 'finalize' }), op('await', { id: 'f' }), claim('newer'), op('commit', { run: 'newer' })] },
  { name: 'stale_timeout_failure_preserves_new_claim', steps: [replace(), op('gate', { name: 'finalize' }), fail('finalize'), op('expire'), op('dispose'), replace({ input: input({ replacementTurnId: 'newer' }) }), claim('newer'), op('ungate', { name: 'finalize' }), claim('newer'), op('commit', { run: 'newer' })] },
  { name: 'optional_project', steps: [replace({ input: { sessionKey: 's', expectedTurnId: 'old', replacementTurnId: ' new ' } }), claim(' new '), op('commit', { run: ' new ' })] },
];
const settle = async () => { for (let n = 0; n < 80; n++) await Promise.resolve(); };
const outcome = async (p: any) => { try { const value = await p; return { ok: value ?? null }; }
  catch (e: any) { return { error: e.code ?? null, message: e.message, details: e.details ?? null }; } };
const cases: any[] = [];
for (const spec of specs) {
  timers = new Map();
  const events: any[] = [], values: any[] = [], waiting = new Map<string, Promise<any>>();
  const gates = new Map<string, { promise: Promise<void>, open: () => void }>();
  let active: string | undefined, failure: string | undefined;
  const call = async (name: string, arg: any) => {
    events.push({ call: name, input: arg });
    await gates.get(name)?.promise;
    if (failure === name) throw new Error(`${name} failed`);
    if (name === 'replace') return { transactionId: `tx-${arg.sessionKey}`, persisted: true };
    if (name === 'fork') return { forked: arg };
    if (name === 'finalize') return { finalized: arg.action, transactionId: arg.transactionId };
  };
  const storage: any = { replaceLastTurn: (arg: any) => call('replace', arg), finalizeLastTurnReplacement: (arg: any) => call('finalize', arg) };
  const session = { activeTurnRunId: () => active, hasActiveTurn: () => active !== undefined,
    abortExpectedTurn: (sessionKey: string, runId: string) => call('abort', { sessionKey, runId }),
    closeSession: (sessionKey: string) => call('close', { sessionKey }) };
  const c = new GatewayTurnReplacementCoordinator({ storage, session, timeoutMs: 10,
    warn: (message: string, error: any) => events.push({ warn: message, error: error.message }) });
  const gateway = Object.create(InProcessGateway.prototype);
  gateway.options = { forkSession: (arg: any) => call('fork', arg) };
  gateway.turnReplacementCoordinator = c;
  for (const step of spec.steps) {
    let operation: any;
    switch (step.op) {
      case 'config': storage.replaceLastTurn = step.name === 'none' ? undefined : (arg: any) => call('replace', arg);
        storage.finalizeLastTurnReplacement = step.name === 'all' ? (arg: any) => call('finalize', arg) : undefined;
        gateway.options.forkSession = step.name === 'none' ? undefined : (arg: any) => call('fork', arg); break;
      case 'active': active = step.run; break;
      case 'fail': failure = step.name; break;
      case 'gate': { let open!: () => void; const promise = new Promise<void>(r => { open = r; }); gates.set(step.name, { promise, open }); break; }
      case 'ungate': gates.get(step.name)!.open(); gates.delete(step.name); break;
      case 'replace': operation = gateway.replaceLastTurn(step.input); break;
      case 'finalize': operation = gateway.finalizeLastTurnReplacement(step.input); break;
      case 'fork': operation = gateway.forkSession(step.input); break;
      case 'commit': operation = c.commitAcceptedInput('s', step.run); break;
      case 'await': values.push({ op: step.op, id: step.id, result: await waiting.get(step.id) }); break;
      case 'claim': values.push({ op: step.op, value: c.claimForSubmit('s', step.run) }); break;
      case 'release': c.releaseSubmitClaim('s', step.run); break;
      case 'reserve': values.push({ op: step.op, result: await outcome(Promise.resolve().then(() => c.reserveTranscriptWrite('s', 'edit'))) }); break;
      case 'releaseWrite': c.releaseTranscriptWrite('s'); break;
      case 'has': values.push({ op: step.op, value: c.hasTranscriptWriteReservation('s') }); break;
      case 'dispose': c.dispose(); break;
      case 'expire': { const ready = [...timers]; for (const [id, fn] of ready) { timers.delete(id); fn(); } break; }
      default: throw new Error(`unknown operation ${step.op}`);
    }
    if (operation) {
      const result = outcome(operation);
      if (step.start) waiting.set(step.start, result);
      else values.push({ op: step.op, result: await result });
    }
    await settle();
  }
  c.dispose();
  assert.equal(timers.size, 0);
  cases.push({ ...spec, events, values });
}
const maintenance: any[] = [];
for (const stopped of [false, true]) {
  const router = Object.create(SessionRouter.prototype);
  router.isShutdown = stopped;
  router.inFlightTurns = new Set(['s']);
  router.inFlightMaintenance = new Set();
  maintenance.push(await outcome(router.compact('s')));
}
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases, maintenance }, null, 2) + '\n');
console.log(`recorded ${cases.length} real TS coordinator/gateway cases plus maintenance precedence`);
