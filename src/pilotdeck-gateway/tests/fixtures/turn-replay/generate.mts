// Run from the repository root with the fixed baseline's tsx loader; see README.md.
// Executes the real GatewayTurnReplayStore and GatewayTurnEventCoordinator. The
// InProcessGateway.readSessionMessages method is called through its real
// prototype with only the durable reader and coordinator injected, so the
// sameTurnEpoch retry loop is the product implementation, not a copy.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/client/GatewayTurnReplayStore.ts',
  'src/gateway/client/GatewayTurnEventCoordinator.ts',
  'src/gateway/client/InProcessGateway.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get(_target, property) {
      if (property === '__esModule') return true;
      throw new Error(`unexercised import accessed: ${source} -> ${name}.${String(property)}`);
    } });
  }, module, module.exports);
  return module.exports;
}
const storeModule = await load(sources[0]);
const coordinatorModule = await load(sources[1], {
  './GatewayTurnReplayStore.js': storeModule,
});
const { InProcessGateway } = await load(sources[2], {
  './GatewayTurnEventCoordinator.js': coordinatorModule,
  './GatewayTurnReplayStore.js': storeModule,
});
const { GatewayTurnReplayStore } = storeModule;
const { GatewayTurnEventCoordinator } = coordinatorModule;

const byteLength = (value: any) => Buffer.byteLength(JSON.stringify(value), 'utf8');
const paddingEmpty = { type: 'padding', text: '' };
const paddingBase = byteLength(paddingEmpty);
function paddingEvent(jsonBytes: number) {
  return { type: 'padding', text: 'a'.repeat(jsonBytes - paddingBase) };
}
// Intercept setTimeout so delays (notably the 30000 ms terminal retention) are
// recorded and expiry is driven by an explicit clock, never wall time.
type Clock = { advance(ms: number): void; delays: number[] };
async function withTimers<T>(run: (clock: Clock) => Promise<T>): Promise<T> {
  const globals = globalThis as any;
  const realSetTimeout = globals.setTimeout;
  const realClearTimeout = globals.clearTimeout;
  const timers = new Map<number, { at: number; seq: number; callback: () => void }>();
  const delays: number[] = [];
  let now = 0;
  let next = 0;
  globals.setTimeout = (callback: (...args: any[]) => void, ms?: number, ...args: any[]) => {
    const raw = Number(ms);
    const delay = Number.isFinite(raw) ? Math.max(0, Math.trunc(raw)) : 1;
    delays.push(Number.isFinite(raw) ? raw : 1);
    const id = ++next;
    timers.set(id, { at: now + delay, seq: id, callback: () => callback(...args) });
    return { unref() {}, __turnTimerId: id };
  };
  globals.clearTimeout = (handle: any) => {
    if (handle && typeof handle === 'object' && '__turnTimerId' in handle) {
      timers.delete(handle.__turnTimerId);
    } else if (typeof handle === 'number') {
      timers.delete(handle);
    }
  };
  const advance = (ms: number) => {
    now += ms;
    for (;;) {
      const due = [...timers.values()]
        .filter(timer => timer.at <= now)
        .sort((left, right) => left.at - right.at || left.seq - right.seq);
      if (due.length === 0) return;
      const timer = due[0];
      timers.delete(timer.seq);
      timer.callback();
    }
  };
  try {
    return await run({ advance, delays });
  } finally {
    globals.setTimeout = realSetTimeout;
    globals.clearTimeout = realClearTimeout;
  }
}

