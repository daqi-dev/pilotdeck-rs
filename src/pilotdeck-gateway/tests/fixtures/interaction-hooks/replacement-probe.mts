// PDRS_BASELINE=<cc7c4e6 checkout> node --import <baseline>/node_modules/tsx/dist/loader.mjs replacement-probe.mts
// Deterministic HookRuntime/LifecycleRuntime ordering probe. It imports the
// real cc7c4e6 source, never the Rust port, and writes no baseline file.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

const baseline = resolve(process.env.PDRS_BASELINE!);
assert.ok(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(baseline, commit, '/usr/bin/git');

const sources = [
  'src/extension/hooks/execution/HookRuntime.ts',
  'src/extension/hooks/execution/CallbackHookExecutor.ts',
  'src/extension/hooks/execution/AsyncHookRegistry.ts',
  'src/lifecycle/runtime/LifecycleRuntime.ts',
];
const [hookRuntimeModule, , , lifecycleModule] = await Promise.all(
  sources.map((path) => import(pathToFileURL(join(baseline, path)).href)),
);
const { HookRuntime } = hookRuntimeModule;
const { LifecycleRuntime } = lifecycleModule;

function deferred() {
  let resolve;
  const promise = new Promise((r) => { resolve = r; });
  return { promise, resolve };
}
function settings() {
  return { PreToolUse: [{ hooks: [{ type: 'callback', name: 'gate' }] }] };
}
function baseInput() {
  return { sessionId: 's', transcriptPath: '', cwd: process.cwd() };
}
function dispatch(runtime, payload, signal) {
  return runtime.dispatch({
    event: 'PreToolUse',
    baseInput: baseInput(),
    payload,
    matchQuery: 'PreToolUse',
    signal,
    env: { PATH: '/usr/bin:/bin' },
  });
}
function registerGate(hooks, handler) {
  hooks.getCallbackExecutor().register('gate', handler);
}
async function tick() {
  await new Promise((resolve) => setImmediate(resolve));
}
async function settled(promise) {
  try {
    await promise;
    return { ok: true };
  } catch (error) {
    return { ok: false, message: error instanceof Error ? error.message : String(error) };
  }
}
const observations = {};
function abortable(signal) {
  if (signal?.aborted) return Promise.resolve('already-aborted');
  return new Promise((resolve) => signal?.addEventListener('abort', () => resolve('aborted'), { once: true }));
}

// 1. Same session key is cached by the host: the same lifecycle runtime is
// reused. A second hook item starts while the first callback is still pending.
{
  const log = [];
  const startedA = deferred();
  const gateA = deferred();
  const gateB = deferred();
  const hooks = new HookRuntime(settings());
  registerGate(hooks, async ({ hookInput }) => {
    const tag = hookInput.tag;
    log.push(`start:${tag}`);
    if (tag === 'A') { startedA.resolve(); await gateA.promise; }
    if (tag === 'B') await gateB.promise;
    log.push(`end:${tag}`);
    return {};
  });
  const sessions = new Map();
  const session = new LifecycleRuntime(hooks);
  sessions.set('s', session);
  const first = dispatch(sessions.get('s'), { tag: 'A' });
  await startedA.promise;
  const second = dispatch(sessions.get('s'), { tag: 'B' });
  await tick();
  const secondStartedBeforeFirstEnd = log.includes('start:B');
  gateB.resolve();
  await second;
  const firstPending = !log.includes('end:A');
  gateA.resolve();
  await first;
  assert.deepEqual(log, ['start:A', 'start:B', 'end:B', 'end:A']);
  await session.dispose();
  observations.same_session_runtime = {
    same_session_key_reused: true,
    second_started_before_first_end: secondStartedBeforeFirstEnd,
    first_pending_when_second_done: firstPending,
    order: log,
  };
}

// 2. Dropping the consumer stream does not dispose the session runtime; the
// old hook keeps running and a new hook item is accepted on the same runtime.
{
  const log = [];
  const startedA = deferred();
  const gateA = deferred();
  const hooks = new HookRuntime(settings());
  registerGate(hooks, async ({ hookInput }) => {
    const tag = hookInput.tag;
    log.push(`start:${tag}`);
    if (tag === 'A') { startedA.resolve(); await gateA.promise; }
    log.push(`end:${tag}`);
    return {};
  });
  const session = new LifecycleRuntime(hooks);
  void dispatch(session, { tag: 'A' }); // consumer never awaits this stream
  await startedA.promise;
  await dispatch(session, { tag: 'B' });
  const oldStillRunning = !log.includes('end:A');
  gateA.resolve();
  await tick();
  await session.dispose();
  observations.dropped_stream = {
    dropped_stream_kept_old_run: oldStillRunning,
    new_item_accepted_while_old_pending: true,
    order: log,
  };
}

// 3. Cancellation reaches the old callback and leaves the session runtime
// active for the next hook item; it does not create a replacement runtime.
{
  const log = [];
  const started = deferred();
  const controller = new AbortController();
  const hooks = new HookRuntime(settings());
  registerGate(hooks, async ({ hookInput, signal }) => {
    log.push(`start:${hookInput.tag}`);
    if (hookInput.tag === 'A') {
      started.resolve();
      log.push(await abortable(signal));
    }
    log.push(`end:${hookInput.tag}`);
    return {};
  });
  const session = new LifecycleRuntime(hooks);
  const first = dispatch(session, { tag: 'A' }, controller.signal);
  await started.promise;
  controller.abort();
  await first;
  const afterCancel = await dispatch(session, { tag: 'B' });
  await session.dispose();
  observations.cancel = {
    old_callback_observed: log.includes('aborted'),
    runtime_state_after_cancel: 'active',
    next_item_accepted: afterCancel !== undefined,
    order: log,
  };
}

// 4. A replacement runtime can start immediately while the old runtime is
// still draining its in-flight callback; runs on the disposed old runtime are
// rejected with its lifecycle state. This is the per-run replacement shape the
// Rust port uses (TS host normally reuses the cached session runtime instead).
{
  const log = [];
  const started = deferred();
  const gate = deferred();
  const oldHooks = new HookRuntime(settings());
  registerGate(oldHooks, async ({ hookInput }) => {
    log.push(`start:${hookInput.tag}`);
    if (hookInput.tag === 'old') { started.resolve(); await gate.promise; }
    log.push(`end:${hookInput.tag}`);
    return {};
  });
  const oldRuntime = new LifecycleRuntime(oldHooks);
  const oldDispatch = dispatch(oldRuntime, { tag: 'old' });
  await started.promise;
  const disposing = oldRuntime.dispose();
  const oldState = oldHooks.lifecycleState;
  const rejected = await settled(dispatch(oldRuntime, { tag: 'late-old' }));

  const newHooks = new HookRuntime(settings());
  registerGate(newHooks, async ({ hookInput }) => {
    log.push(`start:${hookInput.tag}`);
    log.push(`end:${hookInput.tag}`);
    return {};
  });
  const newRuntime = new LifecycleRuntime(newHooks);
  await dispatch(newRuntime, { tag: 'replacement' });
  const replacementBeforeOldDrain = !log.includes('end:old');
  gate.resolve();
  await oldDispatch;
  await disposing;
  const afterDispose = await settled(dispatch(oldRuntime, { tag: 'after-old-cleanup' }));
  await newRuntime.dispose();
  assert.equal(rejected.ok, false);
  assert.equal(afterDispose.ok, false);
  observations.replacement = {
    old_state_while_inflight: oldState,
    late_old_rejected: rejected.message,
    replacement_started_before_old_drain: replacementBeforeOldDrain,
    old_disposed_state: oldHooks.lifecycleState,
    after_cleanup_old_rejected: afterDispose.message,
    order: log,
  };
}

// 5. Session-scoped async pending hooks are dropped with the old runtime; a
// replacement starts with an empty pending set (existing A6.9 observation).
{
  const asyncSettings = {
    PreToolUse: [{ hooks: [{ type: 'command', command: `printf '%s' '{"async":true}'`, asyncRewake: true }] }],
  };
  const oldHooks = new HookRuntime(asyncSettings);
  const oldRuntime = new LifecycleRuntime(oldHooks);
  const pending = await dispatch(oldRuntime, {});
  const pendingCount = pending.pendingAsyncHooks.length;
  await oldRuntime.dispose();
  const afterDispose = oldHooks.collectAsyncResponses().length;
  const newHooks = new HookRuntime(asyncSettings);
  const newRuntime = new LifecycleRuntime(newHooks);
  const replacement = await dispatch(newRuntime, {});
  await newRuntime.dispose();
  observations.async_pending = {
    old_pending_before_dispose: pendingCount,
    old_pending_after_dispose: afterDispose,
    replacement_pending: replacement.pendingAsyncHooks.length,
  };
}

const sourceSha256 = Object.fromEntries(
  sources.map((path) => [path, createHash('sha256').update(readFileSync(join(baseline, path))).digest('hex')]),
);
console.log(JSON.stringify({ commit, sourceSha256, observations }, null, 2));