const storeCases: any[] = [];
async function storeCase(name: string, options: any, steps: any[]) {
  storeCases.push(await withTimers(async clock => {
    const store = new GatewayTurnReplayStore(options ?? undefined);
    const results: any[] = [];
    for (const step of steps) {
      switch (step.op) {
        case 'start': store.start(step.session, step.run); results.push(null); break;
        case 'record': store.record(step.session, step.event); results.push(null); break;
        case 'recordMany':
          for (let index = 0; index < step.count; index += 1) {
            store.record(step.session, { type: 'tick', index });
          }
          results.push(null);
          break;
        case 'recordPadded': {
          const event = paddingEvent(step.jsonBytes);
          const actualBytes = byteLength(event);
          assert.equal(actualBytes, step.jsonBytes, `${name}: padding must hit the requested byte count`);
          store.record(step.session, event);
          results.push({ actualBytes });
          break;
        }
        case 'eventBytes': results.push({ bytes: byteLength(step.event) }); break;
        case 'retainTerminal': store.retainTerminal(step.session, step.run); results.push(null); break;
        case 'clearTerminal': store.clearTerminal(step.session); results.push(null); break;
        case 'withRunId': results.push(store.withRunId(step.session, step.event)); break;
        // gate7 P1 #2: the non-object edge of the same call. The TS spread
        // `{ ...event, runId }` spreads a string by UTF-16 code unit, an array
        // by index, and nothing for number/boolean; null throws a TypeError
        // from the `event.runId` read, recorded here by constructor name.
        case 'withRunIdEdge': {
          try {
            results.push({ value: store.withRunId(step.session, step.event) });
          } catch (error) {
            results.push({ thrown: (error as any)?.constructor?.name ?? 'throw' });
          }
          break;
        }
        case 'snapshot': results.push(store.snapshot({
          sessionKey: step.session,
          ...(step.includeEvents === false ? { includeEvents: false } : {}),
        })); break;
        case 'dispose': store.dispose(); results.push(null); break;
        case 'advanceTime': clock.advance(step.ms); results.push(null); break;
        default: throw new Error(`unknown store op ${step.op}`);
      }
    }
    return { name, options: options ?? null, steps, results, timerDelays: [...clock.delays] };
  }));
}

const multibyte = { type: 'text', text: '😀中' };
const multibyteSecond = { type: 'text', text: 'A😀中B' };
const loneSurrogate = { type: 'text', text: 'x\uD800y' };
const mixedSurrogate = { type: 'text', text: '😀\uD800中' };
const multibyteBytes = byteLength(multibyte) + byteLength(multibyteSecond);
const loneBytes = byteLength(loneSurrogate);
const mixedBytes = byteLength(mixedSurrogate);

await storeCase('default-count-limit-and-sticky-truncation', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'recordMany', session: 's', count: 501 },
  { op: 'snapshot', session: 's' },
  { op: 'start', session: 's', run: 'r2' },
  { op: 'snapshot', session: 's' },
]);
await storeCase('custom-event-limit', { eventLimit: 3, byteLimit: 1_000_000 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'recordMany', session: 's', count: 5 },
  { op: 'snapshot', session: 's' },
]);
await storeCase('multibyte-byte-boundary-one-over', { byteLimit: multibyteBytes - 1 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'eventBytes', event: multibyte },
  { op: 'record', session: 's', event: multibyte },
  { op: 'eventBytes', event: multibyteSecond },
  { op: 'record', session: 's', event: multibyteSecond },
  { op: 'snapshot', session: 's' },
]);
await storeCase('lone-surrogate-exact-limit-kept', { byteLimit: loneBytes }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'eventBytes', event: loneSurrogate },
  { op: 'record', session: 's', event: loneSurrogate },
  { op: 'snapshot', session: 's' },
]);
await storeCase('lone-surrogate-one-over-drops-itself', { byteLimit: loneBytes - 1 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: loneSurrogate },
  { op: 'snapshot', session: 's' },
]);
await storeCase('mixed-surrogate-multibyte-exact-limit-kept', { byteLimit: mixedBytes }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'eventBytes', event: mixedSurrogate },
  { op: 'record', session: 's', event: mixedSurrogate },
  { op: 'snapshot', session: 's' },
]);
await storeCase('mixed-surrogate-multibyte-one-over-drops-itself', { byteLimit: mixedBytes - 1 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: mixedSurrogate },
  { op: 'snapshot', session: 's' },
]);
await storeCase('single-event-over-custom-byte-limit', { eventLimit: 100, byteLimit: 10 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: { type: 'x', text: '0123456789' } },
  { op: 'snapshot', session: 's' },
]);
await storeCase('default-byte-limit-exact-boundary', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'recordPadded', session: 's', jsonBytes: 256 * 1024 },
  { op: 'snapshot', session: 's', includeEvents: false },
  { op: 'record', session: 's', event: { type: 'small' } },
  { op: 'snapshot', session: 's' },
]);
await storeCase('default-byte-limit-one-over', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'recordPadded', session: 's', jsonBytes: 256 * 1024 + 1 },
  { op: 'snapshot', session: 's', includeEvents: false },
  { op: 'snapshot', session: 's' },
]);
await storeCase('terminal-default-30s-expiry', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: { type: 'kept' } },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'snapshot', session: 's' },
  { op: 'advanceTime', ms: 29_999 },
  { op: 'snapshot', session: 's' },
  { op: 'advanceTime', ms: 1 },
  { op: 'snapshot', session: 's' },
]);
await storeCase('terminal-custom-retention-and-restart-cancels-timer', { terminalRetentionMs: 5000 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'advanceTime', ms: 4999 },
  { op: 'snapshot', session: 's' },
  { op: 'start', session: 's', run: 'r2' },
  { op: 'snapshot', session: 's' },
  { op: 'advanceTime', ms: 1 },
  { op: 'snapshot', session: 's' },
]);
await storeCase('terminal-mismatched-run-is-noop', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'other' },
  { op: 'snapshot', session: 's' },
]);
await storeCase('terminal-retain-twice-resets-deadline', { terminalRetentionMs: 1000 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'advanceTime', ms: 600 },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'advanceTime', ms: 400 },
  { op: 'snapshot', session: 's' },
  { op: 'advanceTime', ms: 600 },
  { op: 'snapshot', session: 's' },
]);
// retention 0 is still a setTimeout(0): the synchronous snapshot after
// retainTerminal sees the terminal replay, and only advancing the fake clock
// (running the next timer turn) removes it.
await storeCase('terminal-zero-retention-synchronous-snapshot', { terminalRetentionMs: 0 }, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: { type: 'kept' } },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'snapshot', session: 's' },
  { op: 'advanceTime', ms: 0 },
  { op: 'snapshot', session: 's' },
]);
await storeCase('record-while-terminal-then-clear', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: { type: 'late' } },
  { op: 'snapshot', session: 's' },
  { op: 'clearTerminal', session: 's' },
  { op: 'snapshot', session: 's' },
]);
await storeCase('clear-terminal-active-keeps-replay', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'clearTerminal', session: 's' },
  { op: 'snapshot', session: 's' },
]);
await storeCase('dispose-clears-replay-and-timer', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'dispose' },
  { op: 'snapshot', session: 's' },
  { op: 'advanceTime', ms: 30_000 },
  { op: 'snapshot', session: 's' },
]);
await storeCase('with-run-id-variants', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'withRunId', session: 's', event: { type: 'a' } },
  { op: 'withRunId', session: 's', event: { type: 'b', runId: 'own' } },
  { op: 'withRunId', session: 's', event: { type: 'c', runId: '  \t\uFEFF' } },
  { op: 'withRunId', session: 's', event: { type: 'd', runId: 5 } },
  { op: 'withRunId', session: 's', event: { runId: 'own', type: 'e' } },
  { op: 'withRunId', session: 'missing', event: { type: 'f' } },
  { op: 'dispose' },
  { op: 'withRunId', session: 's', event: { type: 'g' } },
]);
// gate7 P1 #2: withRunId on the five non-object event kinds the TS spread
// handles differently \u2014 the surrogate pair plus the lone surrogate in the
// string pin the UTF-16-code-unit spread (not code points).
await storeCase('with-run-id-non-object-spread', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'withRunIdEdge', session: 's', event: 5 },
  { op: 'withRunIdEdge', session: 's', event: true },
  { op: 'withRunIdEdge', session: 's', event: 'a\u{1F600}\uD800b' },
  { op: 'withRunIdEdge', session: 's', event: ['a', 2, null] },
  { op: 'withRunIdEdge', session: 's', event: null },
  { op: 'withRunIdEdge', session: 's', event: { type: 'obj' } },
  { op: 'withRunIdEdge', session: 'missing', event: 5 },
]);
await storeCase('snapshot-without-replay', null, [
  { op: 'snapshot', session: 'never' },
  { op: 'snapshot', session: 'never', includeEvents: false },
]);

const coordinatorCases: any[] = [];
async function coordinatorCase(name: string, options: any, steps: any[]) {
  coordinatorCases.push(await withTimers(async clock => {
    const replayStore = new GatewayTurnReplayStore(options ?? undefined);
    const coordinator = new GatewayTurnEventCoordinator({ replayStore });
    const delivered: { sink: string; event: any }[] = [];
    const markers: string[] = [];
    let sinkSequence = 0;
    const results: any[] = [];
    for (const step of steps) {
      switch (step.op) {
        case 'start': {
          sinkSequence += 1;
          const sink = `s${sinkSequence}`;
          coordinator.start(step.session, step.run, event => {
            delivered.push({ sink, event: JSON.parse(JSON.stringify(event)) });
          });
          results.push(null);
          break;
        }
        // A sink that re-emits from inside itself, recording markers around
        // the nested call so the golden pins the TS return boundary: the
        // inner delivery runs inside the inner `emit` call.
        case 'startNested': {
          sinkSequence += 1;
          const sink = `s${sinkSequence}`;
          const session = step.session;
          coordinator.start(step.session, step.run, event => {
            delivered.push({ sink, event: JSON.parse(JSON.stringify(event)) });
            if (event.type === 'inner') markers.push('inner-enter');
            if (event.type === 'outer') {
              markers.push('outer-enter');
              const returned = coordinator.emit(session, { type: 'inner' });
              markers.push(`inner-returned:${returned}`);
              markers.push('outer-exit');
            }
          });
          results.push(null);
          break;
        }
        // A sink whose own body throws for the inner event: the nested emit
        // still delivers the inner event into the sink, the throw surfaces at
        // the nested emit call site, and the outer sink continues.
        case 'startThrowing': {
          sinkSequence += 1;
          const sink = `s${sinkSequence}`;
          const session = step.session;
          coordinator.start(step.session, step.run, event => {
            delivered.push({ sink, event: JSON.parse(JSON.stringify(event)) });
            if (event.type === 'inner') {
              markers.push('inner-sink');
              throw new Error('inner sink failed');
            }
            if (event.type === 'outer') {
              markers.push('outer-enter');
              try {
                coordinator.emit(session, { type: 'inner' });
                markers.push('not-reached');
              } catch {
                markers.push('caught-at-inner');
              }
              markers.push('outer-exit');
            }
          });
          results.push(null);
          break;
        }
        // A sink that catches a failing nested `emit` at the call site, like
        // a TS try/catch around the same call. `undefined` has no GatewayEvent
        // shape on purpose: TS throws from inside the nested emit.
        case 'startCatching': {
          sinkSequence += 1;
          const sink = `s${sinkSequence}`;
          const session = step.session;
          coordinator.start(step.session, step.run, event => {
            if (event.type === 'trigger') {
              markers.push('trigger-enter');
              try {
                coordinator.emit(session, undefined as any);
                markers.push('not-reached');
              } catch {
                markers.push('caught');
              }
              markers.push('continued');
            }
            delivered.push({ sink, event: JSON.parse(JSON.stringify(event)) });
          });
          results.push(null);
          break;
        }
        case 'markers': {
          results.push([...markers]);
          break;
        }
        case 'emit': {
          const before = delivered.length;
          const accepted = coordinator.emit(step.session, step.event);
          results.push({ accepted, delivered: delivered.slice(before) });
          break;
        }
        case 'record': coordinator.record(step.session, step.event); results.push(null); break;
        case 'retainTerminal': coordinator.retainTerminal(step.session, step.run); results.push(null); break;
        case 'snapshot': results.push(coordinator.snapshot({
          sessionKey: step.session,
          ...(step.includeEvents === false ? { includeEvents: false } : {}),
        })); break;
        case 'dispose': coordinator.dispose(); results.push(null); break;
        case 'advanceTime': clock.advance(step.ms); results.push(null); break;
        default: throw new Error(`unknown coordinator op ${step.op}`);
      }
    }
    return { name, options: options ?? null, steps, results, timerDelays: [...clock.delays] };
  }));
}

await coordinatorCase('emit-requires-a-started-sink', null, [
  { op: 'emit', session: 's', event: { type: 'ignored' } },
  { op: 'snapshot', session: 's' },
]);
await coordinatorCase('emit-fills-run-and-records-before-delivery', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'emit', session: 's', event: { type: 'x' } },
  { op: 'snapshot', session: 's' },
  { op: 'snapshot', session: 's', includeEvents: false },
]);
await coordinatorCase('emit-preserves-own-run-and-blank-run-becomes-current', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'emit', session: 's', event: { type: 'own', runId: 'mine' } },
  { op: 'emit', session: 's', event: { type: 'blank', runId: ' \t' } },
  { op: 'snapshot', session: 's' },
]);
await coordinatorCase('record-does-not-deliver', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: { type: 'silent' } },
  { op: 'snapshot', session: 's' },
]);
await coordinatorCase('start-replaces-sink-and-buffer', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'emit', session: 's', event: { type: 'old' } },
  { op: 'start', session: 's', run: 'r2' },
  { op: 'emit', session: 's', event: { type: 'new' } },
  { op: 'snapshot', session: 's' },
]);
await coordinatorCase('retain-terminal-removes-sink-even-on-run-mismatch', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'other' },
  { op: 'emit', session: 's', event: { type: 'dropped' } },
  { op: 'snapshot', session: 's' },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'snapshot', session: 's' },
]);
await coordinatorCase('dispose-clears-sinks-and-replay', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'emit', session: 's', event: { type: 'before' } },
  { op: 'dispose' },
  { op: 'emit', session: 's', event: { type: 'after' } },
  { op: 'snapshot', session: 's' },
]);
await coordinatorCase('record-after-terminal-keeps-event-without-sink', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'record', session: 's', event: { type: 'late' } },
  { op: 'emit', session: 's', event: { type: 'ignored' } },
  { op: 'snapshot', session: 's' },
]);
await coordinatorCase('coordinator-terminal-expiry-uses-default-30s', null, [
  { op: 'start', session: 's', run: 'r1' },
  { op: 'retainTerminal', session: 's', run: 'r1' },
  { op: 'advanceTime', ms: 30_000 },
  { op: 'snapshot', session: 's' },
]);
// codex-r2 P1 #3: a reentrant emit from inside the session's own sink runs
// its delivery inside the nested call — markers prove the inner sink ran
// before the inner emit returned, and that the outer sink finished last.
await coordinatorCase('reentrant-emit-delivers-inner-before-outer-returns', null, [
  { op: 'startNested', session: 's', run: 'r1' },
  { op: 'emit', session: 's', event: { type: 'outer' } },
  { op: 'markers' },
  { op: 'emit', session: 's', event: { type: 'plain' } },
  { op: 'markers' },
]);
// Same boundary, failure path: the nested emit throws at the call site, the
// outer sink catches it and continues, and nothing is recorded or delivered
// for the failed event.
await coordinatorCase('reentrant-emit-failure-is-caught-at-the-inner-call-site', null, [
  { op: 'startCatching', session: 's', run: 'r1' },
  { op: 'emit', session: 's', event: { type: 'trigger' } },
  { op: 'markers' },
  { op: 'emit', session: 's', event: { type: 'plain' } },
  { op: 'snapshot', session: 's' },
]);
// Same boundary, with the failure raised by the inner sink itself: the inner
// event is recorded and delivered, the sink's throw surfaces at the nested
// emit call site, and the outer sink finishes. A later emit still delivers.
await coordinatorCase('reentrant-emit-inner-sink-throw-is-caught-at-the-inner-call-site', null, [
  { op: 'startThrowing', session: 's', run: 'r1' },
  { op: 'emit', session: 's', event: { type: 'outer' } },
  { op: 'markers' },
  { op: 'emit', session: 's', event: { type: 'plain' } },
  { op: 'markers' },
  { op: 'snapshot', session: 's' },
]);

const epochCases: any[] = [];
async function epochCase(
  name: string,
  options: any,
  setup: ((store: any, coordinator: any) => void) | null,
  actions: ((store: any, coordinator: any, clock: Clock) => void)[],
) {
  epochCases.push(await withTimers(async clock => {
    const store = new GatewayTurnReplayStore(options ?? undefined);
    const coordinator = new GatewayTurnEventCoordinator({ replayStore: store });
    setup?.(store, coordinator);
    const attempts: any[] = [];
    let calls = 0;
    const gateway = Object.create(InProcessGateway.prototype);
    gateway.turnEventCoordinator = coordinator;
    gateway.options = {
      readSessionMessages: async (input: any) => {
        const index = calls;
        calls += 1;
        const before = coordinator.snapshot({ sessionKey: input.sessionKey, includeEvents: false });
        actions[index]?.(store, coordinator, clock);
        const after = coordinator.snapshot({ sessionKey: input.sessionKey });
        attempts.push({ before, after });
        return { messages: [], attempt: index };
      },
    };
    let outcome: any;
    try {
      outcome = { result: await gateway.readSessionMessages({ sessionKey: 's' }) };
    } catch (error) {
      outcome = { error: (error as Error).message };
    }
    if (outcome.error !== undefined) {
      assert.equal(attempts.length, 3, `${name}: a retry error requires all three attempts`);
      assert.equal(outcome.error, 'Session changed during transcript synchronization; retry the snapshot.');
    }
    const returnedAt = outcome.result === undefined ? -1 : attempts.length - 1;
    const recorded = attempts.map((attempt, index) => ({ ...attempt, sameEpoch: index === returnedAt }));
    return { name, options: options ?? null, attempts: recorded, outcome, timerDelays: [...clock.delays] };
  }));
}

const start = (store: any) => store.start('s', 'r1');
await epochCase('stable-active-turn', null, start, []);
await epochCase('stable-terminal-turn', null, (store) => {
  store.start('s', 'r1');
  store.retainTerminal('s', 'r1');
}, []);
await epochCase('stable-missing-turn', null, null, []);
await epochCase('terminal-during-read-retries', null, start, [
  (store) => store.retainTerminal('s', 'r1'),
]);
await epochCase('run-replacement-during-read-retries', null, start, [
  (store) => store.start('s', 'r2'),
]);
await epochCase('same-run-restart-keeps-epoch', null, start, [
  (store) => store.start('s', 'r1'),
]);
await epochCase('truncation-does-not-change-epoch', { eventLimit: 2 }, (store) => {
  store.start('s', 'r1');
  store.record('s', { type: 'e0' });
  store.record('s', { type: 'e1' });
}, [
  (store) => {
    store.record('s', { type: 'e2' });
    store.record('s', { type: 'e3' });
  },
]);
await epochCase('mismatched-retain-keeps-epoch', null, start, [
  (store) => store.retainTerminal('s', 'other'),
]);
await epochCase('terminal-restart-same-run-changes-epoch', null, (store) => {
  store.start('s', 'r1');
  store.retainTerminal('s', 'r1');
}, [
  (store) => store.start('s', 'r1'),
]);
await epochCase('dispose-during-read-retries', null, start, [
  (_store, coordinator) => coordinator.dispose(),
]);
await epochCase('terminal-expiry-during-read-retries', null, (store) => {
  store.start('s', 'r1');
  store.retainTerminal('s', 'r1');
}, [
  (_store, _coordinator, clock) => clock.advance(30_000),
]);
await epochCase('clear-terminal-during-read-retries', null, (store) => {
  store.start('s', 'r1');
  store.retainTerminal('s', 'r1');
}, [
  (store) => store.clearTerminal('s'),
]);
await epochCase('three-changing-attempts-throw', null, start, [
  (store) => store.start('s', 'r2'),
  (store) => store.start('s', 'r3'),
  (store) => store.start('s', 'r4'),
]);

assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({
  sourceCommit,
  sourceSha256,
  storeCases,
  coordinatorCases,
  epochCases,
}, null, 2) + '\n');
console.log(`recorded ${storeCases.length} store, ${coordinatorCases.length} coordinator, ${epochCases.length} epoch cases from real TS`);
